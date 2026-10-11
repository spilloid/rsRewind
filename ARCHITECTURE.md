# Architecture

This is the durable description of what rsRewind is built as. `docs/mvp-contract.md` is the
orchestrator's contract written *before* the first vertical slice was implemented — this file is
meant to track the real, built system over time and will diverge from the contract as
implementation proceeds and is reviewed.

**Verification status (2026-10-06).** This file's earlier claim that only `rsrewind-core` exists
was stale. Actual state: `rsrewind-core`, `-storage`, `-query` and `-segment` are implemented and
tested (these are the crates that build and test on Linux; run `cargo test -p rsrewind-core -p
rsrewind-segment -p rsrewind-storage -p rsrewind-query -p rsrewind-cli`). `rsrewind-capture`,
`-ocr`, `-daemon` and the recorder half of `-cli` have real code that only builds on Windows; the
Astra-review remediation for them (Unit B in `docs/remediation-status.md`) is still open, so treat
recorder behaviour as unverified. `rsrewind-ui` is an Iced application with a custom wgpu rewind
viewport, reading through the `rsrewind-query` history facade (built 2026-10-06; see
[UI boundary](#ui-boundary)). Sections below say when something is design rather than built.

## Workspace and crate responsibilities

```
core  <- capture
core  <- ocr
core  <- segment          (pure file format; no SQLite)
core, segment  <- storage
core           <- query            (storage only as a dev-dependency, for test fixtures)
core, capture, storage, (ocr: Windows) <- daemon
core, segment, storage, query, daemon, ui  <- cli  (bin: rsrewind.exe)
core, query                           <- ui   (Iced 0.14 + wgpu; no Windows bindings)
```

- **`rsrewind-core`** — shared domain vocabulary with no SQLite, no Windows capture calls, and no
  UI dependency: `Config`, `Timestamp`, row-id newtypes (`EventId`, `VisualStateId`, …),
  `EventKind`, `MonitorInfo`, `ApplicationContext`/`WindowContext`/`FocusContext`, `BgraFrame`,
  `OcrBlock`, `CaptureState`, `PrivacyPolicy`/`PrivacyDecision`, `DataDir`/path layout, and the
  query-facing types (`SearchQuery`, `SearchHit`, `TimelineEntry`, `VisualDetail`, `SourceId`,
  `TimelineCursor`), the schema version both sides check (`SCHEMA_VERSION`) and the one stored
  media path resolver both sides use (`DataDir::resolve_media_checked`). Every other
  crate depends on this one so they agree on vocabulary without depending on each other's
  internals. Implemented and tested.
- **`rsrewind-capture`** — thin, typed wrappers around the Windows capture APIs (Windows.Graphics
  Capture, foreground window/process inspection, visible-window enumeration for privacy
  decisions, idle-time detection) plus a pure, Windows-free change-detection module. Not yet
  implemented.
- **`rsrewind-segment`** — the sealed history segment: manifest types, writer and verifying
  reader for the single-file `.rsseg` format used to replicate history between installations.
  Pure format, no SQLite, no network, no Windows. See [Replication](#replication-probes-and-a-central-instance).
- **`rsrewind-storage`** — owns the SQLite schema and all writes: sessions, monitors,
  applications, windows, visual states, events, OCR blocks and the FTS5 index, plus the
  control/status tables the CLI and daemon use to communicate. Also owns WebP encode/decode and
  exclusive file writes. Not yet implemented.
- **`rsrewind-ocr`** — wraps `Windows.Media.Ocr` to turn a captured frame into text blocks with
  bounding boxes. Not yet implemented.
- **`rsrewind-query`** — a read-only query layer over the same database `rsrewind-storage`
  writes: safe FTS5 query parsing, search, recent/timeline, and point-in-time lookups, plus the
  **history facade** (`History`) that answers the same questions across this machine's store and
  every imported replica. The only crate (besides `rsrewind-storage` itself) that touches SQL. It
  does not depend on `rsrewind-storage` (only its tests do), so the UI cannot reach storage
  through it.
- **`rsrewind-daemon`** — the recorder's orchestration: capture/persist/OCR threads, the
  bounded-channel pipeline, control-state polling, privacy enforcement, retention, and the
  single-instance/heartbeat/shutdown lifecycle. Portable behind the
  [platform seam](#platform-seam-and-capabilities); the Windows backends are wired in
  `windows_platform.rs`.
- **`rsrewind-cli`** — the `rsrewind` binary: argument parsing (`clap`) and dispatch to the
  daemon, storage, and query crates. Currently a placeholder `main.rs`.
- **`rsrewind-ui`** — the desktop UI: Iced for the application chrome and a custom wgpu viewport
  for the rewind room, reading only through `rsrewind-query`'s history facade. See
  [UI boundary](#ui-boundary).

## Process model

**One executable, `rsrewind.exe`, with subcommands** — not a daemon binary plus a separate CLI
binary plus a separate UI binary.

Why: a single binary means one thing to build, sign, package, and update; subcommands share
`rsrewind-core` types and a `clap` parser instead of duplicating argument handling across
binaries; and a user (or an agent) only ever needs to find one executable to do anything with
rsRewind. `rsrewind daemon` is the long-running recorder process; `rsrewind ui` is a separate
*process* (not a thread) that only reads through `rsrewind-query`; every other subcommand
(`search`, `recent`, `status`, `pause`, …) is a short-lived invocation that opens the database,
does one thing, and exits.

The window's front door ("Start recording", "Start rsRewind when I log in", the recording chip) works
the same way as the tray below: it runs `rsrewind start` / `tray` / `status --json` as child processes,
so the UI still cannot reach capture or storage.

`rsrewind tray` is a third kind of process: a notification-area icon that learns the recorder's
state from `rsrewind status --json` and acts only by running `rsrewind` subcommands (pause, resume,
forget, ui, start, stop). It depends on `rsrewind-core` alone (boundary-tested), opens no database
and captures nothing, so a crashed tray is as harmless as a crashed window.

Running the UI as its own process, started independently of the recorder, is deliberate: **the
UI crashing cannot stop recording**, because they are different OS processes with no shared
memory and no supervisory relationship — the daemon does not launch the UI and does not depend on
it being open, running, or even installed.

**Control and status flow through SQLite, not sockets or pipes.** The daemon reads a `control`
table (`state`, `paused_until`) once per capture tick to decide whether to record, and writes a
`recorder_status` table (`pid`, `heartbeat_at`, `state`, counters) roughly every 5 seconds. A
`rsrewind pause` invocation is just a short-lived process that writes one row and exits; it does
not need to find, connect to, or speak a protocol with the running daemon. This avoids needing a
named pipe or loopback socket (which would be exactly the kind of "local HTTP server" the project
explicitly avoids) and means the recorder's state is inspectable with any SQLite tool, not just
rsRewind's own code.

## Capture pipeline and threading model

No async runtime. The daemon is plain OS threads connected by bounded
`std::sync::mpsc::sync_channel`s:

```
capture thread  (tick = 1/fps) -- try_send --> persist thread (owns Store #1)
                                                  |  encode WebP, write file, insert rows
                                                  '-- try_send wake --> OCR thread (Store #2)
                                                                         pulls next_pending_ocr
```

Each capture tick: read control state (and whether a timed pause has expired), check idle time,
run the privacy check for each monitor, grab the latest frame per monitor, run change detection,
and either persist a new visual state or extend the current observation's time span.

**Capture never blocks.** The channel from the capture thread to the persist thread is bounded
and uses `try_send`; if the persist thread is behind (slow disk, OCR backlog, whatever), a full
queue causes the candidate frame to be dropped and a counter incremented, rather than the capture
thread stalling and missing the next tick or falling behind real time.

**The OCR backlog lives in the database, not in memory.** `visual_states.ocr_status = 'pending'`
rows are the queue; the OCR thread polls `next_pending_ocr` for the oldest pending rows. This
means the backlog survives a daemon restart (nothing is lost by stopping the service mid-OCR) and
memory stays bounded regardless of how far behind OCR falls — the cost of a backlog is disk, not
RAM, and OCR runs at below-normal thread priority so it does not compete with foreground work.

The persist thread and the OCR thread each own their own `Store` (their own SQLite connection);
WAL mode plus a 5-second busy timeout make two writers from one process safe. See
[Storage model](#storage-model).

Single-instance enforcement uses a named mutex (`Local\rsRewind.Recorder`) on Windows (the
`SingleInstance` trait of the [platform seam](#platform-seam-and-capabilities); another platform
needs an equivalent, e.g. a lock file held for the process lifetime) so a second `rsrewind
daemon`/`start` cannot run concurrently and corrupt or race on the same database.

## Platform seam and capabilities

The recorder loop, the persist thread and the OCR thread are portable code
(`rsrewind-daemon`: `recorder.rs`, `persist.rs`, `ocr_worker.rs`, `plan.rs`) and reach the desktop
only through the traits in `rsrewind-daemon/src/platform.rs`:

| Trait | Windows implementation (unchanged code, adapted in `windows_platform.rs`) |
|---|---|
| `ScreenContext` — monitors, foreground `FocusContext`, visible windows with rects, and a `Capabilities` value | `rsrewind-capture`: `monitors`, `foreground`, `visible_windows` (EnumWindows) |
| `CaptureBackend` / `FrameSource` — one newest-frame-only source per display | `rsrewind-capture::MonitorCapturer` (Windows.Graphics.Capture) |
| `IdleClock` — time since input, `None` = unknown | `rsrewind-capture::idle_millis` (`GetLastInputInfo`) |
| `OcrBackend` (created on the OCR thread by an `OcrFactory`) | `rsrewind-ocr::OcrEngine` (Windows.Media.Ocr), thread priority lowered first |
| `Lifecycle` / `SingleInstance` | `win::InstanceGuard`: `Local\rsRewind.Recorder` mutex, `rsrewind stop` event, console handler |
| `Clock` — wall, monotonic, sleep | `SystemClock` |

`run_with(options, Platform)` is the portable entry; `rsrewind_daemon::run` (Windows) builds the
Windows `Platform` and calls it with the startup order it always had. Other platforms have no
backends yet, so `rsrewind daemon` there still says the recorder is unsupported. Why traits in the
daemon rather than a new crate: the daemon is their only consumer, and implementing them next to
the recorder keeps `rsrewind-capture`/`rsrewind-ocr` free of a dependency on the daemon. The pure
window shapes (`VisibleWindow`, `ScreenRect`) and change detection in `rsrewind-capture` build on
every platform.

**Capabilities are explicit, never pretended equal.** `rsrewind_core::Capabilities` says whether
the platform can list windows (with rects), read titles, read process names, capture several
monitors, and recognize text. `Capabilities::privacy_gaps(policy)` names what the configured
rules need and the platform lacks (the window list always; titles or process names only when a
rule of that kind exists). Rules the recorder holds to:

- **Fail closed at start.** With any gap, the recorder refuses to start (before touching the data
  folder) unless `privacy.unenforced_ok = true` is set in `config.toml`.
- **Unknown means do not record, per tick.** A provider without a window list is never asked for
  one (an empty answer would read as "nothing excluded is visible") and an excluded focused
  window then skips every monitor; a failed enumeration skips every monitor; unknown idle time
  counts as idle. The Windows adapters never report these states (their degraded-information
  behaviour is remediation F4, still open).
- **Say so afterwards.** Each session's `SessionCapabilities` is stored in `settings`
  (`session.<id>.capabilities`; no schema change). `status` and `doctor` show "privacy rules NOT
  enforced" while the newest session ran with gaps, and export narrows the segment's
  `capabilities` to what every exported session had (`window_titles` only together with the
  window list). Where every rule is enforced (Windows), output is unchanged.

**Tests.** `rsrewind-daemon/src/tests/` drives this loop on every platform with deterministic
fakes (fake clock whose sleeps run scripted hooks between ticks, scripted frames and windows,
in-memory OCR): tick-level tests against a persist queue the test holds, and end-to-end runs of
`run_with` with the real persist and OCR threads over a temp SQLite store.

**KDE Plasma (Wayland) backend** (`rsrewind-daemon/src/linux_platform.rs`, since 0.0.4). Screens,
windows and focus come from a short KWin script loaded for one run per question; it reports a JSON
snapshot by calling a method the recorder exports on its own unique session-bus name (no well-known
name, no control or data over the bus). A report counts only if it comes from KWin's current unique
name and carries the nonce of the run that asked; otherwise, or after 2 s, the window list is unknown
and nothing is stored that tick. Screenshots come from KWin's `ScreenShot2.CaptureScreen` at native
resolution, on request (KWin only serves executables named in a `.desktop` file, which `daemon`
writes). Idle time is the compositor's `ext-idle-notify-v1`; a locked screen counts as idle.
Monitor and window geometry are both KWin's logical coordinates, so the any-visible-window
intersection compares like with like; frames are larger by the screen's scale. No OCR backend yet:
states stay `pending`. Single instance is an advisory lock on `<data>/recorder.lock`; stop is
SIGTERM to the heartbeat's pid. zbus and wayland-client are pure Rust; zbus runs its own small
executor thread for the bus, which is the one exception to "no async runtime" and stays inside that
crate.

## Event model

rsRewind's unit of record is an **event in time**, not a screenshot. An `observation` event says
"from T1 to T2, monitor M showed visual state V while process P / window W had focus." A
screenshot (a `visual_state` row plus its WebP file) is evidence attached to that event, not the
record itself — this is why an unchanged screen that holds steady for an hour costs one row with
an extended `ended_at`, not one row per capture tick.

This separation is deliberate for forward compatibility: future evidence kinds (an audio segment,
meeting context, a manual bookmark) can attach to the same timeline via new `EventKind` variants
without reshaping how time-range queries, retention, or deletion work. Other event kinds already
defined in `rsrewind-core` (`Paused`, `Resumed`, `PrivacySkip`, `IdleStart`/`IdleEnd`,
`RecorderStart`/`RecorderStop`) record recorder lifecycle and privacy decisions on the same
timeline. `PrivacySkip` is defined to record *that* capture was skipped and why, without storing any
pixels, but the recorder does not write it yet: skips are only counted (status quo as of 2026-10-08).

`EventKind` is stored as a stable lowercase string (`as_str`/`parse`), not an integer, so adding a
new kind later never means renumbering values already written to disk.

## Storage model

SQLite via `rusqlite` (bundled, FTS5 enabled). One `Store` is one connection; the daemon opens
two (persist thread, OCR thread). On open: `journal_mode=WAL`, `synchronous=NORMAL`,
`foreign_keys=ON`, `secure_delete=ON`, `busy_timeout=5000`.

Schema v1 (`applications`, `windows`, `visual_states` and `events` use `AUTOINCREMENT`, so ids are
never reused; timestamps are `INTEGER` Unix milliseconds UTC; media paths are stored relative to the
data root, forward slashes only):

```
sessions(id, started_at, ended_at NULL, hostname, app_version)
monitors(id, device_name UNIQUE, width, height, left, top, dpi, primary_monitor, last_seen_at)
applications(id, process_name UNIQUE COLLATE NOCASE, exe_path NULL, first_seen_at)
windows(id, application_id FK, title, class_name NULL, first_seen_at,
        UNIQUE(application_id, title, class_name))
deletion_fences(id, since, until, created_at)  -- written by delete_range; writers refuse
                                               -- states/observations inside a fence
visual_states(id, monitor_id FK, captured_at, media_path UNIQUE, width, height, byte_size,
              fingerprint INTEGER, ocr_status TEXT CHECK in (pending, done, failed, skipped),
              ocr_error NULL, ocr_engine NULL, ocr_ms NULL)
events(id, session_id FK, kind TEXT, started_at, ended_at, monitor_id NULL FK,
       visual_state_id NULL FK, application_id NULL FK, window_id NULL FK, metadata_json NULL)
ocr_blocks(id, visual_state_id FK ON DELETE CASCADE, line_index, text, x, y, width, height,
           confidence NULL)
ocr_fts  = fts5(text, tokenize='unicode61 remove_diacritics 2')  -- rowid = visual_states.id,
                                                                  -- fts5 secure-delete on
control(id INTEGER PRIMARY KEY CHECK(id=1), state TEXT, paused_until NULL, updated_at)
recorder_status(id CHECK(id=1), pid, started_at, heartbeat_at, state, counters_json)
settings(key PRIMARY KEY, value)
```

Indexes on every foreign key, plus `events(started_at)`, `events(kind, started_at)`,
`visual_states(captured_at)`, `visual_states(ocr_status)`.

**Media paths are relative, with forward slashes**, so the whole `%LOCALAPPDATA%\rsRewind`
folder can be moved, backed up, or restored on another drive letter without rewriting the
database. `DataDir::resolve_media` (in `rsrewind-core`) is the one place a stored relative path is
turned back into an absolute one, and it refuses anything that could escape the data root
(absolute paths, drive letters, `..`, leading slashes) — the database is treated as
user-editable, untrusted input for this purpose. A stored path must also start with `media/`, and
`DataDir::resolve_media_checked` (used by both storage and query) additionally rejects backslashes and
any existing component that is a symlink or junction (check-then-use: it stops tampered rows and
stray links, not a racing local attacker).

**File-before-row write ordering.** When a new visual state is captured, the WebP file is written
first (`write_webp_exclusive`: write to a temp file, then rename — never overwrite an existing
file), and only after that succeeds is the database row inserted. A crash between the two leaves
at worst an orphan file with no row pointing at it (reported by `find_orphans`; `delete_range` and
retention sweep orphans in their range by file name), never a
row pointing at a file that was never written. The reverse ordering would risk a row referencing
pixels that don't exist, which is a worse failure mode for a tool whose whole point is "the
screenshot that this row claims exists."

**Backup before migration.** Opening a database that has existing data and pending migrations
triggers a SQLite online-backup-API copy to `backups/recall-v{from}-{utc}.db` *before* the
migration runs (under an exclusive `backups/.migrate.lock`, with the backup name reserved by
`create_new`, so concurrent processes cannot overwrite each other's backup); if the backup fails, the migration is refused rather than attempted blind. Opening
a database whose recorded schema version is newer than the binary understands is also refused
outright, rather than guessing.

## OCR pipeline

`rsrewind-ocr` wraps `Windows.Media.Ocr`: `OcrEngine::TryCreateFromLanguage` or
`TryCreateFromUserProfileLanguages` to construct an engine (a typed error surfaces when no OCR
language pack is installed, which `rsrewind doctor` is meant to report), then
`SoftwareBitmap`-based recognition that respects `OcrEngine::MaxImageDimension` by downscaling the
frame and scaling bounding boxes back up afterward. Each `OcrLine` becomes one `OcrBlock` (the
union of its word rectangles) in reading order; confidence is not reported by this engine and is
left as `None` in the type for future engines that do report it. The blocking `.get()` call on the
async WinRT operation is intentional — it runs on the OCR thread, not an async executor, which is
consistent with the no-async-runtime design (see [Why no async runtime](#why-no-async-runtime)).

## Query architecture

`rsrewind-query` opens its own **read-only** connection (`SQLITE_OPEN_READ_ONLY`,
`query_only=ON`) — a query bug can read the wrong row, but it cannot write one, by construction
rather than by code review alone.

**No SQL in the UI or CLI, ever.** `rsrewind-query` is the only crate besides `rsrewind-storage`
itself that constructs SQL. The CLI and UI call typed functions (`search`, `recent`,
`visual_detail`, `at`) and get back `rsrewind-core` types.

**User search text is never spliced into SQL.** `parse_query` turns free text into a safe FTS5
`MATCH` expression: every bare token becomes a quoted phrase (with internal `"` doubled),
`"already quoted"` phrases are preserved, a trailing `*` keeps prefix-search semantics, and
anything that looks like FTS operator syntax in user input — `NEAR(`, a `column:` filter, bare
`AND`/`OR`/`NOT`, parentheses, `-` exclusion — is treated as literal text to search for, never as
an operator. This is the one place query injection would otherwise be possible (FTS5's MATCH
syntax is expressive enough to be a mini query language of its own), so it is unit-tested
specifically against injection-shaped inputs, not just happy-path queries. An empty or
whitespace-only search string means "no FTS filter," so `search` with only `--app`/`--since`
filters still works as a time/app browser rather than erroring.

Ranking is bm25 then recency; results are joined to the *earliest* observation event of each
visual state for timestamp/app/window/monitor display, with a highlighted snippet
(`snippet(ocr_fts, 0, '[', ']', '…', 12)`) and a default/max result limit (20/200) to keep a broad
query cheap.

## UI boundary

`rsrewind-ui` is a separate crate and a separate *process* from the recorder (see
[Process model](#process-model)): `rsrewind ui` starts `rsrewind ui --foreground` detached and
returns. It talks to history exclusively through `rsrewind-query`'s history facade, never issues
SQL of its own, and its dependency graph cannot reach capture, storage, OCR or the daemon
(`rsrewind-query/tests/ui_boundary.rs` walks `cargo metadata` for every platform and fails the
build otherwise). The daemon does not know the UI exists; the UI does not know how the daemon
captures anything. This means the UI can crash, be force-closed, or simply not be installed
without any effect on whether recording continues, and it means the UI crate can be rewritten
entirely without touching the recorder.

**History facade** (`rsrewind_query::History`, `docs/design/distributed.md` §4). Opens this
machine's store and every `sources/<id>/` replica read-only and answers `sources()`,
`recent(filter, limit, cursor)`, `at(ts, filter)`, `search(query, filter)`,
`visual_detail(source, id)` and, through `MediaReader`, the bytes or decoded pixels of a result's
picture. Results carry `source: Option<SourceId>` (`None` = this machine; omitted from `--json` when
`None`, so existing output is unchanged). The timeline order is `(started_at, source, event_id)`,
total across sources (event ids are only unique per store), and each per-store query gets the
projection of the global cursor, so paging never skips or repeats. A replica is used only if its
own `settings` say it is the replica of exactly the id its folder is named after; anything else is
skipped and reported, never attributed. Pictures are read only from the reporting source's own
`media/` tree, through the same resolver the writer uses, with size limits before decode.

**Threads.** One worker thread owns the facade (SQLite connections are not `Sync`); queued
questions of the same kind collapse to the newest (typing, scrubbing). Two decoder threads read
and shrink pictures through `MediaReader` (no database), newest request first, from a bounded
queue. Replies come back as futures awaited by Iced `Task`s, so the UI thread never blocks.

**Rewind viewport.** A custom `iced::widget::shader` widget. `timeline::model` is pure, unit-tested
math: a camera at a moment, depth logarithmic in elapsed time (the last minute is spread out, last
week compressed into the back), one lane per source, perspective scale, culling (too deep, passed,
off-screen, at most 180 cards, nearest kept), back-to-front order, hit testing and scrub/zoom.
`timeline::gpu` draws rounded, bordered quads with an SDF shader, textured with mip-mapped
thumbnails from a GPU texture LRU bounded at 128 MB (on-screen textures are never evicted); decoded
thumbnails are cached on the CPU in a separate 96 MB LRU. Search hits cue the room to the hit's
matching observation (not its first capture) with a 180 ms settle rather than a fly-through.

**Image viewer.** Double-clicking a card or the detail picture replaces the window's contents with
one screenshot over a dark backdrop (`viewer/`). As with the timeline, the view math (fit, zoom
toward the pointer, clamped pan, 100 %/fit toggle where 100 % is one image pixel per physical
pixel, OCR-box mapping, search-phrase matching for the gold outlines) is pure and unit-tested in
`viewer::model`; `viewer/mod.rs` is the shader widget and `app/viewer.rs` the application side.
The full-resolution picture is decoded on the frame pool, at most one is held, a superseded decode
is aborted (the decoder skips cancelled requests), and its texture uses its own `TexKey` so it
never displaces thumbnails and is freed in `Pipeline::trim` the frame it leaves the screen.
Stepping uses `History::later` / `recent` filtered to the picture's source, skipping repeated
observations of the same picture.

## Replication (probes and a central instance)

Full analysis, options considered and threat model: `docs/design/distributed.md`. In short:

- A **segment** is an immutable, SHA-256-verified file holding a closed slice of one source's
  history (events, the context they reference, WebP images, OCR text). It carries no database ids
  and no absolute paths. `rsrewind export` seals settled history into `outbox/`; `rsrewind import`
  merges segment files into a data directory.
- Every origin store has a random **source id** (in `settings`). A central instance keeps **one
  complete replica store per source** under `sources/<id>/`; a replica refuses segments from any
  other source, so cross-endpoint attribution cannot happen by construction. A replica is an
  ordinary store: `search`, `recent`, `forget`, retention and `doctor` work on it unchanged.
- Sealing selects by `ended_at` once an event has been quiet for at least 60 s, so an observation
  still being extended never leaks half-finished or blocks later history. Publishing a segment
  precedes advancing the export watermark, and a crash between the two is recovered from the file.
- Import is idempotent (`(source, seq)` + content hash), conflict-detecting, validating
  (images, references), fence-respecting (a central forget holds against re-delivery), and
  transactional. Image files are written before rows, as for the recorder.
- **There is no networking in rsRewind.** Moving segment files (rsync, a share, a sync tool,
  over LAN or WireGuard) is the operator's concern. Authentication of probes and an optional
  built-in transport are stage 3 and would need an explicit amendment of the network rule in
  `CLAUDE.md`.
- State lives in the existing `settings` table (`source.id`, `source.role`, `source.label`,
  `export.*`, `import.seq.<n>`); no schema migration was needed.

## Design rationale

### Why SQLite

One embedded, zero-admin, file-based database that is portable with the rest of the data
directory, supports a real query language (versus a bespoke binary log format), has a
battle-tested WAL mode for the two-writer (persist + OCR) case this project needs, and ships an
FTS5 full-text index good enough for personal-scale search without standing up a separate search
service. It is also inspectable by the user with any off-the-shelf SQLite tool, which matters for
a product whose core promise is "this is your data, on your disk, in the open."

### Why WebP first

Screenshots here are mostly text and UI chrome, which WebP's lossy mode compresses well at a
quality setting (80 by default) that stays legible for OCR and for a human looking at the
timeline. Starting with WebP keyframes rather than reaching for video encoding (FFmpeg,
H.264/AV1 inter-frame compression) keeps the dependency surface small and the per-frame storage
model simple: one file per visual state, independently readable, independently deletable.
**No FFmpeg until benchmarks justify it** — if disk usage at realistic retention windows turns
out to need real video compression, that is a deliberate, benchmarked future change, not a
default.

### Why Iced + wgpu (superseding WinUI 3 / windows-reactor)

Decided by the maintainer on 2026-10-06: Rust only, Iced for ordinary application chrome, and a
custom wgpu viewport for the rewind view, because the spatial timeline (screenshots as GPU textures
receding into depth) is the product's centre and needs a real GPU surface. Iced draws with wgpu
itself, so the viewport shares its device and render pass instead of compositing a second surface.
The UI uses no Windows bindings; the section below describes the previous plan and is kept for the
`windows-core` reasoning, which still applies to the recorder crates.

### (Previous plan) WinUI 3 / windows-reactor, and the two `windows-core` lines

The UI uses Microsoft's `windows-reactor` 0.100 (a `Component`/`View` framework over WinUI 3),
which requires the Windows App Runtime 2.4 framework package at runtime. The recorder crates
(`rsrewind-capture`, `rsrewind-core`, etc.) use the `windows` 0.62 crate, which is on an older
`windows-core` projection line than `windows-reactor`'s 0.100 line. These two lines are not
binary-compatible with each other at the type level.

**No `windows`-crate type crosses the `rsrewind-ui` boundary.** `rsrewind-ui` depends only on
`rsrewind-core` (plain Rust/serde types — `SearchHit`, `TimelineEntry`, `BgraFrame`'s *bytes*, not
Windows handle types) and `rsrewind-query`, never on `rsrewind-capture` or the `windows` 0.62
crate directly. This is what makes it possible for the UI to sit on a newer `windows-core` line
than the recorder without a version conflict: the two halves of the dependency graph never need to
agree on which `windows-core` they use, because no type ever needs to flow from one to the other.

### Why no async runtime

The daemon's concurrency is a small, fixed number of long-lived OS threads doing blocking I/O
(capture a frame, write a file, run a blocking WinRT OCR call, write a database row) connected by
bounded channels — not a large number of short-lived concurrent tasks waiting on I/O, which is
the problem async runtimes are built to solve efficiently. Plain threads and
`std::sync::mpsc::sync_channel` give the exact backpressure behavior this design needs
("capture never blocks, a full queue drops frames") without pulling in an executor, and keep every
thread's panic/shutdown behavior easy to reason about independently. If a future version needs
genuine high-fan-out concurrency (many short-lived I/O-bound tasks), that would be reconsidered
then, deliberately, rather than defaulted into now.

### Future DuckDB/dbt separation

Analytics over recorded history (usage trends, time-in-app rollups, etc.) are planned to flow
**SQLite → Parquet → DuckDB → dbt**, entirely downstream of and decoupled from the recording path.
The live `recall.db` that the daemon writes to is never queried directly by an analytics job;
instead, data is expected to be periodically exported to Parquet and analyzed from there. This
keeps analytics workloads (which can be arbitrarily expensive, long-running, or exploratory) from
ever competing with the daemon's own writer for database locks or disk I/O, and keeps the
recording path's schema free to evolve without needing to stay compatible with a downstream
warehouse's dbt models. Not yet implemented — recorded here as settled direction, not shipped
functionality.

## Dependency policy and notable dependencies

- **No network code of any kind.** No HTTP client, no listening socket, no telemetry library, in
  any crate. This is treated as an architectural invariant, not a current state that might
  change — see `CLAUDE.md`'s hard rules. Replication between installations is deliberately
  file-based for this reason (see [Replication](#replication-probes-and-a-central-instance)).
- Library crates (`rsrewind-core`, `rsrewind-capture`, `rsrewind-storage`, `rsrewind-ocr`,
  `rsrewind-query`) expose typed errors via `thiserror`; `anyhow` is reserved for the
  binary-level crates (`rsrewind-cli`, `rsrewind-daemon`) where a typed error no longer needs to
  be matched on by a caller.
- `unsafe` is permitted only inside Windows API wrapper functions, and every such block carries a
  `// SAFETY:` comment explaining the invariant that makes it sound (see `paths.rs`'s
  `local_app_data` for the current example: a `CoTaskMemFree`'d buffer that is copied out exactly
  once before being freed).
- Notable workspace dependencies (`Cargo.toml`): `rusqlite` (bundled SQLite with FTS5),
  `webp` (WebP encode/decode), `windows` 0.62 (Windows API bindings for the recorder crates —
  distinct from `windows-reactor`'s `windows-core` 0.100 line used only by `rsrewind-ui`),
  `chrono` (UTC timestamp handling), `clap` (CLI argument parsing), `tracing`/`tracing-subscriber`/
  `tracing-appender` (structured logging — see the logging rule in `CLAUDE.md`: OCR text and
  window titles are never logged at `info` or above), `thiserror`/`anyhow` (error handling, split
  as above), `serde`/`serde_json`/`toml` (config and the JSON CLI/query surface).
- Workspace lints (`Cargo.toml` `[workspace.lints]`) deny `unsafe_op_in_unsafe_fn` and warn on
  `clippy::unwrap_used`, `clippy::expect_used`, `clippy::dbg_macro`, and `clippy::todo` — a
  warning here is meant to be investigated, not routinely allowed through, in line with the
  "no `unwrap()`/`expect()` in runtime code" rule in `docs/mvp-contract.md`.

## macOS platform (2026-10-10)

Native Apple APIs are behind the same recorder platform seam; Swift bridges are compiled into static archives during the macOS Rust build and linked into the single executable. The menu bar remains a separate CLI-only process, and the UI keeps its read-only query boundary. Storage and per-monitor change detection are shared with Windows/Linux. Hosted ARM64 CI produces development artifacts; interactive permission and desktop acceptance are tracked in `docs/macos.md`.
