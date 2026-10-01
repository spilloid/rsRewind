# Security

## Threat model: local-only

rsRewind is a local-first, single-user Windows application with no network component, no
accounts, and no server. There is no remote attack surface by design — see `CLAUDE.md`'s hard
rules ("no listening socket, no local HTTP server") and `ARCHITECTURE.md`. The realistic threats
this project cares about are local ones:

- **Another local user or process reading your recorded history.** Currently, the only
  protection is normal Windows user-profile file permissions on `%LOCALAPPDATA%\rsRewind` — there
  is **no encryption at rest yet**. See `PRIVACY.md` for the full, plain statement of this gap and
  what it means in practice (anyone who can log into your account, or any local administrator,
  can read everything rsRewind has recorded).
- **A malicious or buggy file silently escaping the data directory.** `DataDir::resolve_media`
  rejects any stored relative path that could resolve outside the data root (`..`, drive letters,
  absolute paths) — see `ARCHITECTURE.md`'s storage model.
- **A privacy rule silently not applying.** `config.toml` rejects unknown keys at parse time
  rather than ignoring a typo'd rule, and an empty/blank exclusion pattern matches nothing rather
  than matching everything — both are deliberate fail-loud choices; see `PRIVACY.md`.
- **An unsigned or tampered binary.** Release binaries and the MSI installer are intended to be
  Authenticode-signed (see `.github/workflows/release.yml` and `.github/actions/sign-release`),
  and the release pipeline is designed to fail closed rather than publish an unsigned build. As of
  this writing, **no signing identity is configured for this repository yet** — there is no signed
  release to verify. Once one exists, verify a download the same way described in
  spoolsmith's `docs/code-signing.md` ("Verifying a download" section): `Get-AuthenticodeSignature`
  against the downloaded file, and the published `SHA256SUMS.txt` against the file you downloaded.

Explicitly out of scope for this project's threat model: network-based attacks against rsRewind
itself (there is no network surface to attack), and multi-tenant/multi-user server deployment
scenarios (rsRewind is not a server).

## Reporting a vulnerability

Please open a GitHub issue on this repository, or contact the maintainer directly, describing the
issue and, if possible, steps to reproduce it. Given the local-only threat model above, most
realistic reports will concern either the privacy-exclusion logic (a rule that should have
excluded something and didn't) or the data-directory path handling (a way to make rsRewind read
or write outside `%LOCALAPPDATA%\rsRewind`) — both are treated as high-priority, since they go
directly against this project's core promise.

There is currently no dedicated security-contact email or bug-bounty program; this is a small,
early-stage open-source project (see `README.md`'s pre-release status), and reports are triaged
by whoever is maintaining it at the time.
