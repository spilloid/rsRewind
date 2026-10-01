# Changelog

All notable changes to this project are documented in this file. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project intends to follow
[Semantic Versioning](https://semver.org/) once it has a first release.

## [Unreleased]

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
