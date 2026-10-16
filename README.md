# rsRewind

**Find anything you saw.** rsRewind records what changes on your screen, reads the words on it, and lets you search and rewind your own history. It runs entirely on your computer: no account, no cloud, no telemetry, no server.

**Current release: v0.1.2** · [Get rsRewind](https://spilloid.github.io/rsRewind/install.html) · [First few minutes](https://spilloid.github.io/rsRewind/guide.html) · [Website](https://spilloid.github.io/rsRewind/)

![rsRewind displaying synthetic history in the rewind window](site/img/room-dark.png)

*The rewind window on made-up history: one lane per computer, the current moment front and centre.*

## Why rsRewind

- **Local-first, by construction.** No network code in the recording path, no listening socket, no cloud component. Your history is a SQLite database and WebP screenshots in a folder you own.
- **Private by design.** Exclusion rules are checked *before* anything is written, and a whole monitor is skipped while any excluded window is visible on it. Where a platform cannot provide that information, rsRewind does not record. The recorder's state is always visible (`rsrewind status`, the icon next to the clock); there is no hidden mode.
- **Records change, not time.** About once a second per monitor it checks whether the screen changed meaningfully; an unchanged screen extends the current moment instead of storing another identical screenshot.
- **Reads what it sees, on device.** Text recognition uses the engine your platform already has (Windows.Media.Ocr, Apple Vision, Tesseract) and feeds a full-text index you can search.
- **You decide what to keep.** Defaults keep 90 days with a 50 GB soft limit. Pause, resume, forget the last ten minutes or hour in two clicks, set retention and exclusions in one `config.toml`.
- **One Rust executable.** `rsrewind` runs the recorder, the window, the tray or menu-bar icon, the command line and an optional MCP server. No browser runtime. MIT licensed.

## Choose your platform

| Platform | What you get |
|---|---|
| **Windows 11 · x64** | [Signed per-user installer or portable ZIP](https://spilloid.github.io/rsRewind/install.html#windows). Capture with Windows.Graphics.Capture, OCR with Windows.Media.Ocr, a notification-area icon, start-at-login from the installer. Windows 10 is unverified. |
| **macOS 14+ · Apple Silicon** | [Disk image with a native app](https://spilloid.github.io/rsRewind/install.html#macos). Capture with ScreenCaptureKit, OCR with Apple Vision, a menu-bar item, login startup through a per-user LaunchAgent. Ad-hoc signed, not notarized (Gatekeeper asks once); no Intel build. [Mac reference](docs/macos.md). |
| **Linux · x86-64** | [AppImage or tarball](https://spilloid.github.io/rsRewind/install.html#linux). Recording requires KDE Plasma 6 on Wayland (KWin provides screenshots and the window list); install Tesseract for text search. Other desktops can run the window and import history. |

Every release ships all five downloads together with `SHA256SUMS.txt`; the Windows EXE and MSI are Authenticode-signed.

## Your first few minutes

1. Install for your platform and open **rsRewind** from your search or application menu; it starts the icon next to the clock and recording, then opens the window.
2. Use a harmless test session. Choose **Start recording**, then open a text document with a distinctive phrase such as “violet bicycle.”
3. Give text recognition a little time, then search for that phrase in rsRewind. Pick a result to return to its screenshot.
4. Use the tray or menu-bar controls to pause, resume or stop recording. Closing the window does **not** stop the background recorder; **Quit** on the icon does.

[The everyday guide](https://spilloid.github.io/rsRewind/guide.html) covers navigation, startup, storage and troubleshooting.

> **Early release: use a session you are comfortable recording.** History is unencrypted at rest, and [known privacy gaps](https://github.com/spilloid/rsRewind/issues/12) remain open. Exclusions and pause controls are not a guarantee that sensitive content can never be stored. Read [privacy and data handling](PRIVACY.md) before recording your normal desktop.

## What you can do

- **Find a moment:** search recognized screen text, inspect the full screenshot with zoom and pan, and step backward or forward through nearby moments. Gaps show when nothing was recorded and why (off, paused, idle, locked).
- **Find your way back:** mark moments with a label (M), hop between marks and automatic keyframes (app switches, resumed recording, a new screen), and jump to a time (`14:30`, `yesterday 9:00`, `15m`).
- **Take clips out:** select moments (click, Shift, Ctrl/Cmd) and copy a picture, save a PNG, or export a silent GIF or, with `ffmpeg` installed, a WebM. Exports are copies outside rsRewind's history.
- **Keep control:** visible recorder state, pause and stop controls, configurable exclusions and retention, two-step forget.
- **Bring another computer:** [export and import history](https://spilloid.github.io/rsRewind/probes.html) into one view with a lane per machine. Transfers are file-based and manual; integrity checks do not authenticate the sender.
- **Script it:** every command has `--json` output with stable, typed records (`rsrewind search`, `recent`, `gaps`, `status`, `doctor`, ...). [CLI and storage reference](docs/cli.md).
- **Ask an assistant:** [optional MCP server](docs/mcp.md) over stdio for Claude Desktop, Claude Code and other MCP clients. Off by default, text-only unless you opt in to screenshots, no network port; returned history can reach the assistant provider.
- **Go deeper:** [architecture](ARCHITECTURE.md), [build instructions](docs/development.md), [roadmap](ROADMAP.md).

Screenshots and recognized text stay in your local store unless you export them or enable an integration that shares them. rsRewind captures screen pixels and available window metadata; it does not record keystrokes or audio. Platform support and privacy enforcement have limits described in the linked guides.

## Help and development

[Documentation](docs/README.md) · [Release notes](CHANGELOG.md) · [Known issues](https://github.com/spilloid/rsRewind/issues) · [Roadmap](ROADMAP.md) · [Contributing](CONTRIBUTING.md)

MIT licensed. Releases include third-party notices; see [licensing](docs/licensing.md).
