# macOS Apple Silicon client

Development client for macOS 14 or newer on Apple Silicon. The hosted macOS CI
builds the native ARM64 executable and packages `rsRewind.app` as a run artifact.
This artifact is ad-hoc signed; it has no Developer ID signature or notarization.
Do not confuse it with a notarized public release. Interactive capture, permission,
menu layout and sleep/wake acceptance require a real Mac and are not proven by CI.

## Install and open

Download the `rsRewind-macos-arm64` artifact from the corresponding GitHub Actions
run, unzip it, and copy `rsRewind.app` to Applications. Open it to see the existing
native rewind window. Recording starts only when you choose **Start recording**.
macOS controls screen-recording access in System Settings → Privacy & Security →
Screen & System Audio Recording. Grant access to the application you actually use
and restart it if macOS asks. The app cannot bypass a denied permission.

For CLI and MCP connections use the binary inside the app:

```sh
/Applications/rsRewind.app/Contents/MacOS/rsrewind doctor
/Applications/rsRewind.app/Contents/MacOS/rsrewind tray --start-recorder
/Applications/rsRewind.app/Contents/MacOS/rsrewind status
```

Keep the app at a stable path: login startup and assistant connections point to
that executable. Recording and history are separate from the window and menu bar;
quitting either does not stop the recorder. Use **Stop recording** to stop it.
The menu bar uses the same state, pause/resume and two-step forget model as Windows
and Linux. Its privacy warning reflects the recorder's reported enforcement state.

## Local storage and startup

History lives under `~/Library/Application Support/rsRewind`, private to its owner.
The portable recorder detects meaningful visual changes; an unchanged screen
extends the existing observation instead of writing another screenshot or OCR job.
New states use compressed WebP images with existing retention and size limits.
This coalesces consecutive duplicates; it does not promise global deduplication of
recurring screens or shared files across machines. No storage schema changes.

```sh
/Applications/rsRewind.app/Contents/MacOS/rsrewind tray --autostart on
/Applications/rsRewind.app/Contents/MacOS/rsrewind tray --autostart off
```

Login startup writes a per-user LaunchAgent with an argument array, not a shell
command, to `~/Library/LaunchAgents/us.spillerstech.rsrewind.plist`. It starts the
menu bar and recorder at the next graphical login. Turning it off removes future
login startup; it does not stop a recorder already running or delete history.
Uninstall: disable startup, stop recording, quit the menu bar and remove the app.
Delete the history folder only if you explicitly want to erase recordings.

## MCP

The same executable serves the first-class stdio MCP surface. See [MCP setup](mcp.md)
for deliberate enablement and access limits. For Claude Desktop, for example:

```json
{"mcpServers":{"rsrewind":{"command":"/Applications/rsRewind.app/Contents/MacOS/rsrewind","args":["mcp"]}}}
```

Assistants may send returned content to their providers. MCP is off by default;
screenshot access requires its separate opt-in. It adds no network listener.

## Acceptance checklist (real Mac required)

- Deny screen recording: actionable error, no stored screenshots.
- Grant permission, restart, record a synthetic desktop and search its recognized text.
- Place an excluded app/title on each display: the entire affected monitor is skipped.
- Pause/resume, stop/start, confirm forget twice, and quit the menu bar during recording.
- Leave a static screen visible: screenshot count stays stable while duration extends.
- Lock, fast-user-switch, sleep/wake, attach/detach a Retina/external display: no private
  or stale frame is stored; capture resumes only with valid context.
- Login startup works; disabling it and uninstalling preserve recordings.
- Verify native menu and window screenshots on synthetic history in light/dark mode
  and Retina/external display scaling (STD-008).

These are release acceptance gates, not checked boxes merely because CI is green.
