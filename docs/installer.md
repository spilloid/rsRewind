# MSI installer

**Status: MSI builds with WiX 5.0.2 from a signed EXE.** `installer/rsrewind.wxs` has not yet
been installed, upgraded, or uninstalled on a real machine. The install behavior below is
intended design until those checks are done.

## Toolchain

[WiX Toolset](https://wixtoolset.org/) v5, installed as a .NET global tool:

```powershell
dotnet tool install --global wix --version 5.0.2
```

Build locally (once a real `dist\rsrewind.exe` exists — see `.github/workflows/release.yml` for
how CI produces one):

```powershell
wix build installer\rsrewind.wxs -d ProductVersion=0.1.0 -d SourceDir=dist -o out\rsrewind.msi
```

`ProductVersion` and `SourceDir` both have fallback defines in the `.wxs` itself
(`0.1.0` / `dist`) so the file is at least syntactically buildable without passing them, but a
real release build should always pass both explicitly — see the release workflow.

## Install model: per-user, no admin rights

The installer targets `%LOCALAPPDATA%\Programs\rsRewind` and declares `Scope="perUser"` — it
never requests or requires administrator rights, and never writes to `Program Files` or
machine-wide registry locations. This matches the project's "no admin, no stealth, user owns
their own data" posture (see `PRIVACY.md`, `CLAUDE.md`'s hard rules): installing rsRewind should
not require elevation any more than installing a normal desktop app for yourself does.

## What it installs

- `rsrewind.exe`, `LICENSE.txt`, `README.md` under `%LOCALAPPDATA%\Programs\rsRewind`.
- A per-user Start Menu shortcut ("rsRewind"), launching `rsrewind.exe ui` — i.e. the shortcut
  opens the desktop UI, not a bare console window.
- **Optionally**, a startup registration: `HKCU\Software\Microsoft\Windows\CurrentVersion\Run\rsRewind`
  = `"<install dir>\rsrewind.exe" start`, so the recorder starts automatically at sign-in.

The startup registration is modeled as a **separate MSI feature** (`StartAtLogin`, `Level="1000"`
— MSI's standard way of saying "present in the feature tree, not selected unless asked for") so
it is genuinely optional and independently toggleable, rather than always-on. No installer UI
currently exposes a checkbox for it (the `.wxs` doesn't define a UI sequence yet — see
[Known gaps](#known-gaps) below); until one exists, it can be selected explicitly:

```powershell
msiexec /i rsrewind.msi ADDLOCAL=Application,StartAtLogin
```

## Upgrades

`<MajorUpgrade>` handles version detection: installing a newer version over an older one upgrades
in place (old files removed, new files installed); installing an older version over a newer one
is refused outright with a clear error, rather than silently downgrading files underneath a
database schema that may have already migrated forward (see `ARCHITECTURE.md`'s "refuse to open a
database whose schema version is newer than the binary knows" rule — a silent downgrade would be
exactly the scenario that rule exists to catch, approached from the installer side instead of the
database side).

The `UpgradeCode` in `installer/rsrewind.wxs` (`269482AF-5A5D-4F35-B1B2-76EE09A72583`) is fixed
and must never change across releases — it is what lets Windows Installer recognize "this is the
same product, a different version" from one release to the next. It was generated once (a random
UUIDv4) when this file was written and should be treated as permanent product identity from here
on.

## Uninstall never deletes recording history

Uninstalling removes exactly what was installed: the program files under
`%LOCALAPPDATA%\Programs\rsRewind`, the Start Menu shortcut, and the startup registration if it
was enabled. **It does not touch `%LOCALAPPDATA%\rsRewind`** (the data directory — `config.toml`,
`recall.db`, `media\`, `logs\`, `backups\`, `models\`), because that folder is not part of the
MSI's installed payload at all; the installer has no component that writes there and therefore
nothing to remove there either, by construction rather than by an explicit "keep this" rule that
could be gotten wrong.

Deleting recorded history is a deliberately separate, explicit, documented action — a planned
`rsrewind purge` CLI command (not yet implemented), not an MSI uninstall checkbox. The reasoning:
an uninstall is often "I want to stop running this for now" or "I'm about to reinstall a newer
build," neither of which implies "also destroy my history" — conflating the two would make
uninstall a silent-data-loss trap. See `PRIVACY.md` and `CLAUDE.md`'s hard rules ("uninstall never
silently deletes recording history").

## Windows App Runtime 2.4

The WinUI 3 UI (`rsrewind-ui`, via `windows-reactor`) requires the Windows App Runtime 2.4
framework package (`Microsoft.WindowsAppRuntime.2`) to be present at runtime. **This is not yet
chained, bundled, or checked by the current `.wxs`.** Two options are under consideration for a
later revision, not yet decided between:

1. **Bundle the redistributable via a WiX Burn bundle EXE** — a `Bundle` project (separate from
   this `.wxs`'s `Package`) that chains the Windows App Runtime installer before (or alongside)
   the rsRewind MSI, so a user installing rsRewind never has to separately find and install the
   runtime. This is the more "just works" option but adds a second installer artifact (a bundle
   EXE wrapping the MSI) and a dependency on redistributing Microsoft's installer, which needs its
   own licensing/distribution check before committing to it.
2. **Detect and prompt at launch instead of at install time** — per `docs/mvp-contract.md`'s
   `rsrewind-ui` section, `windows-reactor`'s own bootstrap already shows an install prompt when
   the runtime is missing, and critically, **the CLI keeps working regardless** (the UI boundary
   means a missing runtime only affects `rsrewind ui`, never capture, search, or any other
   subcommand). This option does less work in the installer and trusts `windows-reactor`'s
   existing missing-runtime handling instead of duplicating it.

Whichever is chosen, the CLI's independence from the UI's runtime requirement (see
`ARCHITECTURE.md`'s UI boundary section) means this is purely a UI-experience decision, not a
blocker for the recorder itself working.

## Known gaps

Tracked here explicitly rather than left implicit:

- **Never built.** No `wix build` has been run against `installer/rsrewind.wxs`. It may contain
  syntax issues that only a real build would surface.
- **Never installed, upgraded, or uninstalled** on a real Windows machine. The upgrade/downgrade
  behavior, the Start Menu shortcut, and the startup registration are all unverified.
- **No product icon.** The `.wxs` does not currently reference an `.ico` file (none exists yet in
  the repository) for `ARPPRODUCTICON` or the Start Menu shortcut's icon; it will use Windows'
  generic executable icon until one is added.
- **No installer UI sequence.** There is no WixUI dialog set wired in yet, so a GUI `msiexec`
  install currently runs with Windows Installer's bare default UI (if any) rather than a
  purpose-built wizard; the `StartAtLogin` feature toggle described above requires command-line
  `ADDLOCAL`/`REMOVE` until a real UI sequence exists.
- **No Windows App Runtime chaining**, per the section above.
- **No released MSI has been signed yet.** The `release` GitHub environment now has a
  repository-specific signing identity. `.github/workflows/release.yml` signs and verifies the
  MSI before publishing, and fails closed if signing does not work.
