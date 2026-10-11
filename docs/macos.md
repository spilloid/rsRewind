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
  `CGWindowListCopyWindowInfo`, so the any-visible-window exclusion rule is enforced per monitor
  before anything is stored. Capture is attempted only while the user's session is on the console
  and logged in; otherwise, and whenever a frame or the window list cannot be obtained, the tick is
  unknown and nothing is stored.
- **Foreground application:** not attributed. CoreGraphics window ordering does not prove which
  window has focus, so the backend reports no foreground app or title rather than guess. Mac
  moments therefore carry no application or window name; exclusions still work from the full
  window list.
- **Idle:** `CGEventSource` seconds since the last input, combined with the console-session check;
  the idle threshold and locked screens are handled by the shared recorder.
- **OCR** (`rsrewind-ocr/src/macos.rs`, `macos.swift`): Apple Vision text recognition, in memory,
  on the OCR thread. Recognised content never appears in errors or logs.
- **Menu bar** (`rsrewind-tray/src/macos.rs`, `native/Tray.swift`): AppKit owns the main thread;
  the bridge exchanges only menu JSON and row indices.
- **Login** (`rsrewind-cli/src/macos_login.rs`): LaunchAgent plist generation with XML escaping.
- **Diagnostics:** `rsrewind doctor` checks screen-recording authorization, display enumeration, the
  window list and idle detection, and asks for the permission only when you run `rsrewind start`
  interactively.

Storage, change detection, retention, the query layer, the window and the segment format are the
shared, portable code.

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

### Real-Mac checklist

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
