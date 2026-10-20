# Command-line reference

The desktop app is the easiest starting point. Use these commands for automation, diagnostics and deeper control. Run `rsrewind --help` or `rsrewind <command> --help` for flags.

On Windows, the installer does not add the executable to PATH; in PowerShell use `& "$env:LOCALAPPDATA\Programs\rsRewind\rsrewind.exe"` followed by the command. On a Mac use `/Applications/rsRewind.app/Contents/MacOS/rsrewind`. On Linux the AppImage keeps its copy at `~/.local/share/rsRewind-app/rsrewind`, and the tarball instructions put it in `~/.local/bin`. Every command below works the same on all three platforms.

## CLI commands

| Command | Purpose |
|---|---|
| `rsrewind start` / `stop` | Start the recorder in the background / stop it gracefully. Windows: `stop --legacy-session` stops a recorder from 0.1.2 or earlier, whichever one runs in the Windows session. |
| `rsrewind daemon` | Run the recorder in this console (debugging). |
| `rsrewind status [--json]` | Whether it is recording or paused, plus counters. Always available; there is no hidden mode. |
| `rsrewind pause [--minutes N]` / `resume` | Pause capture, optionally for a fixed time. |
| `rsrewind search <text> [--app] [--title] [--since] [--until] [--limit] [--json]` | Full-text search over recognized text, on this machine and every imported one. |
| `rsrewind recent [--limit] [--json]` | The most recent moments across all machines, newest first. |
| `rsrewind gaps [--since] [--until] [--min-seconds] [--json]` | When nothing was recorded, and why (off, crashed, paused, idle or locked, ...). |
| `rsrewind mark add [--at TIME \| --at-ms MS] [--label TEXT \| --label-stdin] [--json]` | Mark a moment (default: now) with an optional one-line label of up to 120 characters (`--label-stdin` reads it from standard input, so it stays out of the process list). Refused inside forgotten time. |
| `rsrewind mark list [--json]` / `mark rename <id> --label TEXT\|--label-stdin` / `mark remove <id>` | List, relabel or delete marks. Marks stay in this data folder (never exported); `forget` and age retention delete the ones in their range. The window's M key and marks panel run these. |
| `rsrewind forget <since> --yes` | Delete recent history from the active store (screenshots, text, window titles). |
| `rsrewind ui` | Open the rewind window (its own process; `--foreground` stays attached). |
| `rsrewind export` | Seal settled history into segment files for another machine. |
| `rsrewind import <files or folders>` | Merge segment files from other machines (safe to repeat). |
| `rsrewind sources` | List the machines whose history this one holds. |
| `rsrewind doctor [--json]` | Check capture (including screen-recording permission on macOS and the KWin `.desktop` grant on Plasma), OCR, free disk space, the storage footprint against `max_size_gb`, database, privacy rules, replication and whether the recorder is saving. |
| `rsrewind tray [--start-recorder] [--autostart on\|off]` | The icon next to the clock (notification area on Windows, system tray on Linux, menu bar on macOS): state at a glance, pause, resume, forget the last 10 min / 1 h, open the window, start/stop, Quit (stops recording). `--autostart` registers or removes login startup (HKCU Run entry, XDG autostart file or LaunchAgent). |
| `rsrewind mcp [--enable\|--disable]` | Let an AI assistant search your history over MCP (stdio; off until enabled; see `docs/mcp.md`). |
| `rsrewind data-dir` | Print the data folder. |

Human-readable output reads naturally; `--json` emits the stable `rsrewind-core` query types
(`SearchHit`, `TimelineEntry`, ...) for scripts and agents.

```
$ rsrewind search konica toner
2026-10-06 21:37:56
Application: WindowsTerminal
Window: zebra crossing schedule
"... [konica] bizhub C360 [toner] low ..."
Image: C:\Users\you\AppData\Local\rsRewind\media\2026\10\07\20261007T013756157Z_m1.webp  (id 4)
```

`search` and `recent` never execute user-supplied SQL — see `ARCHITECTURE.md`'s query
architecture section. Several machines in one view: see **[Many machines](https://spilloid.github.io/rsRewind/probes.html)**
and `docs/design/distributed.md`.


## Storage and settings

Run `rsrewind data-dir` to find your history. Defaults:

| Platform | Folder |
|---|---|
| Windows | `%LOCALAPPDATA%\rsRewind` |
| Linux | `$XDG_DATA_HOME/rsRewind`, or `~/.local/share/rsRewind` |
| macOS | `~/Library/Application Support/rsRewind` |

`config.toml` holds settings and privacy rules; `recall.db` holds events and recognized text; `media/` holds screenshots; `backups/` holds database backups; `outbox/` holds exports; `sources/` holds imported history. Stop the recorder before moving its store. `RSREWIND_DATA_DIR` or `--data-dir` selects another location. Defaults retain 90 days with a 50 GB soft storage limit, which counts `recall.db`, its WAL and `media/` only (not backups, exports, outbox, sources or logs) and is enforced hourly. Data is unencrypted at rest. Forgetting history does not erase exported copies or backups.

[Privacy and configuration](../PRIVACY.md) · [Multiple computers](https://spilloid.github.io/rsRewind/probes.html) · [Architecture](../ARCHITECTURE.md)
