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
  `Dashlane.exe`, `CredentialUIBroker.exe`, `LogonUI.exe`, `consent.exe`.
- **`excluded_title_patterns`** — case-insensitive glob patterns against the window title (`*`
  matches any run of characters, `?` matches exactly one — no regex, so a rule you write is a
  rule you can predict). The suggested defaults catch private-browsing windows by their title:
  `*InPrivate*`, `*Incognito*`, `*Private Browsing*`.

**A monitor is skipped entirely — no pixels stored at all — when *any* visible window on that
monitor matches an exclusion rule**, not only the foreground window. This is intentionally
conservative: if a password manager is open in a background window on a monitor you're otherwise
using for something else, that whole monitor's capture is skipped for that tick rather than
risking a screenshot that happens to include it. The matching rule name is recorded (e.g.
`process:1Password.exe`) as a `privacy_skip` event, with no pixels attached — the fact that
capture was skipped, and why, is itself visible in your history, but what was on screen is not.

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

Deleting a time range (`delete_range`, exposed via the CLI as a delete-recent action) removes, in
one transaction: the `events` rows in that range, any `visual_states` no longer referenced by any
remaining event (and their `ocr_blocks` and full-text index rows), and finally the backing WebP
image files on disk. A visual state still referenced by an event outside the deleted range (for
example, an unchanged screen whose observation span straddles the boundary) is preserved, not
deleted out from under a surviving row. A failed file deletion is reported back rather than
treated as fatal, so a locked or already-missing file doesn't abort an otherwise-successful
deletion of everything else.

Retention (`apply_retention`, `storage.retention_days` / `storage.max_size_gb`) runs the same
underlying deletion path automatically, oldest-first, on a schedule — see `ARCHITECTURE.md`.

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
- **Secure-delete limits on SSDs** — `delete_range` and retention currently delete files through
  the normal filesystem delete path. On an SSD, a normal delete does not guarantee the underlying
  flash cells are immediately or securely overwritten (wear-leveling and TRIM behavior are outside
  rsRewind's control); a genuinely secure-delete guarantee is not currently made and is tracked
  here as unresolved, not silently assumed.
