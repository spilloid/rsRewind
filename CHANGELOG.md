# Changelog

All notable changes to this project are documented in this file. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project intends to follow
[Semantic Versioning](https://semver.org/) once it has a first release.

## [Unreleased]

### Added

- **Full-window image viewer.** Double-click a card in the room, or the picture in the detail pane,
  to see that moment's screenshot at full resolution: fit to the window by default, scroll (or
  pinch) to zoom toward the pointer, drag to pan, double-click or `1` / `0` for 100 % / fit, `+` /
  `-`, `←` / `→` for the previous / next moment in the same source, Esc or a click on the backdrop to
  close. Words matching the current search are outlined in gold from the recognized-text boxes.
  Full-resolution pictures decode off the UI thread, one at a time (reduced to fit 4096 px), and
  their GPU textures are freed as soon as they leave the screen.
- `Ctrl+F` or `/` jumps to the search box.
- `rsrewind ui --appearance light|dark` forces an appearance (default `system`: follow Windows).
- `History::later` in `rsrewind-query`: observations after a cursor, oldest first (the forward
  counterpart of `recent`), in the same cross-source order.

## [0.0.2] - 2026-10-06

**Second pre-release: history from other machines, and a window to browse it.** Everything in
0.0.1 still applies, including its warning below: recorder-side privacy gaps from the adversarial
review are **not fixed yet**, so this is still not ready for sensitive desktops. The new import
path takes files from other machines and has had tests and mutation checks but **no independent
security review yet**. The new window has been checked on Windows with synthetic history only.

### Added

- **Desktop UI (`rsrewind ui`).** An Iced window with a custom wgpu rewind viewport: recorded
  screens as GPU-textured cards receding into depth (logarithmic in time), one lane per source,
  drag/wheel scrubbing, a filament of all history, keyboard stepping. Search results cue the room
  to the moment the text was on screen and open it in a detail pane with the recognized text.
  Runs in its own detached process (`--foreground` to stay attached) and reads history only through
  the new cross-source **history facade** in `rsrewind-query` (`History`: sources, recent, at,
  search, detail, media), which merges this machine with every imported replica.
- `cargo run -p rsrewind-query --example synth_history -- <folder>` builds throwaway synthetic
  history (this machine plus two probes) for trying the UI without recording a real screen.

- **Probe / central-instance replication, stage 1.** `rsrewind export` seals settled history into
  immutable, hash-verified `.rsseg` segment files; `rsrewind import` merges them into a data
  directory as one independent replica store per source (`sources/<id>/`); `rsrewind sources`
  lists them. Import is idempotent, detects conflicting content for a known `(source, seq)`,
  validates images and references, respects deletion fences and refuses cross-source history.
  Still no network code: moving files is up to the operator. New crate `rsrewind-segment`. No
  schema migration. Design and threat model: `docs/design/distributed.md`.

- `forget` deletes outbox segments covering the forgotten time and reports how many deleted
  moments were already sealed. `doctor` gains a replication check (outbox size, files that do not
  verify, retention about to outrun export).

### Verified

- Replication stage 1 builds, passes fmt/clippy/tests, and works end to end with the real recorder
  on Windows 11 (2026-10-06).

### Security

- Window titles, process names, source labels and recognized text are stripped of terminal
  control characters before the command line prints them, so a remote source cannot drive your
  terminal through them.

### Changed

- Documentation reconciled with the code (`ARCHITECTURE.md`, `CLAUDE.md` said most crates were
  unimplemented).

## [0.0.1] - 2026-10-01

**First pre-release: the first vertical slice, end to end.** rsRewind watches your screen,
keeps a searchable record of what changed, and lets you search and review it from the command
line — entirely on your own machine, with no cloud component and no network code.

> **Not ready for sensitive desktops.** An adversarial review of this slice found recorder-side
> privacy gaps that are *not fixed yet* (frame/privacy-check provenance, fail-closed handling of
> unreadable window info, pause acknowledgement, writer feedback). Use it on a machine and
> session you are comfortable recording. Details: `docs/reviews/2026-09-30-astra-ultra-mvp.md`
> and `docs/remediation-status.md`. There is no encryption at rest (see `PRIVACY.md`).

### What you can do

- **Record:** `rsrewind start` / `stop` / `status` / `pause [--minutes N]` / `resume`. Capture uses
  Windows.Graphics.Capture per monitor with change detection, so only meaningful changes are
  stored (as WebP). Recording state is always discoverable; there is no hidden mode.
- **Read the screen back:** Windows.Media.Ocr text recognition feeds a SQLite FTS5 index.
  `rsrewind search "text" [--app] [--title] [--since] [--until] [--limit] [--json]` and
  `rsrewind recent`.
- **Look after it:** `rsrewind forget <since> --yes` deletes a time range for real, `rsrewind
  doctor` checks health, `rsrewind data-dir` shows where everything lives
  (`%LOCALAPPDATA%\rsRewind`).
- **Privacy rules:** process and window-title exclusions are checked before anything is stored,
  and a monitor is skipped when any visible window on it matches.

### Deletion that actually deletes

`forget` writes a durable deletion fence (so frames queued while you forgot cannot reappear),
removes window titles and application names that nothing references any more, sweeps orphaned
screenshot files and stale temp files, zeroes deleted pages (SQLite `secure_delete` and FTS5
secure-delete) and truncates the write-ahead log. Pre-migration backups are never rewritten; it
tells you which exist. Size retention now counts the real disk footprint (database, WAL and every
media file).

### Also fixed in this release (from the adversarial review)

- Row ids are never reused, and OCR results are bound to the exact screenshot they were computed
  from, so text from a forgotten screenshot can never attach to a later one.
- Media paths must live under `media/` and may not cross symlinks or junctions.
- Concurrent schema migrations are serialised and cannot overwrite each other's backup.
- `at()`, timeline paging (a `(started_at, event_id)` cursor), search time ranges (any observation
  overlapping the range) and `visual_detail` (one read snapshot) now return correct results.

### Known gaps

- Recorder privacy hardening from the review is still open (see the warning above).
- The desktop viewer (`rsrewind ui`) is not built yet.
- Releases are not code-signed yet, so this pre-release is published only once signing is
  configured; the MSI installer is written but untested.
- No encryption at rest: data is protected only by Windows user-account permissions.

[Unreleased]: https://github.com/spilloid/rsRewind/compare/v0.0.1...HEAD
[0.0.1]: https://github.com/spilloid/rsRewind/releases/tag/v0.0.1
