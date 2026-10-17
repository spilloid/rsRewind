# macOS

rsRewind on macOS is a native Apple Silicon app for macOS 14 (Sonoma) or newer: the same single
`rsrewind` executable as Windows and Linux, wrapped in `rsRewind.app`. It records with
ScreenCaptureKit, recognises text with Apple Vision on device, shows a menu-bar item next to the
clock, and starts at login through a per-user LaunchAgent. There is no Intel build.

[Install steps](https://spilloid.github.io/rsRewind/install.html#macos) · [Everyday guide](https://spilloid.github.io/rsRewind/guide.html) · [Privacy](../PRIVACY.md)

## Install

Each release ships `rsrewind-vX.Y.Z-macos-arm64.dmg` alongside the Windows and Linux downloads,
with its SHA-256 in `SHA256SUMS.txt`.

1. Download the disk image and `SHA256SUMS.txt` into the same folder and verify:
   `shasum -a 256 -c SHA256SUMS.txt 2>/dev/null | grep macos-arm64.dmg` must print `OK`.
2. Open the disk image and drag **rsRewind** to Applications.
3. Open the app. The bundle is code-signed ad hoc, not with an Apple Developer ID, and it is not
   notarized, so Gatekeeper says it cannot verify the developer the first time. Control-click the
   app and choose **Open**, or allow it in System Settings → Privacy & Security → **Open Anyway**.
   The checksum is your integrity check; verify it before you do this.
4. Grant **Screen Recording** in System Settings → Privacy & Security (called Screen & System Audio
   Recording on some versions) when the app asks, and reopen it if macOS asks you to. The recorder
   cannot and does not bypass a denied permission: without it, `rsrewind doctor` fails the capture
   check and nothing is stored.

Opening the app starts the menu-bar item and, unless `capture.record_on_launch = false` in
`config.toml`, the recorder, then shows the rewind window. On macOS the window runs in the app's
own process so the app stays in the Dock; the recorder is a separate process, as on every platform.

## The menu bar

The menu-bar item is the always-visible sign that recording is running. Its symbol changes with the
state (recording, paused, stopped, unknown), it is tinted red while recording and orange when the
newest session ran with privacy rules not enforced. The menu offers pause (15 minutes, 1 hour, until
resumed), resume, forget the last 10 minutes or 1 hour (two clicks: the first arms, the second within
10 seconds confirms), open the window, and **Quit rsRewind (stops recording)**. Closing the window
does not stop the recorder; Quit does. The menu bar holds no capture and no database: it acts only
by running `rsrewind` subcommands.

## Command line

The command-line tool is the executable inside the bundle. Keep the app in Applications so login
startup and assistant connections keep pointing at the same path:

```sh
/Applications/rsRewind.app/Contents/MacOS/rsrewind doctor
/Applications/rsRewind.app/Contents/MacOS/rsrewind status
/Applications/rsRewind.app/Contents/MacOS/rsrewind search violet bicycle
/Applications/rsRewind.app/Contents/MacOS/rsrewind tray --start-recorder
```

Every subcommand in the [CLI reference](cli.md) works the same way on macOS.

## Storage and login startup

History lives in `~/Library/Application Support/rsRewind` (`rsrewind data-dir` prints it), private
to your account: `config.toml`, `recall.db`, `media/`, `backups/`, `outbox/`, `sources/` and
`exports/`, exactly as on the other platforms. Nothing is encrypted at rest; see
[PRIVACY.md](../PRIVACY.md). Retention defaults are 90 days and a 50 GB soft limit.

```sh
/Applications/rsRewind.app/Contents/MacOS/rsrewind tray --autostart on
/Applications/rsRewind.app/Contents/MacOS/rsrewind tray --autostart off
```

`--autostart on` writes `~/Library/LaunchAgents/us.spillerstech.rsrewind.plist`, a per-user
LaunchAgent with an argument array (no shell command interpolation) that starts the menu bar and the
recorder at the next graphical login. `--autostart off` removes it; that does not stop a recorder
already running or delete history.

**Uninstall:** turn autostart off, choose Quit in the menu bar, and move the app to the Trash. The
history folder is never deleted by removing the app; delete it yourself only if you want to erase
your recordings.

## MCP

The same executable serves the stdio MCP server; see [MCP setup](mcp.md) for enabling it and its
access limits. For Claude Desktop, for example:

```json
{"mcpServers":{"rsrewind":{"command":"/Applications/rsRewind.app/Contents/MacOS/rsrewind","args":["mcp"]}}}
```

MCP is off until `rsrewind mcp --enable`; screenshots need a separate opt-in; assistants may send
what they read to their provider. It opens no network port.

## How the Mac backend works

All Apple-specific code sits behind the recorder's platform seam (`ARCHITECTURE.md`), in small Swift
bridges compiled into static archives by each crate's build script and linked into the one binary:

- **Capture** (`rsrewind-capture/src/macos.rs`, `macos.swift`): displays and frames come from
  ScreenCaptureKit; the visible-window list (process name, title, geometry) from
  `CGWindowListCopyWindowInfo`. The any-visible-window rule is checked per monitor before anything
  is stored: application rules against every on-screen window, title rules only against windows
  macOS reports a title for (see [what that relaxes](#window-list-rules-and-what-they-relax)).
  Capture is attempted only while the session is on the console and logged in. Nothing is stored
  on a tick whose window list is unknown (by the rules below), whose session is unknown, or whose
  frame fails; an *individual* untitled window or unresolvable owner no longer makes the list
  unknown. The Swift bridge only reports raw facts as JSON; which windows
  count and when something is unknown is decided in portable Rust
  (`rsrewind-capture/src/macos_facts.rs`, unit-tested on every platform). See
  [window list rules](#window-list-rules-and-what-they-relax) below.
- **Foreground application:** not attributed. CoreGraphics window ordering does not prove which
  window has focus, so the backend reports no foreground app or title rather than guess. Mac
  moments therefore carry no application or window name; exclusions still work from the full
  window list.
- **Idle:** `CGEventSource` seconds since the last input, combined with the session check. The
  session is unknown (nothing is recorded, and no frame is taken) when it is not on the console,
  login has not finished, `CGSSessionScreenIsLocked` is set, the window list cannot be read, any
  window owned by `loginwindow` is on screen, or a window that nothing identifies (no executable, no
  owner name) covers at least 90 % of a display or has unreadable bounds. A window owner that
  `NSRunningApplication` cannot resolve no longer makes it unknown by itself. The session is checked
  before and after every frame, in Rust. **Lock detection rests on heuristics:** macOS offers no
  documented "screen is locked" call to this kind of process, so it relies on the console/login
  keys, the *undocumented* `CGSSessionScreenIsLocked` key, and the lock UI's windows being owned by
  `loginwindow` (or being unidentified and display-covering). A macOS release that changes those
  could let a lock screen through; the real-Mac checklist below includes locking.
- **OCR** (`rsrewind-ocr/src/macos.rs`, `macos.swift`): Apple Vision text recognition, in memory,
  on the OCR thread. Recognised content never appears in errors or logs.
- **Menu bar** (`rsrewind-tray/src/macos.rs`, `native/Tray.swift`): AppKit owns the main thread;
  the bridge exchanges only menu JSON and row indices.
- **Login** (`rsrewind-cli/src/macos_login.rs`): LaunchAgent plist generation with XML escaping.
- **Diagnostics:** `rsrewind doctor` checks screen-recording authorization, takes **one real test
  frame** of the first display (discarded at once, nothing stored: permission can read "granted"
  while capture still fails; a 15 s ScreenCaptureKit timeout shows as "ScreenCaptureKit timed out",
  worth watching for on a real Mac), reports the window list (counts of untitled windows, owners
  named by fallback, zero-size windows skipped) or the exact reason it is unknown, and the session
  state or the exact reason it is unknown (with the shape of any `loginwindow` windows). It asks
  for the permission only when you run `rsrewind start` interactively. Run from Terminal, macOS
  judges *Terminal's* Screen Recording grant, not rsRewind.app's.

Storage, change detection, retention, the query layer, the window and the segment format are the
shared, portable code.

## Window list rules and what they relax

Until issue #33 the bridge discarded the **whole** window list when any one on-screen window had no
title, an owner `NSRunningApplication` could not resolve, or a non-integer layer. Every real Mac has
such windows (WindowServer, Control Center, the Dock, MDM and security agents' overlays), so every
monitor was skipped on every tick while the menu bar said "Recording". The rules now are:

| Window fact | Treatment |
|---|---|
| No title (`kCGWindowName` absent or empty) | Kept, with an empty title, exactly as an untitled window on Windows. |
| Owner not resolvable by `NSRunningApplication` | Kept; process name is `kCGWindowOwnerName`. |
| Owner resolvable, display name differs from the executable name | Kept, and rules are checked against **both** names (an Electron app named "Code" whose executable is "Electron" matches either rule). |
| No owner name at all | Kept as `unknown`; if any application rule exists, the **whole list is unknown** (nothing recorded). |
| Layer missing or not an integer | Kept. |
| Bounds missing, unparseable, not finite or negative | The **whole list is unknown** (`bounds-unknown`): the window could be on any display. |
| Valid bounds of zero width or height | Skipped and counted (nothing of it is visible). |

**What this relaxes, plainly:** a window-title exclusion rule (`excluded_title_patterns`) cannot
match a window whose title macOS does not report. Before, such a window made the recorder store
nothing at all; now that window's monitor is recorded unless an application rule or another
window's title excludes it. Application rules are unaffected (and match more than before).

**The safeguard:** when title rules exist and regular applications (Dock apps) have windows on
screen but **not one** of them reports a title, titles are evidently being withheld (a permission or
TCC identity problem), so the list is treated as unknown and nothing is recorded (doctor and status
say `titles-unavailable`). rsRewind's own windows never count as evidence: neither this process nor
any process running the rsRewind executable (the separate `rsrewind ui` window), and neither do
owners `NSRunningApplication` could not resolve. A Mac showing only one untitled application window
(some full-screen apps) therefore records nothing while title rules exist; that is the deliberate,
visible, fail-closed cost.

**Visible, not silent:** whenever title rules exist and application windows report no title, the
recorder counts those ticks (`untitled_app_window_ticks`); `rsrewind status` then prints "⚠ Title
rules could not be checked on N tick(s)…", `status --json` carries `untitled_app_window_ticks`, and
`doctor` has a `title rules` warning (and, on the Mac itself, counts in its window-context line).

**Open owner decisions (2026-10-16):**

- Whether this per-window relaxation is acceptable, or the stricter alternative (fail closed
  whenever *any* regular application window is untitled while title rules exist) is preferred. The
  stricter option may re-create #33 on Macs whose apps keep untitled helper windows on screen; the
  doctor counts and `untitled_app_window_ticks` on a real Mac are the evidence to decide on.
- The macOS backend still declares the `window_titles` capability, so the recorder does not refuse
  to start with title rules and sessions record titles as enforced, although title rules cannot
  match untitled windows. (Correction 2026-10-16: exported segments from a Mac do **not** carry the
  `window_titles` tag, or any tag: `rsrewind export` claims capability tags only on Windows, see
  `docs/platform-parity.md`. Whether Mac and Linux exports should claim their session's tags, and
  then whether `window_titles` is among them, is part of the same owner decision.) Flipping the capability
  would make the recorder refuse to start with title rules unless `privacy.unenforced_ok` is set;
  that is the owner's call, not made here.
- Follow-up, not done: judge title availability **per owner** (an application that reports titles
  for some windows but not others) instead of one screen-wide "any titled window" test.

## When it says "Recording" but nothing is saved

The recorder classifies every recording tick and publishes the result in its heartbeat. `rsrewind
status` prints `⚠ Nothing is being saved since HH:MM:SS: window context unavailable` (or session
state unknown, capture failing, no displays, saving to disk is failing), `status --json` carries `not_saving` with a `reason`,
the window's chip reads "Recording · nothing saved: …", the menu bar headline says the same with a
warning badge, and `doctor` has a "saving" line with how many ticks were skipped for each reason.
The log gets one warning when the condition starts and one line when saving resumes. Idle and an
excluded window on every screen are reported as "not saving" without a warning. Run `rsrewind
doctor` for the exact cause; its window-context and idle/session lines name the failing condition.

## Build and packaging

`.github/workflows/macos.yml` runs `fmt`, `clippy -D warnings`, the test suite and a release build on
a hosted Apple Silicon runner (`macos-15`, deployment target 14.0) for every pull request and push to
`main`, then `scripts/package-macos.sh` assembles `dist/rsRewind.app` (`installer/macos/Info.plist`,
icon, LICENSE and `THIRD-PARTY-NOTICES.html` in `Contents/Resources`), signs it ad hoc, checks that
the binary links only system frameworks, and uploads the zipped app as a run artifact. The release
workflow runs the same steps and `scripts/package-macos-dmg.sh` to produce the disk image that is
published with every release. There is no Developer ID signature or notarization step; adding one
is a distribution decision tracked in the ROADMAP.

To build locally you need an Apple Silicon Mac with macOS 14+, Xcode command-line tools and the
pinned Rust toolchain: `cargo build --release -p rsrewind-cli --target aarch64-apple-darwin`, then
`./scripts/package-macos.sh` (it expects `THIRD-PARTY-NOTICES.html`, from `cargo about generate
about.hbs -o THIRD-PARTY-NOTICES.html`).

## Status and verification

- **2026-10-11:** the maintainer states that macOS is as stable in daily use as Windows and KDE
  Plasma. This is the owner's attestation from real-Mac use; it is not something hosted CI can
  prove, because a cloud runner has no interactive screen-recording grant and no desktop.
- Hosted CI proves: arm64 build, lints, the full test suite (including the portable recorder tests
  on fakes and the menu-bar model tests), bundle assembly, ad-hoc signature verification, and that
  the packaged binary starts without Xcode or Homebrew libraries.
- The checklist below is what a release should still be checked against on a real Mac; it is the
  same kind of live verification `CLAUDE.md` asks for on every platform.

- **2026-10-15:** the issue #33 fix (window list and session tolerate odd windows; "nothing saved"
  status) was written and tested on Windows only. Its Swift and macOS-only Rust were **not compiled**
  in that session; macOS CI and a real-Mac trial are the first checks of it.

### Real-Mac checklist

- `doctor` on a normal desktop: capture `[ ok ]` with a test frame, window context `[ ok ]` or
  `[warn]` with counts (not `[FAIL]`), idle/session `[ ok ]`; `status` shows moments growing and no
  "Nothing is being saved" line. Watch for `ScreenCaptureKit timed out` (the 15 s wait), for
  `bounds-unknown` or `unidentified-covering-window` (they would block recording on every tick), and
  note whether a `Title rules could not be checked` line appears and how fast its count grows.
- Deny screen recording: actionable error from `doctor` and `start`, no stored screenshots.
- Grant permission, reopen, record a synthetic desktop and search its recognised text.
- Place an excluded app or title on each display: the entire affected monitor is skipped.
- Pause/resume, stop/start, confirm forget twice, and Quit the menu bar during recording.
- Leave a static screen visible: screenshot count stays stable while the moment's duration extends.
- Lock, fast-user-switch, sleep/wake, attach/detach a Retina or external display: no private or stale
  frame is stored; capture resumes only with valid context.
- Login startup works; disabling it and removing the app preserve recordings.
- Window and menu on synthetic history in light and dark mode and at Retina and external-display
  scaling.
