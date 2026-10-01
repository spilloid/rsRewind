# Roadmap

> **Note on the MVP bootstrap.** The first implementation push (branch `feat/mvp-bootstrap`,
> see `docs/mvp-contract.md`) deliberately pulled pieces of v0.2 (Memory), v0.3 (Reading) and
> v0.4 (Recall) forward into one vertical slice — capture, storage, OCR and search were specified
> and are being implemented together rather than strictly in the version order below, so that the
> first working version is a genuinely usable (if minimal) recorder rather than a sequence of
> non-functional stubs. The version numbers and acceptance criteria below describe the
> **product's** intended shape; they are not a literal claim that v0.1 ships before anything
> from v0.2 exists in the codebase.
>
> ### Status after bootstrap
>
> *Filled in by the orchestrator after reconciling this document against the actual state of
> each crate once the parallel implementation workstreams land. Do not treat the table below as
> current status — it is placeholder structure for that reconciliation.*
>
> | Area | Status | Notes |
> |---|---|---|
> | `rsrewind-core` (domain types, config, privacy rules, paths) | TODO | |
> | `rsrewind-capture` (monitors, change detection, foreground/privacy context) | TODO | |
> | `rsrewind-storage` (schema, migrations, backup-before-migrate, media I/O) | TODO | |
> | `rsrewind-ocr` (Windows.Media.Ocr wrapper) | TODO | |
> | `rsrewind-query` (safe FTS5 search, timeline) | TODO | |
> | `rsrewind-daemon` (threading model, control/status, retention) | TODO | |
> | `rsrewind-cli` (subcommands) | TODO | |
> | `rsrewind-ui` (WinUI 3 UI) | TODO | |
> | v0.1.0 acceptance criterion (8-hour session, no uncontrolled growth or catastrophic failure) | TODO — not yet run | |

## v0.1.0 — Eyes

The recorder can see the screen and not fall over.

- Per-monitor screen capture via Windows.Graphics.Capture, with change detection so only visually
  distinct frames are stored.
- Foreground process/window tracking attached to each observation.
- WebP screenshots written to the data directory; SQLite schema v1 recording sessions, monitors,
  applications, windows, visual states and observation events.
- `rsrewind start`/`stop`/`status`/`daemon` — the recorder runs as a background process that can
  be started, stopped, and inspected.
- Bounded capture → persist pipeline; capture never blocks on a slow writer.

**Acceptance criterion:** an 8-hour recording session completes without uncontrolled memory
growth and without catastrophic capture failure (the daemon does not crash, does not leak memory
without bound, and does not stop producing new visual states partway through).

## v0.2.0 — Memory

The recorder remembers reliably over time.

- Retention by age and by size (`apply_retention`), running on a schedule, oldest-first.
- `delete_range` / `rsrewind` delete-recent support: removing a time range removes its events,
  orphaned visual states, OCR blocks, FTS rows and backing image files together.
- Database backup-before-migration and schema-version refusal (never silently migrate data the
  binary doesn't understand, never run forward against a database from a newer version).
- `stats`/`integrity_check`/`find_orphans` — the building blocks `rsrewind doctor` reports from.

## v0.3.0 — Reading

The recorder can read what's on screen.

- `rsrewind-ocr`: Windows.Media.Ocr integration, language-pack detection and reporting.
- OCR backlog processing on a dedicated thread, backed by the database (`ocr_status`) rather than
  an in-memory queue, so a backlog survives a restart.
- OCR text and bounding boxes stored per visual state (`ocr_blocks`), FTS5 index maintained
  alongside.

## v0.4.0 — Recall

You can find things again.

- `rsrewind-query`: safe FTS5 query parsing (`parse_query`) and `search`/`recent`/`at` over a real
  fixture database.
- `rsrewind search`/`rsrewind recent` CLI commands, human and `--json` output.
- Time/app/title filtering combined with full-text search.

## v0.5.0 — Privacy

Recording respects what you don't want recorded.

- `PrivacyPolicy` evaluation wired into the capture pipeline: a monitor's frame is skipped
  whenever any visible window on it matches an excluded process or title pattern, checked
  *before* anything is persisted.
- Suggested default exclusions (password managers, private/incognito browser windows),
  user-configurable via `config.toml`.
- `rsrewind pause [--minutes N]` / `rsrewind resume`, with recorder state always visible via
  `rsrewind status` — no stealth mode, ever.
- Delete-recent exposed as a first-class CLI action, not just a storage-layer capability.

## v0.6.0 — Compression

Storage stays bounded and efficient at real-world retention windows.

- Benchmarking actual WebP storage cost per day of normal use against target retention windows
  and disk quotas.
- Evaluate whether per-frame WebP remains sufficient or whether a video-style encoding (only once
  benchmarks justify the added dependency surface — see `ARCHITECTURE.md`) is warranted for
  long-retention users.
- Disk quota enforcement (`storage.max_size_gb`) proven under sustained real usage, not just unit
  tests.

## v0.7.0 — Context

Richer context around each observation.

- Multi-monitor timeline correlation (what was on *every* screen at a given moment, not just one).
- Session/window-switch context (what you were doing immediately before and after a given
  moment).
- Groundwork for future evidence kinds beyond screenshots on the same event timeline (see
  `ARCHITECTURE.md`'s event model).

## v0.8.0 — Semantic recall

Search understands meaning, not just exact text.

- Local embedding-based semantic search layered on top of (not replacing) FTS5 exact search.
- Agent/LLM-facing CLI surface: the CLI's `--json` output and `rsrewind-core` query types are
  designed from the start to be a stable integration surface for scripts and agents (see
  `crates/rsrewind-core/src/search.rs`'s doc comment); this version is where that surface is
  exercised by real agent tooling sitting on top of rsRewind, strictly as a consumer of the CLI —
  never with elevated access to the raw database.

## v0.9.0 — Harden

Make it trustworthy enough to recommend.

- Full WinUI 3 desktop UI, reading only through `rsrewind-query`.
- `rsrewind doctor` covering the realistic failure modes found so far (missing OCR language pack,
  schema drift, orphaned media files, disk pressure).
- Signed binaries and a signed, tested MSI installer (see `docs/installer.md`,
  `.github/workflows/release.yml`).
- CI/CD release pipeline producing a verifiable, prerelease-then-promoted GitHub Release.
- Documented architecture and privacy model kept in sync with the shipped behavior (`ARCHITECTURE.md`,
  `PRIVACY.md`, per `docs/standards.md`'s STD-003 adoption).

## v1.0.0

Everything above, proven together:

- Reliable multi-monitor capture.
- Visual change detection, so storage cost tracks actual screen activity, not wall-clock time.
- OCR over captured screenshots.
- Full-text search, with time/app/title filtering.
- Timeline browsing of recorded history.
- A native WinUI 3 desktop UI.
- Privacy exclusions (process and title rules), checked before anything is persisted.
- Pause (indefinite and timed), always visible in `status`, no stealth mode.
- Delete-recent.
- Retention policy (by age) and disk quotas (by size).
- SQLite schema migrations, with backup-before-migration on any database that has real data at
  stake, and refusal to open a database from a newer schema version than the binary understands.
- Integrity diagnostics (`rsrewind doctor`): orphaned files, orphaned rows, OCR backlog health,
  missing language packs.
- A portable data directory (`%LOCALAPPDATA%\rsRewind`, relocatable via `RSREWIND_DATA_DIR`,
  relative media paths throughout).
- Signed binaries and a signed MSI installer.
- A CI/CD release pipeline that fails closed rather than ever publishing an unsigned release.
- Documented architecture (`ARCHITECTURE.md`) and privacy model (`PRIVACY.md`), kept current with
  the shipped product.
