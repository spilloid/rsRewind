# Privacy and data handling

rsRewind records a continuous visual history of your own screens, on your own machine. This
document says exactly what is captured, what is excluded and how, how to pause or delete, and —
importantly — what protection your data does and does not currently have.

If anything here is unclear or you believe the actual implementation disagrees with this
document, treat that as a bug: `docs/standards.md` tracks this repository's commitment to keeping
docs and behavior in sync (STD-003), and a privacy-document mismatch is exactly the kind of drift
that standard exists to catch.

## Local by default

rsRewind has no network client code anywhere in the recording path. There is no cloud sync, no
account, no analytics/telemetry call, and no listening socket. History stays in your local store unless you export or copy it, or enable optional MCP access for an assistant. Returned assistant content may reach its provider; see the MCP section below. Run `rsrewind data-dir` to locate your store on any platform.

> **Early release:** known privacy enforcement gaps remain in [issue #12](https://github.com/spilloid/rsRewind/issues/12), on every platform. The rules below describe intended behavior; use a session you are comfortable recording.

## What is captured

When the recorder is active and a monitor is not excluded (see below), approximately once per
second per monitor (configurable, `capture.fps_candidate`) it checks whether that monitor's
content has visibly changed since the last stored state. **Pixels are only written to disk when
a meaningful visual change is detected** — most candidate ticks are discarded without ever being
stored, because the screen hasn't changed enough to be worth a new entry.

For each stored visual state:

- **Screen pixels** of the monitor, as a WebP image (`capture.image_quality`, default 80).
- **The foreground process's name and, where available, its full executable path** (reading an
  elevated process's path can fail; in that case the process name is still recorded, but the path
  is left empty — this never blocks capture).
- **The foreground window's title and window class**, where available. On macOS the recorder does not attribute a foreground application or window to a moment (CoreGraphics window ordering does not prove focus, and it will not guess), so Mac moments carry no application or window name; the visible-window list is still used for exclusions.
- **A timestamp** (UTC, millisecond precision) and which **monitor** (device name, resolution,
  position, DPI) the state came from.
- **OCR text** recognized from the image, if OCR is enabled (`ocr.enabled`, on by default), stored
  both as per-line blocks with bounding boxes and as a full-text search index entry.

None of your keystrokes, clipboard contents, audio, or network traffic are ever captured. The
recorder only ever looks at what is already visible on screen and metadata about which window
owns it.

## What is excluded, and how

Before anything is persisted, every monitor's frame is checked against your configured privacy
rules (`[privacy]` in `config.toml`, `PrivacyPolicy` in `rsrewind-core`):

- **`excluded_processes`** — exact, case-insensitive executable-name matches (e.g.
  `1Password.exe`). The suggested defaults cover common password managers and Windows' own
  credential UI: `1Password.exe`, `Bitwarden.exe`, `KeePass.exe`, `KeePassXC.exe`,
  `Dashlane.exe`, `CredentialUIBroker.exe`, `LogonUI.exe`, `consent.exe`, and on Linux (where the
  name is the executable's file name) `keepassxc`, `1password`, `bitwarden`,
  `polkit-kde-authentication-agent-1`, `pinentry-qt`, `pinentry-qt5`, `ksecretd`.
- **`excluded_title_patterns`** — case-insensitive glob patterns against the window title (`*`
  matches any run of characters, `?` matches exactly one — no regex, so a rule you write is a
  rule you can predict). The suggested defaults catch private-browsing windows by their title:
  `*InPrivate*`, `*Incognito*`, `*Private Browsing*`.

**A monitor is skipped entirely — no pixels stored at all — when *any* visible window on that
monitor matches an exclusion rule**, not only the foreground window. This is intentionally
conservative: if a password manager is open in a background window on a monitor you're otherwise
using for something else, that whole monitor's capture is skipped for that tick rather than
risking a screenshot that happens to include it. A skipped tick stores no pixels and no text. As of 2026-10-08 the recorder only
**counts** skips (visible in `rsrewind status`); it does not yet write a `privacy_skip` event naming the
matching rule to your history, although the event kind exists for it. Writing that event (rule name only,
never what matched) is open work.

**Where the rules cannot be enforced, rsRewind does not record.** Enforcing "any visible window"
needs the list of every visible window with its position (and, for each kind of rule, its title
or process name). Windows provides all of it, and so do KDE Plasma on Wayland, where KWin
reports every window on the current virtual desktop that is not minimized, and macOS, where the
window list comes from CoreGraphics once Screen Recording is granted. A platform that cannot (for example a Wayland
desktop without a window-list protocol) makes the recorder **refuse to start** unless you set
`unenforced_ok = true` under `[privacy]`. If you do, `rsrewind status` and `rsrewind doctor`
say **"privacy rules NOT enforced"** for as long as the newest recording session ran that way,
and history exported from such a session does not claim window titles. The key defaults to
`false` and is not written into a new `config.toml`. Whatever the platform, information the
recorder cannot get on a given tick (a failed window enumeration, unknown idle time) means
nothing is stored for that tick. On Plasma a locked screen counts as idle, so the lock screen is never
recorded.

**Linux screenshot permission.** KWin lets a program take screenshots only if a `.desktop` file names
it. The recorder writes `~/.local/share/applications/rsrewind-recorder.desktop` (naming its own
path) each time it starts. That file is a standing permission for that program path; delete it to
withdraw it.

**macOS: untitled windows and unresolvable owners (since 2026-10-15, issue #33).** macOS does not
report a title for many windows (system overlays, untitled app windows) and some window owners
cannot be resolved to an executable. Such windows no longer make the whole window list "unknown":
an untitled window is checked with an empty title (as on Windows), and an unresolvable owner is
named by the owner name macOS reports. **A window-title rule therefore cannot match a Mac window
whose title macOS does not report**; application rules still apply to it, and on macOS are checked
against both the executable name and the owner's display name. If title rules exist and *no*
application window on screen reports a title (titles are being withheld), or a window has no owner
name at all while application rules exist, or a window's position cannot be read, the list is
still treated as unknown and nothing is stored. Whenever title rules exist and application windows
report no title, `rsrewind status` prints "Title rules could not be checked on N tick(s)" and
`rsrewind doctor` warns. **Open decision for the maintainer:** the Mac backend still declares that
it provides window titles, so the recorder starts with title rules without `unenforced_ok`, and
its sessions record title rules as enforced even though they cannot match untitled windows.
(Correction 2026-10-16: exports made on a Mac, like those made on Linux, carry no capability tags
at all, because the export command's list of tags is empty outside Windows; earlier text said Mac
exports carry `window_titles`.) The details, and the stricter alternative, are in
[docs/macos.md](docs/macos.md#window-list-rules-and-what-they-relax). When the recorder runs but
stores nothing, `rsrewind status` and the window say so and why.

**macOS lock screen.** macOS has no documented "screen is locked" call for this kind of program,
so the recorder treats the session as unknown, and stores nothing, from several signals: the
session not being on the console, an undocumented lock flag (`CGSSessionScreenIsLocked`), any
window of the system's `loginwindow` process on screen, or an unidentifiable window covering most
of a display. If a future macOS changes all of these, a lock screen could be recorded.

The suggested patterns are Windows and Linux process names; on macOS add the names of the apps you use (as `rsrewind doctor` and Activity Monitor report them) and check a rule with synthetic content before relying on it. Editing a process or title list replaces that list, so preserve any defaults you still want.

These defaults are **suggestions, not a mandatory policy** — every rule can be edited or removed
in `config.toml`, and you can add your own. `config.toml` rejects unknown keys (a typo like
`excluded_proceses` fails to parse rather than silently doing nothing), specifically so a mistake
in a privacy rule is loud, not silent.

## When recording starts

Opening rsRewind (from search, the Start menu, the application menu or the Mac Applications folder) and
signing in to a computer where the Windows installer's start-at-login entry or `rsrewind tray --autostart on`
is set both start the recorder, so that history exists when you need it. The tray icon (red while
recording, green when not) is the always-visible sign of it, and `rsrewind status` reports it. To stop
recording from starting by itself set `record_on_launch = false` under `[capture]` in `config.toml`; the
tray icon and window still open, and recording starts only when you press Start. Choosing **Quit** from
the tray or menu-bar icon stops the recorder before the icon goes away, so recording does not carry on
with nothing on screen to show it. Pausing and the privacy rules below apply either way.

## Pausing and resuming

`rsrewind pause` requests a pause until you run `rsrewind resume`. Check `rsrewind status` after requesting it; known acknowledgement/enforcement gaps are tracked in [issue #12](https://github.com/spilloid/rsRewind/issues/12). `rsrewind pause
--minutes N` pauses for a fixed window that resumes itself automatically once it expires.

**Recording state is always discoverable.** `rsrewind status` reports whether the recorder is
currently recording or paused, and there is deliberately no hidden or stealth mode — pause and stop controls are
available, and it is always visible that you did it (or didn't).

## How deletion works

Deleting a time range (`delete_range`, exposed as `rsrewind forget`) runs, in one transaction: it
records a durable **deletion fence** for the range, deletes the `events` rows in it, any
`visual_states` no longer referenced by any remaining event (and their `ocr_blocks` and full-text
index rows), and any `windows` / `applications` rows (window titles, process names) that no
remaining event references. After the commit it deletes the backing WebP files, then sweeps
*orphan* image files — files with no database row, such as the leftovers of a crash between a file
write and its row insert, recognised by the capture time in their file name — and stale temp files
from the same range. Finally it truncates the SQLite write-ahead log.

- **Fence.** Until a fence is pruned (retention does that once it passes the retention horizon),
  the store refuses to insert a screenshot or observation whose capture time falls inside the
  range (`StorageError::Fenced`). A frame that was queued or still being encoded when you ran
  `forget` therefore cannot bring the forgotten moment back.
- **Stale OCR.** OCR results are saved against the screenshot's id *and* file path, and ids are
  never reused, so a result computed for a deleted screenshot cannot attach to another one.
- **Physical erasure inside `recall.db`.** `secure_delete` and FTS5's `secure-delete` are on, so
  deleted pages and index entries are zeroed rather than left in free pages, and the WAL (which
  keeps earlier page versions) is checkpoint-truncated. If another process is reading at that
  moment the truncation is skipped, `forget` says so, and the next checkpoint finishes it.
- **Backups are not rewritten.** `backups/recall-v{N}-*.db` copies, made before a schema
  upgrade, contain history as it was then. `forget` lists any that exist; deleting them is your
  decision. Uninstalling never removes them either.
- A failed file deletion is reported back rather than treated as fatal, so a locked or
  already-missing file doesn't abort an otherwise-successful deletion.

Retention (`apply_retention`, `storage.retention_days` / `storage.max_size_gb`) runs the same
deletion path (without a fence) oldest-first. The size cap bounds the real disk footprint:
`recall.db`, its WAL and every file under `media/`, orphans included; the database is compacted
before more history is deleted. It is a **soft** cap: retention runs about a minute after the
recorder starts and then hourly, so the folder can exceed it by up to an hour of capture. It does
**not** count `backups/`, `exports/`, `outbox/`, `sources/` (history imported from other machines)
or `logs/` (daily files, the newest 14 kept). `rsrewind doctor` prints the counted footprint against
the limit. When the disk is full the recorder stores nothing, and `status`, the window and the tray
say "saving to disk is failing" until space is freed; rsRewind does not itself stop short of filling
the disk.

## Encryption — current state: none

**rsRewind does not currently encrypt anything at rest.** `recall.db`, every WebP screenshot
under `media\`, and `config.toml` (including your privacy rules) are stored as plain files.

Protection comes from normal OS account permissions and any disk encryption you configure
(BitLocker, FileVault, LUKS). On Windows the default folder under `%LOCALAPPDATA%` inherits the
user profile's ACL (you, SYSTEM and Administrators); **a folder you choose with `--data-dir` or
`RSREWIND_DATA_DIR` inherits the ACL of wherever you put it**, which outside your profile (for
example `C:\rsrewind` or a second drive) often lets every user of the machine read it; rsRewind does
not tighten it. On Linux and macOS rsRewind sets the data folder to `0700` (owner only) whenever the
recorder starts. rsRewind itself adds no encryption. Concretely, this means:

- **Anyone who can use your OS account can read your recorded history.**
- **A local administrator on the machine can read it too**, regardless of which account it was
  recorded under — this is true of essentially anything stored in a user profile on Windows, not
  specific to rsRewind, but worth stating plainly given what's being stored here.
- If the machine is shared, or if you don't fully trust everyone with administrative access to it,
  treat rsRewind's history the same way you'd treat an unencrypted folder of screenshots of
  everything you've looked at.
- **Any program running as you can read it**, including malware, sync clients and an AI assistant
  or coding agent that can read files or run commands (whatever the MCP settings below say).
- **Backups copy it.** On a Mac, Time Machine backs up `~/Library/Application Support` by
  default, so recorded history lands on the backup disk (encrypted only if that backup is). On
  Windows `AppData\Local` is not roamed between machines, but backup and endpoint tools your
  organisation runs may copy or scan it. Exclude the data folder from backups if that matters to you.
- **Screens are recorded as they are.** There is no detection of password fields, one-time codes or
  customer records on screen; only the process and window-title rules below, pause, and forget
  keep such content out. A password you type is normally masked on screen, but a code or a secret
  shown in clear text is captured and its text made searchable.

Encryption at rest is not implemented. The current design (SQLCipher for the database,
authenticated encryption for screenshots, OS-protected keys, and a hard "no usability cost" ship
gate) is an exploration in [docs/design/encryption-at-rest.md](docs/design/encryption-at-rest.md);
nothing of it is built. This document will be updated the moment it is, per this repo's
documentation-freshness commitment (`docs/standards.md`, STD-003).

## Marks

A mark (`rsrewind mark`, or M in the window) is a moment you chose plus an optional label you typed.
It is stored in `recall.db` with the rest of your history, unencrypted like it. Marks are never put
into segment files, so they never reach another machine through `rsrewind export`. `forget` deletes
the marks inside the forgotten range, age retention deletes marks older than the retention horizon,
and a mark cannot be added inside forgotten time. Size retention keeps marks (they are a few bytes
each). Labels are never logged. The window hands a label to `rsrewind mark` on standard input
(`--label-stdin`), not as a command-line argument, so it does not appear in the process list (`ps`,
Task Manager) while the command runs; `rsrewind mark add --label TEXT` typed by hand does put it there,
like any argument.

## Saving pictures and clips from the window

The window can copy a moment's picture to the clipboard and save pictures and silent clips (PNG,
GIF, and WebM when an `ffmpeg` executable is installed) to `<data folder>/exports/`. **An export is a
copy that leaves rsRewind's history**: `forget`, retention and uninstall do not delete exported files
or clipboard contents, and other programs can read them. The window says so before writing a file.
Exports happen only when you ask, in the window's own process; the recorder is not involved, nothing
is sent anywhere, and clips contain no audio (rsRewind records none). A moment that was forgotten
after you picked it is not exported: the export stops and says the picture is gone. While an export
runs, its unfinished file (`.rsrewind-….part`) and, for WebM, a scratch folder of full-size frames
(`.rsrewind-….frames`) sit hidden in `exports/`; they are removed when the export finishes, fails or
is cancelled. **If the window crashes or is killed mid-export, they stay on disk** until the window is
opened again (or another export starts), which removes any whose export is no longer running. Only
names of exactly that shape are removed; nothing else in `exports/` is touched.

## Exporting history to another machine

`rsrewind export` writes segment files containing screenshots, window titles, process names and
recognized text for settled history, to `outbox/` (or wherever `--out` points). Treat those files,
the folder they are moved through, and a central instance's `sources/` folder as exactly as
sensitive as the recorder's own data: there is no encryption at rest and, in this stage, no
authentication of who produced a segment (a hash detects corruption, not forgery). Exporting is
always an explicit command; nothing is sent anywhere by rsRewind, and the recorder never exports
on its own.

Deleting history with `forget` on the recording machine does **not** remove copies already exported
or imported elsewhere. It does delete sealed segments in `outbox/` that cover the forgotten time, and tells you how many of the deleted moments had already been sealed; delete delivered copies yourself. Forgetting on a
central instance's replica is fenced, so re-delivering the same segments does not bring it back.
History that is forgotten before it has settled (about five minutes by default) is never exported.

## Logs

Application logs (`%LOCALAPPDATA%\rsRewind\logs\`) do not contain captured screen text or window
titles by default (`logging.log_captured_content`, off by default). Logs record ids, counts,
durations, and privacy *rule names* (e.g. that `process:1Password.exe` matched something — which
is your own configuration, not captured content), never the OCR text or titles that triggered a
privacy skip or a successful capture. This is a hard rule for anyone working on this codebase —
see `CLAUDE.md`.

## No telemetry, no network, no listening socket

Worth restating directly: rsRewind does not phone home in any way, does not check for updates
over the network from inside the recorder or CLI, and does not open any listening socket or local
HTTP server. Control and status between `rsrewind` CLI invocations and the running daemon go
through SQLite tables on disk (see `ARCHITECTURE.md`'s process model), not through any network
primitive.

## Roadmap for privacy

Not implemented yet; tracked here so the gap is explicit rather than silent:

- **Encryption at rest** for `recall.db` and the screenshots, with keys held by the OS key store
  (exploration: `docs/design/encryption-at-rest.md`).
- **Sensitive-content pauses**: pausing automatically while a password or one-time-code field has
  focus, or while an app from a "sensitive" list is in front (today only whole-monitor exclusion by
  process name or window title exists).
- **A free-space floor**: stop writing before the disk is full instead of when it is.
- **Private-browsing heuristics beyond title matching** — the current `*InPrivate*`/`*Incognito*`
  rules match on window title text, which is simple and predictable but not foolproof (a browser
  that doesn't put that text in its title window, or a renamed/localized build, would not match).
- **A delete-recent UI** in the WinUI 3 desktop app, not just the CLI/storage-layer capability.
- **Secure-delete limits on SSDs** — inside `recall.db` deleted content is overwritten (see "How
  deletion works"), but `delete_range` and retention delete screenshot files through
  the normal filesystem delete path. On an SSD, a normal delete does not guarantee the underlying
  flash cells are immediately or securely overwritten (wear-leveling and TRIM behavior are outside
  rsRewind's control); a genuinely secure-delete guarantee is not currently made and is tracked
  here as unresolved, not silently assumed.

## AI assistants (MCP)

`rsrewind mcp` lets an AI assistant you configure read your history. **It is off until you run `rsrewind mcp
--enable`**, because most assistants send what they read to their provider's servers: that is the only way rsRewind
history leaves your computer, and only for what the assistant asks for. When on, agents see recognized text, app and
window names from the last 30 days by default; screenshots only if you set `allow_screenshots = true`; only the
machines you list under `[mcp] sources`. Agents cannot resume recording, forget, export or import. rsRewind logs each
agent call's tool name, a call number it generates itself, the decision, result count and duration; never
the client's request id, its arguments, or what was returned. These limits govern only what `rsrewind mcp`
answers: an assistant that can also read files or run commands as you (a coding agent, for example) can read the
data folder directly, screenshots included, whatever `[mcp]` says. See `docs/mcp.md`.

## macOS (2026-10-10; refreshed 2026-10-11)

macOS screen-recording authorization is required; the application cannot bypass a denial, and without it nothing is captured. The backend follows the same fail-closed platform contract as Windows and Plasma: a tick with no frame, no window list, no idle value, or a session that is not the logged-in console session stores nothing. Text recognition is Apple Vision, in memory, on device. No foreground application is attributed to Mac moments (see above). The Mac port does not close the recorder findings in issue #12, which apply to every platform. The maintainer states (2026-10-11) that macOS is as stable as the other platforms; that is an owner attestation about daily use, not an independent privacy review, and the real-Mac checklist in `docs/macos.md` is what a release should be checked against.
