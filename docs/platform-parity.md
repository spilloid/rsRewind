# Platform parity

What each platform actually does for each user-visible behaviour, and how sure we are. Written
2026-10-16 (road-to-1.0 audit, `docs/dev-process.md`) from the source at v0.1.2 plus branch
`hardening/road-to-1.0`. This file is subject to STD-003: when a row goes stale, it is a bug.

**Evidence levels** (never upgrade one without doing the thing):

- **ran**: someone used it on a real machine of that platform, with a date in `CLAUDE.md` or below.
- **tested**: an automated test exercises that platform's code path *on that platform* in CI
  (`ci.yml`: Windows and Ubuntu; `macos.yml`: Apple Silicon). Portable code tested on fakes counts as
  tested for every platform only where the row says "shared".
- **static**: read in the source, not run and not tested on that platform.
- **owner**: the maintainer's attestation from daily use, not independently checked.

What CI builds and tests per OS (from the workflow files, 2026-10-16): Windows `fmt`, `clippy -D
warnings`, `cargo test --workspace`, release build; Ubuntu `clippy`, `cargo test --workspace`, release
build of the CLI, `cargo deny check licenses`, the docs version check; macOS (`macos-15`, arm64)
`fmt`, `clippy`, `cargo test --workspace`, release build, app bundle and ad-hoc signature check. No CI
runner has a desktop session, a screen-recording grant or a compositor, so **no capture, window-list,
idle or OCR backend is exercised by CI on any platform**; only their pure parsing/classification code
is.

## Recording

| Behaviour | Windows 11 | KDE Plasma 6 (Wayland) | macOS 14+ (Apple Silicon) | Evidence |
|---|---|---|---|---|
| Frame source | WGC, frames on change, per monitor | KWin ScreenShot2 per screen, every tick | ScreenCaptureKit screenshot per display, every tick | Win ran (daily, 2026-10-16); Plasma ran (one machine, 2026-10-08); Mac ran (0.1.1, owner, 2026-10-16) |
| Frame layout handed to storage | BGRA top-down, row pitch stripped (`wgc.rs` tests) | QImage ARGB32/RGB32 (= BGRA on little-endian), stride kept (`kwin.rs` tests) | CGContext BGRA premultiplied, top-down since 0.1.2 | Win/Linux tested; **Mac orientation fix unverified on a Mac**; shared encode/decode orientation pinned by `rsrewind-query/tests/orientation.rs` (tested, all three CI OSes) |
| Capture resolution | physical pixels | device pixels (screen scale applied) | `CGDisplayPixelsWide/High` (suspected to be *points* on Retina, i.e. half the panel's pixels; affects OCR of small text) | static |
| Window list for exclusions | `EnumWindows`, all visible top-level windows | KWin script, current virtual desktop, not minimized | `CGWindowListCopyWindowInfo`, on-screen only; untitled windows kept (title rules cannot match them) | Win ran; Plasma ran; Mac owner; classification tested on all CI OSes (`macos_facts.rs`, `kwin.rs` parsing on Linux) |
| Window → monitor geometry | per-monitor DPI aware V2 (`enable_dpi_awareness`) | KWin logical coordinates for windows and screens alike | CG global points, top-left origin for windows and displays, mapped to a horizontal pixel strip | static (Mac mapping tested on fakes) |
| Foreground app/window per moment | yes | yes (KWin active window) | **none** (not attributed) | Win/Plasma ran; Mac by design |
| Idle time | `GetLastInputInfo`; **unknown now fails closed** (was "active", fixed on this branch) | Wayland idle-notify; locked = idle; unknown = do not record | `CGEventSource` + session facts; unknown = do not record | static (recorder's handling of "unknown" is tested on fakes, shared) |
| Lock screen | not detected; the lock screen and UAC run on the secure desktop, which a user process cannot capture; idle after `idle_after_secs` | screen locker over D-Bus counts as idle | heuristics: console/login keys, undocumented `CGSSessionScreenIsLocked`, `loginwindow` or unidentified display-covering windows | Win static; Plasma ran; Mac static (owner) |
| Monitor hot-plug / resolution / DPI change | re-enumerated every 30 s and at once after a capture error; changed geometry restarts the capturer (shared); WGC pool recreated on size change | same shared logic | same shared logic | shared logic tested on fakes; Windows: the installed 0.1.2 log on the maintainer's machine shows displays found, changed and removed on 2026-10-16/17 with no warning or error logged (seen read-only; whether the pictures around them are right was not checked); Plasma and Mac never ran it |
| Sleep/wake | shared: spans never bridge more than three ticks; idle after wake if no input | same | same | static; checklist items in `docs/macos.md` never run |
| Single recorder | one per **Windows session** (named mutex) | one per **data folder** (`recorder.lock`) | one per **data folder** | static; documented difference, see below |
| `rsrewind stop` | sets the stop event of exactly the run this folder's live heartbeat names by its random `stop_token` (**fixed on this branch**: it used to stop whichever recorder ran in the session); a run that has exited, or a stale heartbeat, reaches no one. A 0.1.2 or older recorder (no token) is signalled only with `--legacy-session`, which reaches whichever such recorder runs in the session | SIGTERM to the pid in this folder's heartbeat (a stale heartbeat whose pid was reused would signal that process) | same as Plasma | Win: CLI `stop_target` unit tests (replacement between check and signal, stale heartbeat with a reused pid, legacy without and with the flag), `win.rs` unit test, `stop_scope.rs` end to end (a gone run and a legacy heartbeat signal nothing; `--legacy-session` itself is not run, it would reach the installed recorder); others static |
| Saving health ("Recording · nothing saved") | shared; now also reports **storage failing** (disk full, unwritable folder) | shared | shared | tested on fakes (shared) |
| Start at login | HKCU Run entry (installer or `tray --autostart`) | XDG autostart file | per-user LaunchAgent | Win ran (installer); Plasma ran; Mac owner |
| Notification icon | notification area | StatusNotifierItem | AppKit menu bar | Win/Plasma ran; Mac owner |

## Text recognition

| Behaviour | Windows | Plasma | macOS | Evidence |
|---|---|---|---|---|
| Engine | Windows.Media.Ocr (in process) | Tesseract child process (must be installed) | Apple Vision (Swift bridge) | Win ran; Plasma ran; Mac owner |
| Box coordinates | top-left origin | TSV, top-left | Vision's bottom-left boxes converted (`1 - maxY`) | Win/Linux ran; Mac static |
| `doctor` OCR check | language packs | Tesseract found + language | Vision available | static per platform |

## Data, privacy and diagnostics

| Behaviour | Windows | Plasma | macOS | Evidence |
|---|---|---|---|---|
| Default data folder | `%LOCALAPPDATA%\rsRewind` | `$XDG_DATA_HOME/rsRewind` or `~/.local/share/rsRewind` | `~/Library/Application Support/rsRewind` | ran (all) |
| Folder permissions | inherited: private under the profile by default (SYSTEM, Administrators, the user; checked read-only on the installed copy 2026-10-16). **A custom `--data-dir` / `RSREWIND_DATA_DIR` inherits its parent's ACL and may be readable by other users** | root set to `0700` on every start | `0700` on every start | Win ran (icacls); Unix tested (`the_data_folder_is_private_to_its_owner`) |
| Picked up by backups | `AppData\Local` is not roamed; machine/enterprise backup tools may copy it | whatever backs up `~/.local/share` | **Time Machine backs up `~/Library/Application Support` by default** | static |
| Encryption at rest | none | none | none | by design today (`docs/design/encryption-at-rest.md`, exploration) |
| `doctor` free disk space | `GetDiskFreeSpaceExW` | `df -Pk` (**added on this branch**, was Windows-only) | `df -Pk` (**added on this branch**) | Win ran; parser unit-tested; Unix call static (not compiled on the authoring machine) |
| Export capability tags | `ocr`, `window_titles`, `process_names`, `multi_monitor`, narrowed by each session | **none** (the build list is empty off Windows) | **none** | static; docs used to say Mac exports carry `window_titles`, which was not true (corrected on this branch) |
| Clip export helper (`ffmpeg`) | found on PATH, run without a console window | found on PATH | found on PATH | WebM ran only under `cargo test` on Linux |
| GIF / PNG / clipboard | pure Rust, same code | same | same | GIF/PNG tested (shared, every CI OS); clipboard ran once on X11 only |

## Differences that are deliberate

- **Single instance scope.** Windows allows one recorder per session, whatever its data folder; Linux
  and macOS one per data folder. A second Windows recorder with another `--data-dir` exits at once
  and `rsrewind start` reports that it did not report in. Not changed here: a per-folder Windows
  mutex would let two recorders capture the same desktop at once, which is no one's intent.
- **No foreground attribution on macOS** (see `docs/macos.md`).
- **Windows has no lock detection** of its own; it does not need one for the lock screen itself (the
  secure desktop is not capturable), but a short lock shorter than `idle_after_secs` shows as
  recording, not as a "locked" gap.

## Path to the full version of this matrix

Every "static" and "owner" cell above becomes "ran" only by doing it. The 1.0 bar in `ROADMAP.md`
lists the drills; the per-platform checklists are `docs/macos.md` (real-Mac checklist) and the pilot
checklist in `ROADMAP.md`. Record each run with its date here and in `CLAUDE.md`.
