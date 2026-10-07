# Probes and a central instance

Status: **stage 1 built and verified on Windows** (sealed segments, file-based transfer, import into per-source replicas).
Stages 2-4 are design only. Written 2026-10-06 against `82014a0`.

rsRewind stays a self-contained recorder. Distribution adds one idea on top of it: a recorder can
**seal closed history into a segment file**, and any rsRewind can **import segment files** from
other recorders. A *probe* is just an ordinary recorder whose operator also runs `rsrewind export`.
A *central instance* is an ordinary install that has run `rsrewind import`. There is no second
product, no mode switch, and no code path where standalone use depends on distributed machinery.

```
Standalone     capture -> store -> OCR -> query -> UI              (unchanged)

Probe          capture -> store -> OCR            -> export -> outbox/*.rsseg
                         (same recorder)                          |  any file mover you trust
Central                                           import <--------'  (rsync, SMB, Syncthing, scp,
                                                    |                 over LAN / WireGuard)
                                       sources/<id>/ one store per probe
                                                    |
                                                 query -> UI
```

## 1. What the repository already gives us

The architecture already had most of the seams; the main surprise is that the existing network
rule points at a *simpler* design than the brief assumed.

**Compatible as-is**

- **Events are the unit of record, screenshots are evidence** (ARCHITECTURE "Event model"). A
  segment is a closed slice of events plus their evidence. Nothing about the model is
  screen-capture-specific, so new platforms and evidence kinds fit.
- **`EventKind` is a stable string.** Segments carry the string, so a receiver on an older build
  skips an unknown kind instead of rejecting the segment.
- **Relative, forward-slash media paths and `DataDir`.** A store is a self-contained folder. That
  is what lets a central instance hold one complete store per probe with no schema change.
- **Privacy is decided before persistence** (CLAUDE.md). So whatever a probe stores is already
  filtered; export cannot leak what was never recorded, and the central side never has to guess
  what a probe's rules were.
- **Deletion fences** (`deletion_fences`, `StorageError::Fenced`). Built to stop queued writes
  resurrecting forgotten history; they also stop a late or duplicate *delivery* doing the same.
- **File-before-row ordering, AUTOINCREMENT ids, backup-before-migrate, schema-too-new refusal.**
  Import reuses all of it, because import goes through the same inserts as the recorder.
- **Read-only query layer with typed results.** The UI boundary is already where we want it.
- **`PrivacyPolicy`, `ocr_status` backlog in the database.** OCR placement becomes a per-state
  fact (`pending`/`done`/`failed`/`skipped`) rather than a deployment mode.

**Friction**

- **`CLAUDE.md` forbids any network client or listening socket.** A probe that *pushes* over TCP
  needs that rule amended. Sealed files do not: moving a file is not rsRewind's business. This is
  why stage 1 contains no networking.
- **Row ids are local and `monitors.device_name` is globally UNIQUE.** `\\.\DISPLAY1` exists on
  every Windows probe, so a single merged database would conflate endpoints unless the schema
  gained source scoping everywhere. One store per source avoids needing it.
- **Observations are extended in place.** An event is not final until it stops growing, so sealing
  must wait for quiet (see "Sealing" below).
- **`MonitorInfo`, `ApplicationContext` and `WindowContext` read as Windows concepts** (`.exe`
  process names, `\\.\DISPLAYn`). They are tolerated as data today; they must stay *optional
  metadata* in the model. Segments therefore carry a `capabilities` list and the receiver never
  infers "nothing was happening" from absent metadata.
- **`rsrewind-query` depends on `rsrewind-storage`** (for `SCHEMA_VERSION` and
  `resolve_media_path`), which contradicts CLAUDE.md's "UI must not depend on storage, even
  transitively". Already recorded in `docs/remediation-status.md`; it must be fixed before a real
  UI crate lands, and a federated query layer makes it more pressing.

## 2. Options considered

| | **A. Sealed segments into per-source stores** *(chosen)* | B. Segments into one merged store | C. Live stream / remote capture | D. Replicate the database |
|---|---|---|---|---|
| Local durability | The probe's normal store; a network fault cannot touch it | same | Needs a local spool anyway, i.e. A plus a fragile stream | Same store, but shipping WAL/pages couples probe uptime to the link |
| Transfer unit | One immutable, hash-verified file | same | Frames over a connection | Raw SQLite pages |
| Idempotency | `(source, seq)` + content hash; re-delivery is a no-op | same | Needs its own dedup protocol | Page-level; no semantic dedup |
| Endpoint identity | Random id per install; one store per id | `source_id` column on every table + schema migration + every query | Session/connection identity | Whole-DB identity |
| Cross-endpoint attribution errors | Impossible by construction (separate stores, identity check on import) | Possible through any missed `WHERE source_id` | n/a | Possible on restore/merge |
| Deletion | Central forget = `rm`/`delete_range` + fence; local forget does not propagate (see 3.6) | Prune shared apps/windows per source | Unclear | Cannot delete one source's rows from a shared stream |
| Storage efficiency | Per-frame WebP, as today | slightly better (shared apps/windows) | n/a | Ships index pages and FTS shadow tables |
| Operational simplicity | Two commands, plain files | Same commands, much harder migration | A server, a listener, back-pressure | Tooling coupled to SQLite internals |
| Cross-platform | Format has no OS types; capabilities list | same | Per-OS stream quirks | SQLite is portable, schema drift is not |
| Cost of query side | Federated read facade (stage 2) | None | None | None |
| Violates a hard rule today | No | No | **Yes** (network client, listener) | Possibly (transport) |

**Why not B.** It is the obvious design and its failure mode is the one the brief warns about:
one forgotten filter and Probe A's screenshots appear on Probe B's timeline. It also needs a schema
migration on a *released* schema (older binaries then refuse to open the file) and source scoping
in every query. A's cost is a fan-out in the query layer, in one place, with an obvious test.

**Why not C.** The brief rules it out (this is replication, not remote desktop), and it would need
a local spool to be reliable anyway: A with extra steps.

**Why not D.** It couples the probe's recorder to the link, replicates indexes instead of facts,
shares row ids across machines, and cannot honour per-source deletion.

## 3. The design

### 3.1 Roles

- **Origin store**: written by a local recorder. Has a `source.id` (random 128-bit, 32 lowercase
  hex, minted on first need, kept in `settings`). Can export.
- **Replica store**: holds exactly one remote source, lives at `<data>/sources/<id>/`, is written
  only by `import`, and refuses a segment whose source id differs from its own. It is a normal
  store: `recent`, `search`, `forget`, retention and `doctor` work on it unchanged
  (`rsrewind --data-dir <data>/sources/<id> search ...`).
- A central instance may *also* record locally; its own history is simply another origin store in
  the data root. Standalone users never see `sources/`.

### 3.2 Segment

One file, `seg-<id8>-<seq>.rsseg` (format in `crates/rsrewind-segment`): magic, JSON manifest,
WebP images, SHA-256 trailer. The manifest contains events, sessions, monitors, applications,
windows, visual states (with OCR text and boxes if the probe had them) and the source's
`capabilities`. Cross-references are indexes *within the segment*: no database ids, no absolute
paths. It is self-contained, so importing needs nothing but the file and the receiver's identity
check.

Why a custom container instead of tar/zip: three fixed regions, a mandatory trailing digest, and
bounded reads are the whole spec, it avoids a dependency, and the digest is part of the format
instead of a convention.

### 3.3 Sealing (exactly-once, without blocking)

- Selection is by `ended_at`, not id: `ended_at <= now - settle AND (ended_at > last_cut OR id >
  last_max_id)`. A screen that has not changed for an hour keeps extending one observation; it
  must not hold back everything recorded after it, and it is sealed once, when it finally
  settles. The second clause catches rows stamped before the cut by a clock that stepped backwards.
- `settle` is at least 60 s (the recorder only extends across a few capture ticks) and defaults to
  5 min, which also covers OCR finishing. A sealed event can no longer change.
- A big backlog becomes several segments (`max_events`); events sharing an end instant are never
  split across a boundary.
- Crash safety: publish the file (atomic, never replaces), *then* advance the watermark. If the
  process dies between, the next export finds the file in the outbox, advances from the cursor in
  its manifest, and never reuses a sequence number. A corrupt outbox file stops export loudly.
- History deleted locally before it settles is never exported. History retention removes before
  it settles is lost to the central instance; `doctor` should eventually warn when retention is
  closer than the export lag.

### 3.4 Import (idempotent, validating, atomic)

1. Verify the digest (a truncated or corrupt file never opens) and validate every index in the
   manifest.
2. Identity: `adopt_replica_identity`; a mismatch is `SourceMismatch`, nothing written.
3. `(source, seq)` already imported with the same hash: `AlreadyImported`, no change. With a
   *different* hash: `SegmentConflict`, no change (cloned/restored identity, or tampering).
4. Images are validated cheaply (still WebP, declared dimensions) and written file-before-row;
   an identical existing file is adopted (re-import after a crash), a different one refuses. An
   invalid image drops that state and the events pointing at it, and is counted, not fatal.
5. All rows land in one transaction; content inside a deletion fence is skipped and counted.
6. The import record (`settings` key `import.seq.<n>` = hash) commits in the same transaction.

No migration is needed: identity, export watermark and import records live in the existing
`settings` table. (A dedicated table can replace them when a transport needs fast "which segments
do you have" answers; that is a normal additive migration then.)

### 3.5 Endpoint identity and reinstall

The id identifies an *installation's data directory*, nothing else. The label (hostname) is
display only. Consequences, all deliberately conservative:

- Reinstall that keeps the data folder: same identity, export resumes at the next sequence.
- Reinstall that loses the data folder: a **new** source appears on the central instance. History
  is not merged or overwritten. If the operator wants one lane, they can relabel; merging is an
  explicit future action, not an accident.
- A restored backup or cloned disk reuses an id with a stale watermark, so it will reseal old
  sequence numbers with different content. Import reports `SegmentConflict` instead of silently
  forking history.

### 3.6 Deletion

- **Central forget** (`delete_range` on the replica) writes a fence; later or repeated deliveries
  of overlapping segments skip the fenced content. Deleting a whole probe is deleting
  `sources/<id>/` (and, for a replica that is no longer wanted, its segment files).
- **Local forget on a probe does not propagate.** Anything already sealed and delivered stays
  central. This is the honest default for "local deletion versus central deletion": a user who
  deletes a moment on a probe expects *that machine* to forget it. Propagating deletes silently
  would let any probe erase central history; not propagating means a user can be surprised that a
  copy exists. Both must be visible: `forget` on a probe says how much of the range was
  already exported, and a signed tombstone segment is the clean way to opt in later.
- Sealed files in an outbox are history too. `forget` on a probe counts how many of the deleted
  moments were already sealed, prints that copies moved elsewhere are out of reach, and deletes
  outbox segments overlapping the range whole (a segment is immutable; a never-delivered one is
  simply lost, which is the point). Files that do not verify are reported, never deleted.

### 3.7 OCR placement

| | At the probe | At the central instance | Both / either |
|---|---|---|---|
| Cost | Probe CPU (below-normal priority already) | Central CPU, scales with probes | Complexity |
| Segment size | +text, negligible | smaller, but pixels must arrive first | |
| Availability | Needs a probe OCR engine (Windows today) | Needs a central OCR engine | |
| Privacy | Text travels with pixels either way | same | |
| Failure isolation | A probe without OCR still records | Backlog accumulates centrally | |

Choose **per state, not per deployment**. The recorder OCRs when it can; the segment carries
`done` text/boxes, or `pending` ("no usable text yet"), `failed` or `skipped`. A central instance
with an engine processes imported `pending` rows through its existing database-backed backlog; one
without simply leaves them unsearchable. Probes on platforms without OCR therefore work with no
special case, and nothing assumes both sides run the same engine.

### 3.8 What travels

Pixels (WebP, as stored), event metadata, window/process names, OCR text and boxes. Not sent:
database ids, absolute paths, the probe's config, or its privacy rules. Window titles and OCR text
are exactly as sensitive as the screenshots they describe; treat segments, the outbox and the
central store as equally sensitive (see `PRIVACY.md`).

### 3.9 Threat model

In scope (trusted private network, honest-but-fallible operators): corruption, truncation,
duplicates, replay, reordering, interrupted transfers, clone/restore identity mishaps, and
accidental cross-attribution.

| Concern | Today |
|---|---|
| Truncated / corrupted segment | Digest + length checks; never opens |
| Duplicate / replayed delivery | `(source, seq)` + hash; no-op |
| Reordered delivery | Accepted in any order; each segment is self-contained |
| Conflicting content for a known `(source, seq)` | Refused, nothing changed |
| Wrong-source segment into a replica | Refused by identity check |
| Hostile source id as a path | Must be exactly 32 lowercase hex or refused |
| Malformed images / references | Validated; bad state dropped and counted |
| Forgotten history re-delivered | Fenced |
| Terminal escape sequences in a remote window title or label, shown by `search`/`sources` | Control characters are replaced before the CLI prints them (`render::plain`); the UI draws text as glyphs and is not exposed |
| **A party who can write files impersonating a probe** | **Not defended.** The digest is integrity, not authentication |

Authentication is a deliberate stage-3 item: a per-probe enrolment secret exchanged out of band,
with each segment's manifest MAC'd and the central instance holding an allow-list of source ids.
It is a small addition (one key per probe, one HMAC), not a PKI. Until then, the central
`sources/` tree and the folder segments arrive in must be writable only by people you would trust
with the history itself. No control plane, discovery, relay or NAT traversal is planned.

## 4. Query boundary and UI

The UI keeps depending only on `rsrewind-core` + `rsrewind-query`. For the combined views, add a
read-only **history facade** in `rsrewind-query` (stage 2): it opens the local store plus every
`sources/<id>/` replica read-only and answers the high-level questions - `at(ts)`,
`recent(source?)`, `search(text, source?)`, `visual_detail(source, state)`, `sources()` - merging
by time (timeline) or rank (search). Types gain a `SourceId` field (`Option`/local = `None`);
cursors become `(started_at, source, event_id)`. A UI never learns about `.rsseg`, `settings`, or
that sources are separate databases, so this can later become a different backend without UI
changes. Fix the `query -> storage` dependency first (move `SCHEMA_VERSION` and media-path
resolution into core).

The UI technology is decided (2026-10-06): Iced chrome plus a custom `wgpu` viewport, built on the
facade (`rsrewind_query::History`). Either way the 2.5D viewport only needs: a time range of `TimelineEntry` per source,
decoded `BgraFrame`s for visible entries, and `at(ts)` for focus. A GPU texture cache is a UI
concern with no storage knowledge.

## 5. Platform capabilities

Segments declare `capabilities` (`ocr`, `window_titles`, `process_names`, `multi_monitor`, ...).
Today they are strings so a macOS (ScreenCaptureKit) or Linux (portal/PipeWire) probe can declare
fewer without a format change. The domain structs keep their Windows-flavoured field names for
now; the rule to hold is that nothing in storage, query or replication may *require* a process
name or window title. Capture backends stay behind `rsrewind-capture`. No macOS/Linux capture work
is proposed here.

## 6. Staged plan

| Stage | Deliverable | State |
|---|---|---|
| **1** | Segment format; export (settle, exactly-once, crash-safe); import (idempotent, validating, fenced, per-source replicas); `export`/`import`/`sources` CLI; end-to-end tests through the unchanged query API | **Built** (Linux-verified: core, segment, storage, query, cli) |
| 1b | `doctor` replication check (outbox size, unverifiable files, retention outrunning export); `forget` reports sealed moments and clears overlapping outbox segments | **Built** (Linux-verified) |
| 1c | Windows run of the whole workspace; real-recorder end to end; adversarial review of the import path | Windows runs **done 2026-10-06** on a physical Windows 11 box (fmt, build, test, clippy `-D warnings` all green; real recorder -> export -> import -> `search` finds the OCR text, duplicate import is a no-op). **Review not done** |
| 2 | History facade in `rsrewind-query` (combined timeline/search, per-source filter); fix `query -> storage` dependency; `rsrewind export --watch`/scheduled task wrapper for probes | Facade and dependency fix **built 2026-10-06** (Linux-tested, used by the UI); `export --watch` not started |
| 3 | Authentication (per-probe secret, MAC'd manifest, allow-list); **optional** push transport: probe-initiated, `HAVE`/`PUT`/`ACK` over TCP on a private network, central `rsrewind serve` listener. Needs a CLAUDE.md amendment (see 7); only worth building if the file-mover approach proves too awkward | Design only |
| 4 | Tombstone segments (opt-in deletion propagation), source merge/rename, outbox retention by receipt, GPU timeline viewer | Candidates |

## 7. Decisions for the maintainer

1. **Network rule.** Stage 1 needs no amendment. If stage 3's built-in transport is wanted, the
   hard rules need a narrowly worded exception (explicit opt-in subcommand, probe-initiated
   connections only, central listener never started by the recorder, nothing in the recording
   path). Recommend deferring until the file-based workflow has been used in anger.
2. **Local-forget semantics** (3.6): recommend "does not propagate, but is reported" for now.
3. **Reviewer tier.** The import path writes files and rows from untrusted input, i.e.
   security-sensitive under STD-001. This work has had tests and mutation checks but **no Opus or
   Astra pass**; it needs one before release.
