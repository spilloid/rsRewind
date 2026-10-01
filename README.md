# rsRewind

A Windows-native, local-first screen-history recorder written in Rust. rsRewind watches your
screens, keeps a visual record of what changed, runs OCR over it, and lets you search your own
history by text, app, window title, or time — entirely on your own machine. There is no cloud
component, no required network service, and no telemetry.

## Status: pre-release development software

**rsRewind is early, unreleased, and not ready for use on a machine you care about.**

As of this writing the workspace contains the shared domain types (`rsrewind-core`) and the
architecture contract (`docs/mvp-contract.md`) that the rest of the crates are being built
against. The capture, storage, OCR, query, daemon, CLI, and UI crates are scaffolded but not
yet implemented — the `rsrewind.exe` binary currently only prints a placeholder string. Nothing
in this README's "Usage" section works yet; it documents the contract the implementation is
being built to, and is written so that section can be verified command-by-command as each piece
lands rather than rewritten from scratch.

Do not install this on a system where you would be upset by:

- a recorder that captures continuously, including things you forgot were on screen;
- a prerelease bug losing or corrupting your recorded history;
- data stored **unencrypted at rest** (see [Privacy and data handling](#privacy-and-data-handling));
- no published code-signed binaries yet (see [code signing](docs/code-signing.md)).

See `ROADMAP.md` for what "done" looks like at each version, and `docs/dev-process.md` for how
this repository's AI-assisted implementation work is reviewed before it ships.

## What it is, concretely

rsRewind runs a background recorder that, roughly once a second per monitor, checks whether the
screen has visually changed since the last capture. When it has, it stores a WebP screenshot,
runs on-device OCR over it, and indexes the recognized text for full-text search. It also tracks
which application and window had focus, so you can later ask things like "what was I looking at
in Teams around 2:30 yesterday" or "find the email that mentioned the Q3 budget."

It is explicitly **not**:

- a cloud service, or anything that talks to one;
- a browser extension, Electron app, or anything running inside a browser runtime;
- a tool that requires Python, Docker, or any other runtime beyond Rust's own compiled output;
- a keylogger or input recorder — it captures screen pixels and window metadata, never keystrokes;
- something that hides itself. The recorder's state is always visible via `rsrewind status`, and
  there is no stealth mode.

## Supported OS

- **Windows 11** — primary target; all capture, OCR and UI APIs are developed and expected to be
  verified against current Windows 11.
- **Windows 10** — expected to work where the underlying Windows APIs (Windows.Graphics.Capture,
  Windows.Media.Ocr, per-monitor-v2 DPI awareness) are available, but this is **unverified**: no
  testing has been done on Windows 10 yet.
- No other OS is supported. rsRewind is Windows-only by design (see `ARCHITECTURE.md`).

## Build requirements

- **Rust**, stable channel, **1.95 or newer** (pinned via `rust-toolchain.toml`; edition 2024).
  Target `x86_64-pc-windows-msvc`.
- **MSVC Build Tools** (the "Desktop development with C++" workload from Visual Studio Build
  Tools) and the **Windows 11 SDK** — required to link against the Windows APIs the `windows`
  crate wraps.
- **Windows App Runtime 2.4** (the `Microsoft.WindowsAppRuntime.2` framework package) — required
  only to *run* `rsrewind ui` (it uses `windows-reactor`/WinUI 3). It is not required to build or
  run the recorder, the CLI, or the test suite.

If `cargo`/`rustc` are installed via `rustup` but not on your shell's `PATH`, prefix commands
with the toolchain's `bin` directory, e.g. (PowerShell):

```powershell
$env:PATH = "$env:USERPROFILE\.cargo\bin;$env:PATH"
```

## Building and running

```powershell
# Build everything
cargo build --workspace

# Run the test suite
cargo test --workspace

# Format and lint (both must pass with zero warnings before a change ships)
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings

# Run the CLI once it has subcommands implemented
cargo run -p rsrewind-cli -- status
```

There is one executable, `rsrewind.exe`, with subcommands — see [CLI commands](#cli-commands)
below. There is no separate installer required for development; see `docs/installer.md` for the
planned MSI packaging.

## CLI commands

Design per `docs/mvp-contract.md`; **not yet implemented** (`rsrewind-cli`'s `main.rs` is
currently a placeholder). This section documents the committed shape so each command can be
checked off as it lands, rather than written up after the fact.

| Command | Purpose |
|---|---|
| `rsrewind daemon` | Run the recorder in the foreground (for debugging/service hosting). |
| `rsrewind start` | Spawn the recorder as a detached background process with no visible console window. |
| `rsrewind stop` | Signal the running recorder to shut down gracefully. |
| `rsrewind status [--json]` | Show whether the recorder is running, recording, or paused, plus basic counters. |
| `rsrewind pause [--minutes N]` | Pause capture, optionally for a fixed duration; omit `--minutes` to pause indefinitely until `resume`. |
| `rsrewind resume` | Resume capture immediately. |
| `rsrewind search <text> [--app] [--title] [--since] [--until] [--limit] [--json]` | Full-text search over recognized OCR text, with optional app/title/time filters. |
| `rsrewind recent [--limit] [--json]` | List the most recent observations, newest first. |
| `rsrewind doctor [--json]` | Report environment/health issues: missing OCR language pack, schema problems, orphaned files, etc. |
| `rsrewind ui` | Launch the WinUI 3 desktop UI as a separate process. |
| `rsrewind data-dir` | Print the resolved data directory path. |

Human-readable output is meant to read naturally; `--json` emits the stable `rsrewind-core`
query types (`SearchHit`, `TimelineEntry`, etc. — see `crates/rsrewind-core/src/search.rs`) for
scripts and agents to consume.

Example `search` output (per the product brief's intended format — illustrative, not yet
produced by a working binary):

```
$ rsrewind search "Q3 budget" --app Outlook.exe --limit 3
2026-09-30 20:32:11  Outlook.exe  "Q3 Budget Review - Message"       "...the [Q3 budget] numbers need..."
2026-09-30 18:04:52  Outlook.exe  "RE: Q3 Budget Review"             "...attached is the revised [Q3 budget]..."
2026-09-29 11:17:03  Outlook.exe  "Q3 Budget Review - Message"       "...before we finalize the [Q3 budget]..."
```

Each row is: local timestamp / application / window title / matching snippet, with the matched
text bracketed. `search` and `recent` never execute user-supplied SQL — see `ARCHITECTURE.md`'s
query architecture section.

## Where your data lives

Everything rsRewind records stays under:

```
%LOCALAPPDATA%\rsRewind\
  config.toml                 — your settings and privacy rules
  recall.db (+ -wal, -shm)    — SQLite database: events, OCR text, search index
  media\YYYY\MM\DD\*.webp     — screenshots, one file per stored visual change
  logs\                       — application logs (never contain captured screen text; see PRIVACY.md)
  backups\                    — automatic pre-migration database backups
  models\                     — any local models the recorder downloads for its own use
```

This folder is portable: move it, back it up, or copy it to another machine (set
`RSREWIND_DATA_DIR` to point rsRewind at a non-default location, including another drive). All
database records referencing media use paths relative to this root.

Nothing is ever uploaded anywhere. There is no network client code in the recording path at all.

## Pausing

Run `rsrewind pause` to stop capturing immediately until you resume, or `rsrewind pause
--minutes 30` to pause for a fixed window that resumes itself. `rsrewind status` always shows
whether you are currently paused — there is no way to put the recorder into a hidden or
undetectable state, by design.

## Privacy and data handling

rsRewind captures screen pixels, foreground application/window identity, and OCR text, with
built-in exclusion rules (password managers, private/incognito browser windows) that you can
extend or remove. See **[PRIVACY.md](PRIVACY.md)** for exactly what is captured, how exclusions
work, how deletion works, and — importantly — the current state of encryption at rest (none yet;
your data is protected only by normal Windows user-account file permissions).

## License

MIT. See `LICENSE`.
