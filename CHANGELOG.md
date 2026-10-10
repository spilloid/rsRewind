# Changelog

All notable changes to this project are documented in this file. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project intends to follow
[Semantic Versioning](https://semver.org/) once it has a first release.

## [Unreleased]

### Added

- **Tray icon (Linux and Windows).** `rsrewind tray` shows rsRewind in the notification area (Windows;
  KDE Plasma natively,
  other desktops through their tray applet) with the rsRewind icon: red while recording, green while
  not, plus a badge (pause bars, a stop square, an amber dot when privacy rules are not enforced or the
  recorder is in trouble), so state never depends on colour alone. The menu pauses (15 minutes, 1 hour, until
  resumed) and resumes, forgets the last 10 minutes or hour (a second click within 10 seconds confirms;
  one click deletes nothing), opens the window, and starts or stops the recorder. Left click opens the
  window. It is its own process and acts only by running `rsrewind` commands, so quitting or crashing
  it never touches recording. `rsrewind tray --autostart on` starts the icon and the recorder at login
  (`off` undoes it): an XDG autostart entry on Linux, the per-user `Run` key on Windows. macOS is next.

### Changed

- Releases are normal GitHub releases. v0.0.2, v0.0.3 and v0.0.4 were first published as
  pre-releases and were made normal releases on 2026-10-08, each with a notice at the top of its
  notes listing the open privacy issues ([#12](https://github.com/spilloid/rsRewind/issues/12)). The release workflow still publishes a
  pre-release first and it is promoted once its files are verified.

### Added

- **Gaps are explained.** `rsrewind gaps [--since 24h] [--until] [--min-seconds 60] [--json]` lists every
  stretch with no recorded picture and why: recorder off, recorder stopped unexpectedly, paused, idle or
  locked, or nothing stored while recording (a privacy rule or a capture problem; privacy skips are not
  written as events yet, so the two are not told apart). `rsrewind recent` prints a line where time
  jumps. A screen that did not change is never a gap. Works for imported machines too, up to the last
  thing each one sent. Reasons come from what the recorder already writes; no database change.
  A recorder that died without restarting is recognised from its stale heartbeat.
- **Gaps in the rewind window.** The strip under the room shows each gap as a faint band (rose when
  something went wrong: the recorder died, or nothing was stored while recording). When the camera
  sits inside a gap, a note under the time says how long and why; stepping with ← / → across a gap
  says what was skipped. The window looks up gaps over the last 30 days.

### Known issues

- The first time the window opens on a large real history, thumbnails can stay blank for 20 seconds
  or more while the decoders work; later openings were quick. Seen twice on Linux, cause not
  established.

## [0.0.4] - 2026-10-08

**Fourth release: Linux.** rsRewind now records on KDE Plasma (Wayland) as well as Windows, the
rewind window runs on Linux, and `search` / `recent` read every machine's history together. Linux
gets a plain tarball next to the signed Windows files.

> **Warning.** The recorder-side privacy gaps from the first adversarial review are still open on
> every platform, nothing is encrypted at rest, and the Linux recorder has run on one machine. Use
> it only on machines you own and sessions you are comfortable recording.

### Added


- **Recording on KDE Plasma (Wayland).** `rsrewind daemon` / `start` / `stop` work on Linux under
  Plasma 6: screenshots from KWin (native resolution), the visible-window list and focus from a
  short KWin script (so the any-visible-window privacy rule is enforced, as on Windows), idle time
  from the compositor, and a locked screen counts as idle. On first start the recorder installs
  `~/.local/share/applications/rsrewind-recorder.desktop`, which is how KWin decides which program
  may take screenshots. There is no Linux OCR engine yet: moments are stored with text recognition
  `pending` and become searchable once one exists.
- `rsrewind ui` runs on Linux (it was already portable; the command refused).
- `rsrewind search` and `recent` read this machine and every imported machine together, and name the
  machine a result came from. A data folder that holds only imported machines now works.
- Linux names in the suggested privacy exclusions (KeePassXC, 1Password, Bitwarden, the KDE polkit
  prompt, pinentry, the KDE secret service). Existing `config.toml` files keep their own lists.
- Default data folder outside Windows: `$XDG_DATA_HOME/rsRewind` (usually
  `~/.local/share/rsRewind`), `~/Library/Application Support/rsRewind` on macOS.

### Changed

- The whole workspace builds on Linux (the OCR crate is now empty outside Windows), and CI tests
  Linux on every pull request.
- Release assets include `rsrewind-vX.Y.Z-linux-x86_64.tar.gz` (not code-signed; checked by
  `SHA256SUMS.txt`).

### Known issues

- **Recorder privacy gaps from the first adversarial review are still open**, listed with workarounds
  in [#12](https://github.com/spilloid/rsRewind/issues/12) (Unit B in `docs/remediation-status.md`). Not suitable for sensitive
  desktops.
- **No encryption at rest**, on any platform.
- **No text recognition on Linux yet.** Linux moments are stored with OCR `pending`: they appear in
  `recent` and the window but not in `search` until a Linux engine lands (or the history is imported
  by a machine that has one).
- The Linux recorder needs **KDE Plasma 6 on Wayland**. Other desktops refuse to record. It was tested
  on one machine (one screen, 135 % scale); several screens and other scales are unverified. It does
  not start at login yet; run `rsrewind start` after logging in.
- On Linux, KWin still takes the screenshot on a tick that a privacy rule then excludes; the picture
  is dropped from memory and never stored.
- On Linux the recorder uses about 15-30 % of one CPU core while the screen changes, mostly comparing
  and encoding full-resolution frames.
- Gaps in the timeline (recorder off, paused, idle, locked) are not shown as gaps yet: the window and
  `recent` simply jump to the next moment.
- Imported segments are checked for damage, not authorship; the import path has had no independent
  security review. Keep the folders segments pass through as private as the history.
- Everything listed for 0.0.3 that is not mentioned above still applies (Windows 10 unverified,
  `doctor` over SSH, `start` piped through another program).

### Upgrade notes

- Windows: no database migration and no required settings change.
- Linux: new. Unpack the tarball, put `rsrewind` somewhere permanent (for example `~/.local/bin`), and
  run `rsrewind start`. The first start writes
  `~/.local/share/applications/rsrewind-recorder.desktop`, which KWin needs before it lets the
  recorder take screenshots. If you move the binary, start it again and the entry is rewritten.
  Delete that file to withdraw the permission.
- The new Linux names in the suggested privacy exclusions apply only to a new `config.toml`; add them
  to an existing one yourself if you want them.

### Security

- **The data folder is private to its owner on Linux and macOS** (`0700`, re-applied every time the
  recorder starts). Before this, a Linux data folder was created with the default mode, so on a system
  whose home folders are readable by other accounts, so was the history.
- The KWin window list is accepted only from KWin's own bus name, and only for the request that asked
  for it; anything else counts as "windows unknown" and nothing is stored for that tick.

## [0.0.3] - 2026-10-08

**Third release: look closer.** The rewind window gets a full-window image viewer, and the
recorder is restructured so other platforms can follow. Everything in 0.0.1 and 0.0.2 still
applies, including their warning: recorder-side privacy gaps from the adversarial review are **not
fixed yet**, so this is still not ready for sensitive desktops. Importing history from other machines
still has had no independent security review.

### Added

- **Image viewer.** Double-click a card in the room, or the picture in the detail pane, to open the
  screenshot full window. It fits the window by default; the wheel zooms toward the pointer, dragging
  pans, double-click or `1` / `0` toggles 100% and fit, `+` / `-` zoom, Esc or a click outside closes
  it, and ← / → step to the previous or next moment on the same machine. Words that match your search
  are outlined in gold. Full-size pictures decode off the UI thread and only one is held at a time.
- Ctrl+F or `/` focuses the search box. `rsrewind ui --appearance system|light|dark` chooses the
  look; the default follows the system.
- **Platform seam for the recorder.** The capture loop, persist thread and OCR thread are portable
  and reach the desktop only through traits. Windows behaviour, config, schema and CLI output are
  unchanged; Windows is still the only platform that records.
- **Capabilities are explicit.** Where a platform cannot list windows, the recorder refuses to start
  unless `privacy.unenforced_ok = true` is set (default `false`; the Windows recorder never needs
  it). `status`, `status --json` and `doctor` then report "privacy rules NOT enforced", and exported
  segments never claim window titles for such a session.
- `History::later` in `rsrewind-query`, the forward step in the cross-source order.
- Recorder tests that drive the real capture loop with fake clocks, desktops and OCR (privacy skip
  before persist, any-visible-window rule, pause ordering, idle, fail-closed start, non-blocking
  capture on a full queue, shutdown order). They run on Linux and Windows.
- The `synth_history` example now draws convincing, entirely fictional desktops for demos and tests.
- A website at https://spilloid.github.io/rsRewind/ (install and verification, many-machine setup,
  privacy) and a documentation check that fails CI if the version differs between `Cargo.toml`, the
  changelog, the README and the site.

### Changed

- Moments newer than the cursor appear as small faint ghosts instead of covering the room.
- Transport buttons use plain ← → arrows (Windows drew the triangles as blue emoji tiles).
- The application name is not repeated when the window title already contains it; recognized text in
  the detail pane wraps to the pane.
- `rsrewind-capture`: `VisibleWindow` / `ScreenRect` moved to a portable module (same public paths).
- Release notes now include a download table with SHA-256 per file and verification commands.

### Known issues

- **Recorder privacy gaps from the first adversarial review are still open** (frame/privacy-check
  provenance, fail-closed handling of unreadable window information on Windows, pause
  acknowledgement, writer feedback). See `docs/remediation-status.md`, Unit B. Not suitable for
  sensitive desktops.
- **No encryption at rest.** Anyone who can read the data folder can read the history.
- **Imported segments are checked for damage, not for authorship**, and the import path has had no
  independent security review. Keep the folders segments pass through as private as the history.
- `rsrewind forget` cannot reach copies already moved to another machine; it reports how many of the
  deleted moments had already been sealed.
- Privacy skips are counted (`rsrewind status`) but not written to your history as events, although
  earlier documentation said they were.
- `rsrewind doctor` reports capture as failed when run from a non-interactive session (for example
  over SSH). Run it from your logged-in desktop.
- `rsrewind search` pointed at a data folder that holds only imported machines reports "no
  database"; point `--data-dir` at `sources\<id>`, or use `rsrewind ui`, which reads them all.
- `rsrewind start` hangs if its output is piped through another program (the background recorder
  inherits the pipe). Redirect to a file instead.
- The rewind window has been checked on Windows with synthetic history only. Touchpad pinch, more
  than one monitor, light mode on every setup and display scaling other than 100% and 125% are
  unverified; the strip under the room marks only the moments loaded around the cursor.
- Windows 10 is unverified. Linux and macOS recorders do not exist yet.
- With default settings nothing new is stored after five minutes without keyboard or mouse input
  (`capture.idle_after_secs`).

### Upgrade notes

- From 0.0.2: no database migration and no required settings change. `privacy.unenforced_ok` is a new
  optional key; leave it unset on Windows.
- `status` and `doctor` print the "privacy rules NOT enforced" line only when it applies, so output
  is unchanged for ordinary Windows use.

### Security

- Documentation corrected: PRIVACY.md and ARCHITECTURE.md claimed a `privacy_skip` event is written
  for each skip; the recorder only counts them.

## [0.0.2] - 2026-10-06

**Second release: history from other machines, and a window to browse it.** Everything in
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

### Known issues

- **Recorder privacy gaps from the first adversarial review are still open** (frame/privacy-check
  provenance, fail-closed handling of unreadable window information, pause acknowledgement, writer
  feedback). See `docs/remediation-status.md`, Unit B. Not suitable for sensitive desktops.
- **No encryption at rest.** Anyone who can read the data folder can read the history.
- **Imported segments are checked for damage, not for authorship**, and the import path has had no
  independent security review. Keep the folders segments pass through as private as the history.
- `rsrewind forget` cannot reach copies already moved to another machine; it reports how many of the
  deleted moments had already been sealed.
- `rsrewind doctor` reports capture as failed when run from a non-interactive session (for example
  over SSH). Run it from your logged-in desktop.
- The rewind window has been checked on Windows with synthetic history only. Cards newer than the
  cursor drift past the camera large and semi-transparent and can cover a lane label, and the strip
  under the room marks only the moments loaded around the cursor, not all of history.
- Windows 10, multiple-monitor recording on other hardware, light mode and display scaling other than
  125% are unverified. Linux and macOS recorders do not exist yet.
- With default settings nothing new is stored after five minutes without keyboard or mouse input
  (`capture.idle_after_secs`).

### Upgrade notes

- From 0.0.1: no database migration (schema stays at version 1) and no settings change is required.
  New folders appear in the data directory only when used: `outbox\` (after `rsrewind export`) and
  `sources\<id>\` (after `rsrewind import`).
- `rsrewind ui` is now a native window and no longer needs the Windows App Runtime.

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
