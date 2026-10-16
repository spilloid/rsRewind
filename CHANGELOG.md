# Changelog

All notable changes to this project are documented in this file. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project intends to follow
[Semantic Versioning](https://semver.org/) once it has a first release.

## [Unreleased]

### Fixed

- **macOS: "Recording" but nothing ever saved (issue #33).** The window list was discarded whenever
  any on-screen window had no title, an unresolvable owner or an unusual layer, which every real Mac
  has, so every monitor was skipped on every tick; an unresolvable window owner also made idle time
  unknown. Odd windows are now kept (untitled: empty title; unresolvable owner: the owner name macOS
  reports; application rules are checked against both names). The list still counts as unknown,
  and nothing is stored, when title rules exist and no application window (other than rsRewind's
  own) reports a title, a window has no owner name while application rules exist, or a window's
  position cannot be read. Locked, switched-away and login-window sessions still record nothing, and
  so does a session where an unidentifiable window covers most of a display. **Privacy note:** a
  title rule cannot match a Mac window whose title macOS does not report, and Mac exports still
  carry the `window_titles` tag (an open maintainer decision); `status` and `doctor` say when this
  happened. See PRIVACY.md. **Unverified on a real Mac**: written and tested on Windows; the Swift
  and macOS-only code was not compiled before this entry was written.
- **Logs never carry a window title from a parse error.** The KWin snapshot parser no longer passes
  serde_json's message (which quotes rejected values) into its error, and the "nothing is being
  saved" warning logs only the condition's name and counts.

### Added

- **Saving health.** When the recorder runs but stores nothing, `rsrewind status` says so and why
  ("Nothing is being saved since 14:02:11: window context unavailable"), `status --json` adds an
  optional `not_saving` object (absent while saving), `doctor` adds a "saving" line with per-reason
  tick counts, and the window's chip and the tray headline say "Recording · nothing saved: …". The
  log gets one warning when it starts and one line when saving resumes. New heartbeat counters:
  `window_list_unknown_ticks`, `idle_unknown_ticks`, `untitled_app_window_ticks`, `health`,
  `health_since_ms`. No schema change.
- **macOS doctor** names the exact failing condition for the window list and the session, takes one
  real test frame (discarded) so a granted-but-failing capture shows, and warns when title rules
  exist and application windows report no title.

### Changed

- **macOS is presented as a fully supported platform** alongside Windows 11 and KDE Plasma 6, on the
  website, in the README and throughout the docs; the "development preview" wording is gone. The
  maintainer stated on 2026-10-11 that the Mac app is as stable in daily use as the other two platforms
  (an owner attestation, recorded as such in `CLAUDE.md`). What ships is unchanged: an Apple Silicon
  app for macOS 14+, ad-hoc signed and not notarized, in the release disk image. No code change.
- Website and README rewritten as a product pitch: value propositions, use cases, how it works, the
  rewind window's features, the command line and MCP, and one download path per platform. Static
  HTML/CSS/JS, nothing loaded from other servers.
- Stale references to v0.0.6 and to MCP or macOS being "development builds only" removed from
  `docs/README.md`, `docs/cli.md`, `docs/mcp.md` and `docs/standards.md`.

## [0.1.0] - 2026-10-11

**Navigate and take clips out.** Marks, keyframes, a real navigator in the window, multi-select and
exports. The 8-hour acceptance run for this version has **not** been run; see the ROADMAP.

### Added

- **Marks.** Mark the moment on stage (M, or "+ Mark"), with an optional label; step between marks
  ([ ]); list, rename and delete them in the marks panel; `rsrewind mark add/list/rename/remove`.
  Marks stay in this data folder and are never exported to other machines.
- **Keyframes.** Automatic stops derived when reading history (recording resumed, application or
  window switched on a monitor, a different screen); step with `,` `.` or PageUp/PageDown. No
  recorder change.
- **The navigator.** The top-right pill now steps keyframes and marks, opens the marks panel, and has
  "Go to" (`14:30`, `yesterday 9:00`, `15m`, ...). The camera glides to nearby targets and cues to
  far ones.
- **Selection and exports.** Click, Shift-click and Ctrl/Cmd-click cards (Shift with any navigation
  key extends to it); copy a picture, save a PNG, export a silent GIF, or a WebM when `ffmpeg` is
  installed. Exports go to `<data folder>/exports/` and are copies outside rsRewind's history.

### Changed

- **Database schema v2** (adds `marks`). The first run of this version backs the database up to
  `backups/recall-v1-*.db` and migrates it; older versions then refuse it. Readers (the window,
  `search`, `recent`) also accept a v1 store, so imported sources stay visible until the next import
  migrates them. `forget` reports the marks it deleted.
  **Any** command of this version that opens the database for writing migrates it, including
  `rsrewind mark add` (pressing M in the window) and `status`. If an older recorder is still running
  at that moment, it keeps writing to the migrated database (v2 only adds a table), but older
  binaries started afterwards refuse it; restart the recorder from this version. The window says
  "Updating the history format…" if a mark takes over a second while the store is still v1.

### Known issues

- **The 8-hour acceptance run for v0.1.0 has not been run** (ROADMAP). Treat this as an early release.
- **Recorder privacy gaps are still open**: every known case, with workarounds, is in [#12](https://github.com/spilloid/rsRewind/issues/12).
- The window was run on synthetic history under a virtual display with software rendering, once on real
  Linux history before this release, and never on Windows or Mac with these features. The clipboard, the
  `ffmpeg` lookup and keyboard shortcuts are unverified there. Keyframe thresholds are untuned guesses.
- Exports are copies outside rsRewind's history; `forget` does not delete them.
- Marks are local to this data folder and are not part of `.rsseg` segments.
- Reviewed read-only by a second model on 2026-10-11 and the findings fixed; there has been no review by a person.

### Upgrade notes

- The first command of this version that opens the database for writing backs it up to
  `backups/recall-v1-*.db` and migrates it to schema v2. Older versions then refuse the database, so restart the
  recorder from this version.
- Install `ffmpeg` if you want WebM export; GIF and PNG need nothing extra.

## [0.0.10] - 2026-10-11

**Tenth release: closing the icon stops recording.** Quit on the tray (Windows, Linux) or menu-bar item
(macOS) used to leave the recorder running with no icon to show it. Now Quit stops recording.

> **Privacy: open issues, read before installing.** The recorder-side privacy gaps listed in
> [#12](https://github.com/spilloid/rsRewind/issues/12) are still open on every platform, and nothing is
> encrypted at rest. Use rsRewind only on machines you own, in sessions you are comfortable recording.

### Changed

- **Quit on the tray icon stops the recorder, then removes the icon, on every platform.** The menu item now
  reads "Quit rsRewind (stops recording)". If the recorder will not stop, the icon stays up and keeps
  showing that it is still recording, rather than disappearing. Closing the history *window* still does
  not stop recording; the icon is how you see and end it.
- A crash or a forced kill of the tray process still leaves the recorder running, by design (the icon
  holds no capture and no database); `rsrewind status` and `rsrewind stop` still work from a terminal.
  Starting rsRewind again brings the icon back.

### Known issues

- **Recorder privacy gaps are still open**: every known case, with workarounds, is in [#12](https://github.com/spilloid/rsRewind/issues/12).
- If the tray process is killed or crashes, recording continues with no icon until you open rsRewind again or
  run `rsrewind stop`.
- Everything listed for 0.0.9 that is not mentioned above still applies, including that the launcher,
  icons and Quit behaviour have not been run on real Windows or Mac machines.

### Upgrade notes

- No database migration and no required settings change.
- A running tray keeps the old Quit behaviour until it is restarted: quit it once and open rsRewind again.

## [0.0.9] - 2026-10-11

**Ninth release: one way in.** Search for rsRewind on any platform and the same thing happens: the tray
icon comes up, recording starts (unless you turned that off), and the window opens. The tray icon is how
you see that it is running.

> **Privacy: open issues, read before installing.** The recorder-side privacy gaps listed in
> [#12](https://github.com/spilloid/rsRewind/issues/12) are still open on every platform, and nothing is
> encrypted at rest. Use rsRewind only on machines you own, in sessions you are comfortable recording.
> **Recording now starts by itself** when you open rsRewind or sign in; see Changed for how to turn that off.

### Added

- **`rsrewind launch`, and a branded launcher on every platform.** It brings up the tray icon (and, by
  default, the recorder) and then the window, and is idempotent: a second launch finds the icon already up.
  Running `rsrewind` with no arguments, or double-clicking the executable, does the same.
  - **Windows:** the Start Menu shortcut runs `launch` and `rsrewind.exe` now carries the rsRewind icon, so
    Windows search, Explorer and the taskbar show it.
  - **Linux:** the first run adds `rsrewind.desktop` and its icon to your user menu, so KRunner and the GNOME
    overview find "rsRewind" by name.
  - **macOS:** the app bundle now has an icon, and opening it launches the tray item and the window.
- **The AppImage installs itself on first run.** An AppImage runs from a temporary mount that vanishes when it
  exits, so anything it started (the tray icon, the recorder) died with it, and KDE could not recognise the
  program for screenshot authorization. The first run now copies the program to
  `~/.local/share/rsRewind-app/` and continues from there; a newer AppImage refreshes the copy. The install
  page now leads with the AppImage for Linux.
- `capture.record_on_launch` in `config.toml` (default `true`).

### Changed

- **Recording starts when you open rsRewind or sign in.** Before, the window waited for you to press Start
  recording. Recall is the point of the app, and a recorder that is off after installing is a recall tool that
  has nothing to recall. The tray icon (red while recording, green when not) and `rsrewind status` always show
  the state. Set `record_on_launch = false` under `[capture]` to bring up only the tray icon and window and
  start recording yourself. A settings screen for this, and for time-of-day schedules, is planned for 0.0.10.
- **The Windows installer's start-at-login entry is on by default** (it was opt-in), so the tray icon and
  recording come up after every sign-in. Install without it with `msiexec /i rsrewind.msi ADDLOCAL=Application`
  or run `rsrewind tray --autostart off` afterwards.
- PRIVACY.md, the install page, the guide and the installer notes say all of the above.

### Known issues

- **Recorder privacy gaps are still open**: every known case, with workarounds, is in [#12](https://github.com/spilloid/rsRewind/issues/12).
- **Nothing in this release has been run on a real Windows or Mac machine.** The launcher, the exe icon, the
  Start Menu shortcut (which runs minimized because `rsrewind.exe` is a console program, so a brief taskbar
  flash is possible) and the Mac app icon were checked by build and test only. The Linux launcher and the
  AppImage's copy-and-continue were run here.
- On the first AppImage run the 30 MB program is copied once; it is not removed when you delete the AppImage
  (delete `~/.local/share/rsRewind-app/` and `~/.local/share/applications/rsrewind.desktop`).
- A running tray icon or recorder keeps its old version after an upgrade until it is restarted.
- Everything listed for 0.0.8 that is not mentioned above still applies.

### Upgrade notes

- No database migration. **Recording now starts by itself**; set `record_on_launch = false` under
  `[capture]` in `config.toml` if you do not want that.
- Windows: an existing install keeps its old login entry until you run `rsrewind tray --autostart on` once or
  reinstall with this MSI.
- Linux: run the new AppImage once (or `rsrewind launch` from the tarball) to get the menu entry.

## [0.0.8] - 2026-10-11

**Eighth release: the icon is there when you need it.** The Windows tray icon has existed since 0.0.5 but
nothing started it for you after an install. Now the window and the login entry both bring it up.

> **Privacy: open issues, read before installing.** The recorder-side privacy gaps listed in
> [#12](https://github.com/spilloid/rsRewind/issues/12) are still open on every platform, and nothing is
> encrypted at rest. Use rsRewind only on machines you own, in sessions you are comfortable recording.

### Changed

- **The tray icon comes up with the recorder.** Opening the rewind window while the recorder is running (or
  paused) now starts the tray icon if it is not already up; `rsrewind tray` exits at once when an icon exists,
  so this never makes a second one. Quitting the icon from its menu stops it until the next time the window
  opens or `rsrewind tray` runs.
- **Windows login entry starts the icon.** The MSI's optional start-at-login registration now runs
  `rsrewind.exe tray --start-recorder` (the same command `rsrewind tray --autostart on` writes) instead of
  `rsrewind.exe start`, so the icon and the recorder start together and capture is visible after sign-in.
  Existing installs keep their old entry until you run `rsrewind tray --autostart on` once or reinstall with
  the start-at-login feature.
- The install page says where to find the icon on Windows (it may sit in the hidden-icons overflow).

### Known issues

- **Recorder privacy gaps are still open**: every known case, with workarounds, is in [#12](https://github.com/spilloid/rsRewind/issues/12).
- **The Windows tray icon has not been checked by eye in the taskbar** (light and dark themes, several DPI
  scales); it was verified by driving its menu commands in a real session.
- **The macOS menu-bar item and app are a development preview**: not notarized, and not run on a real Mac.
- The AppImage and the macOS DMG, first shipped in 0.0.7, have not been run from a published release yet.
- Everything listed for 0.0.7 that is not mentioned above still applies.

### Upgrade notes

- No database migration and no required settings change.
- Windows: to have the icon at every sign-in on an existing install, run `rsrewind tray --autostart on` once.

## [0.0.7] - 2026-10-11

**Seventh release: ask your history, and a Mac.** AI assistants can now search your screen history through an
MCP server (off until you enable it), Apple Silicon Macs get a development-preview app, the rewind window puts
the current moment front and centre, and every release now ships for Windows, Linux and macOS with a
SHA-256 for each file.

> **Privacy: open issues, read before installing.** The recorder-side privacy gaps listed in
> [#12](https://github.com/spilloid/rsRewind/issues/12) are still open on every platform, and nothing is
> encrypted at rest. Use rsRewind only on machines you own, in sessions you are comfortable recording.

### Documentation

- Product-first website and README, platform-aware installation suggestions, everyday guide and separate CLI/development references. macOS is clearly labeled as a development preview; privacy and release-channel limits stay visible.

### Added

- **macOS Apple Silicon development client.** Native screen capture and on-device OCR, the shared rewind window, a menu bar with pause/resume and confirmed forget, and per-user login startup. ARM64 cloud CI builds an ad-hoc signed app artifact; interactive Mac acceptance and notarization remain release gates. Consecutive unchanged screens reuse existing observations and compressed storage.

- **An MCP server for AI assistants.** `rsrewind mcp` lets Claude Code, Claude Desktop, Cursor, Codex or any MCP
  client search your screen history and read what was on screen (`search`, `recent`, `moment_at`, `get_moment`,
  `list_gaps`, `list_sources`, `get_status`, `pause_recording`, and `get_screenshot` if you allow pictures). It runs
  over stdio, so rsRewind still opens no network port. **Off until `rsrewind mcp --enable`**, because assistants
  send what they read to their model provider; then limited to the last 30 days and to text unless you allow
  screenshots (`[mcp]` in config.toml). Agents cannot resume recording, delete or move history. Every call is logged
  without content. Setup: `docs/mcp.md`.

### Changed

- **The current moment is the main view.** The small preview in the detail pane is gone; the moment on
  screen now fills the middle of the window, with the rewind room as a band underneath. It follows you:
  scrubbing the strip, stepping with ← / →, picking a search result or opening the window puts that moment
  on the stage, no click needed. Double-click it for the full-window viewer. The detail pane keeps the
  time, machine, app, window and recognized text.

### Release files

- **Linux AppImage** (`rsrewind-v0.0.7-linux-x86_64.AppImage`): make it executable and run it; with no
  arguments it opens the rewind window, with arguments it is the `rsrewind` command line. Not code-signed.
- **macOS disk image** (`rsrewind-v0.0.7-macos-arm64.dmg`): Apple Silicon, macOS 14+. Ad-hoc signed, **not
  notarized**; Gatekeeper will warn.
- Every file has a `<name>.sha256` next to it, and `SHA256SUMS.txt` lists all of them. The release pipeline
  refuses to publish unless the tag matches the declared version and all five downloads (MSI, Windows ZIP,
  Linux tarball, AppImage, macOS DMG) exist and verify.

### Known issues

- **Recorder privacy gaps are still open**: every known case, with workarounds, is in [#12](https://github.com/spilloid/rsRewind/issues/12).
- **The macOS app is a development preview**: not notarized, and interactive capture, permissions, multiple
  screens and sleep/wake have not been checked on a real Mac.
- **The AppImage and the macOS DMG have not been run from a published release yet**; the first
  release to carry them is this one.
- **MCP is new**: assistants send what they read to their model provider, so it is off until
  `rsrewind mcp --enable`. It has had no independent security review.
- Everything listed for 0.0.6 that is not mentioned above still applies (Linux text recognition needs
  Tesseract installed; piping `rsrewind start` hangs, [#15](https://github.com/spilloid/rsRewind/issues/15);
  thumbnails can stay blank for 20 s+ the first time a large real history opens).

### Upgrade notes

- No database migration and no required settings change.
- MCP stays off until you run `rsrewind mcp --enable`; see `docs/mcp.md`.

## [0.0.6] - 2026-10-10

**Sixth release: a front door, and a memory that can read.** Opening rsRewind now offers to start
recording instead of telling you it hasn't, and Linux history becomes searchable: the recorder reads the
screen with Tesseract. Every download now carries the notices for the open-source software inside it.

> **Privacy: open issues, read before installing.** The recorder-side privacy gaps listed in
> [#12](https://github.com/spilloid/rsRewind/issues/12) are still open on every platform, and nothing is encrypted at rest (a design that does
> not cost any usability is in `docs/design/encryption-at-rest.md`). Use rsRewind only on machines you
> own, in sessions you are comfortable recording.

### Added

- **A front door.** The rewind window no longer greets a new user with "once recording begins" and no
  way to begin: when nothing is recorded it explains what rsRewind does and offers **Start recording**
  (which also brings up the tray icon) and **Start rsRewind when I log in**. The top bar shows whether
  rsRewind is recording; when it is not, the chip itself starts it. Like the tray, the window does this
  by running `rsrewind` commands; it still never touches capture or storage.
- **Text recognition on Linux.** The Plasma recorder now reads the text on screen with Tesseract, so Linux
  history becomes searchable like Windows history, including moments recorded before this version, which
  are worked through in the background. Install Tesseract from your distribution (Fedora/Bazzite:
  `rpm-ostree install tesseract` or `brew install tesseract`; Debian/Ubuntu: `apt install tesseract-ocr`);
  rsRewind runs it as a separate program on two threads and never downloads anything. Until it is
  installed, moments wait and `rsrewind doctor` says what to do. Chosen by measurement over the pure-Rust
  `ocrs`: on a real dark-mode screen Tesseract found 5 of 8 visible phrases in about 1.5 s, `ocrs` 1 of 8
  in about 16 s.
- **Third-party notices.** Every release now carries `THIRD-PARTY-NOTICES.html` (generated from the exact
  dependency graph, including libwebp's BSD license), and CI rejects any dependency whose license is not on
  the permissive allowlist. Policy and inventory: `docs/licensing.md`.

### Known issues

- **Recorder privacy gaps are still open**: every known case, with workarounds, is in [#12](https://github.com/spilloid/rsRewind/issues/12).
- **Linux text recognition needs Tesseract installed** (see Upgrade notes). Measured on a real dark-mode
  laptop screen it reads most interface text but not all of it (5 of 8 phrases on the test screen).
- Piping the output of `rsrewind start` or `rsrewind tray` hangs, and `rsrewind status | head` ends with
  a broken-pipe panic ([#15](https://github.com/spilloid/rsRewind/issues/15)). Redirect to a file instead.
- The first time the window opens on a large real history, thumbnails can stay blank for 20 seconds or
  more; later openings are quick.
- The Windows tray was verified by driving its menu commands in a real session; how the icon looks in the
  Windows taskbar has not been checked by eye yet.
- When the recorder cannot write fast enough, changed screens can be dropped and the previous picture's
  span extended over them (F9 in [#12](https://github.com/spilloid/rsRewind/issues/12)).
- Everything listed for 0.0.5 that is not mentioned above still applies.

### Upgrade notes

- No database migration and no required settings change.
- Linux: install Tesseract once (`rpm-ostree install tesseract` and reboot on Fedora Atomic / Bazzite,
  `brew install tesseract`, or `sudo apt install tesseract-ocr`), then restart the recorder. Everything
  recorded since 0.0.4 is read in the background, oldest first; on the maintainer's laptop that ran at
  about 0.6 s per moment with the recorder using 5-7 % CPU. `rsrewind doctor` shows "Tesseract ready".
- The notices file is `THIRD-PARTY-NOTICES.html`, next to `rsrewind` in the ZIP and the tarball, and in the
  install folder for the MSI.

## [0.0.5] - 2026-10-09

**Fifth release: always there, honest about the holes.** rsRewind gets a tray icon on Windows and
Linux (the new red and green artwork) to pause, resume, forget the last 10 minutes or hour, and open the
window, and every stretch of time with nothing recorded is now explained, in the command line and in
the rewind window.

> **Privacy: open issues, read before installing.** The recorder-side privacy gaps listed in
> [#12](https://github.com/spilloid/rsRewind/issues/12) are still open on every platform, and nothing is encrypted at rest. Use rsRewind only
> on machines you own, in sessions you are comfortable recording.

### Added

- **Tray icon (Windows and Linux).** `rsrewind tray` puts rsRewind in the notification area (Windows;
  KDE Plasma natively, other Linux desktops through their tray applet). The icon is red while
  recording and green while not, with a badge for the rest (pause bars, a stop square, an amber dot
  when privacy rules are not enforced or the recorder is in trouble), so state never depends on colour
  alone. Its menu pauses (15 minutes, 1 hour, until resumed) and resumes, forgets the last 10 minutes or
  hour (a second click within 10 seconds confirms; one click deletes nothing), opens the window, and
  starts or stops the recorder; a left click opens the window. It is its own process and acts only by
  running `rsrewind` commands, so quitting or crashing it never touches recording.
  `rsrewind tray --autostart on` starts it at login (an XDG autostart entry on Linux, the per-user `Run`
  key on Windows; `off` undoes it). macOS is next.
- **Gaps are explained.** `rsrewind gaps [--since 24h] [--until] [--min-seconds 60] [--json]` lists every
  stretch with no recorded picture and why: recorder off, recorder stopped unexpectedly, paused, idle or
  locked, or nothing stored while recording (a privacy rule or a capture problem; privacy skips are not
  written as events yet, so the two are not told apart). `rsrewind recent` prints a line where time
  jumps. A screen that did not change is never a gap. It works for imported machines too, up to the last
  thing each one sent, and recognises a recorder that died from its stale heartbeat. No database change.
- **Gaps in the rewind window.** The strip under the room shows each gap as a faint band (rose when
  something went wrong). When the camera sits inside a gap a note under the time says how long and why,
  and stepping with ← / → across one says what was skipped. The window looks up gaps over the last 30
  days.

### Changed

- Releases are normal GitHub releases. v0.0.2, v0.0.3 and v0.0.4 were first published as pre-releases
  and became normal releases on 2026-10-08, each with a notice listing the open privacy issues
  ([#12](https://github.com/spilloid/rsRewind/issues/12)).

### Known issues

- **Recorder privacy gaps are still open**: see [#12](https://github.com/spilloid/rsRewind/issues/12) for every known case, with workarounds.
- **No text recognition on Linux yet**: Linux moments appear in `recent`, `gaps` and the window but are
  not searchable by text.
- Piping the output of `rsrewind start` or `rsrewind tray` hangs, and `rsrewind status | head` ends with
  a broken-pipe panic ([#15](https://github.com/spilloid/rsRewind/issues/15)). Redirect to a file instead.
- The first time the window opens on a large real history, thumbnails can stay blank for 20 seconds or
  more while the decoders work; later openings were quick. Seen twice on Linux, cause not established.
- The Windows tray was verified by driving its menu commands in a real session; how the icon looks in
  the Windows taskbar has not been checked by eye yet.
- When the recorder cannot write fast enough (seen on a 2496x1664 Linux screen), changed screens are
  dropped and the previous picture's span is extended over them (finding F9 in [#12](https://github.com/spilloid/rsRewind/issues/12)).
- Everything listed for 0.0.4 that is not mentioned above still applies.

### Upgrade notes

- No database migration and no required settings change.
- New: `rsrewind tray`. To have the icon at every login, run `rsrewind tray --autostart on` once.
- Linux: to have the recorder start with your Plasma session, a systemd user service works well
  (`ExecStart=%h/.local/bin/rsrewind daemon`, `WantedBy=graphical-session.target`); see the Linux section
  of the install page. A built-in `rsrewind service install` is planned.

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
