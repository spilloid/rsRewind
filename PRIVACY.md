# Privacy and data handling

rsRewind records a continuous visual history of your own screens, on your own machine. This
document says exactly what is captured, what is excluded and how, how to pause or delete, and —
importantly — what protection your data does and does not currently have.

If anything here is unclear or you believe the actual implementation disagrees with this
document, treat that as a bug: `docs/standards.md` tracks this repository's commitment to keeping
docs and behavior in sync (STD-003), and a privacy-document mismatch is exactly the kind of drift
that standard exists to catch.

## Everything stays local

rsRewind has no network client code anywhere in the recording path. There is no cloud sync, no
account, no analytics/telemetry call, and no listening socket. Your data never leaves the machine
it was recorded on unless you move it yourself (copying the `%LOCALAPPDATA%\rsRewind` folder, for
backup or migration to another machine you own).

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
- **The foreground window's title and window class.**
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
or process name). Windows provides all of it, and so does KDE Plasma on Wayland, where KWin
reports every window on the current virtual desktop that is not minimized. A platform that cannot (for example a Wayland
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

These defaults are **suggestions, not a mandatory policy** — every rule can be edited or removed
in `config.toml`, and you can add your own. `config.toml` rejects unknown keys (a typo like
`excluded_proceses` fails to parse rather than silently doing nothing), specifically so a mistake
in a privacy rule is loud, not silent.

## Pausing and resuming

`rsrewind pause` stops capture immediately, until you run `rsrewind resume`. `rsrewind pause
--minutes N` pauses for a fixed window that resumes itself automatically once it expires.

**Recording state is always discoverable.** `rsrewind status` reports whether the recorder is
currently recording or paused, and there is deliberately no hidden or stealth mode — pausing is
the only way to stop capture, and it is always visible that you did it (or didn't).

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
- **Backups are not rewritten.** `backups
ecall-v{N}-*.db` copies, made before a schema
  upgrade, contain history as it was then. `forget` lists any that exist; deleting them is your
  decision. Uninstalling never removes them either.
- A failed file deletion is reported back rather than treated as fatal, so a locked or
  already-missing file doesn't abort an otherwise-successful deletion.

Retention (`apply_retention`, `storage.retention_days` / `storage.max_size_gb`) runs the same
deletion path (without a fence) oldest-first. The size cap bounds the real disk footprint:
`recall.db`, its WAL and every file under `media/`, orphans included; the database is compacted
before more history is deleted.

## Encryption — current state: none

**rsRewind does not currently encrypt anything at rest.** `recall.db`, every WebP screenshot
under `media\`, and `config.toml` (including your privacy rules) are stored as plain files.

The only protection your data has today is whatever normal **Windows user-profile ACLs** already
apply to `%LOCALAPPDATA%` — the same protection as any other file in your user profile. Concretely,
this means:

- **Anyone who can log into your Windows account can read your entire recorded history.**
- **A local administrator on the machine can read it too**, regardless of which account it was
  recorded under — this is true of essentially anything stored in a user profile on Windows, not
  specific to rsRewind, but worth stating plainly given what's being stored here.
- If the machine is shared, or if you don't fully trust everyone with administrative access to it,
  treat rsRewind's history the same way you'd treat an unencrypted folder of screenshots of
  everything you've looked at.

DPAPI-based (or equivalent) encryption at rest is planned future work (see
[Roadmap for privacy](#roadmap-for-privacy) below) — it is not implemented yet, and this document
will be updated the moment it is, per this repo's documentation-freshness commitment
(`docs/standards.md`, STD-003).

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

- **Encryption at rest**, most likely DPAPI-based (tied to your Windows user account), for
  `recall.db` and/or the media folder.
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
agent call's tool name, request id, result count and duration, never what was returned. See `docs/mcp.md`.
