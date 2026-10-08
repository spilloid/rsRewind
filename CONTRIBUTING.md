# Contributing

## Build and test commands

```powershell
cargo build --workspace
cargo test --workspace
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

If `cargo`/`rustc` aren't on your shell's `PATH` (installed via `rustup` but not exported),
prefix with the toolchain's `bin` directory — see `CLAUDE.md` for the exact prefix used on the
maintainer's development machine.

Target is `x86_64-pc-windows-msvc` (pinned in `rust-toolchain.toml`, which also pins the stable
channel and the `rustfmt`/`clippy` components). Building needs MSVC Build Tools and the Windows 11
SDK; see `README.md`'s build requirements.

## The bar before a change ships

All four commands above must pass, with zero warnings from `fmt` and `clippy`, for every crate
your change touches — not just the crate you think you changed, since a shared type in
`rsrewind-core` can ripple. Report the exact commands you ran and the output tails when you hand
off work for review; "it builds" is not the same claim as "fmt, clippy and the full test suite
pass," and reviewers should be able to tell which one you're making.

## No-network rule

No network code of any kind, anywhere in this workspace: no HTTP client, no socket that listens,
no telemetry call, no "phone home to check for updates." This is an architectural invariant (see
`ARCHITECTURE.md`'s dependency policy and `CLAUDE.md`'s hard rules), not a style preference — a
PR that adds any networking capability, even an opt-in or clearly-labeled one, is out of scope for
this repository and should be raised as a design discussion before any code is written, not
reviewed as an ordinary change.

## No-unwrap rule

No `unwrap()` or `expect()` in runtime code. Tests may use `?` with `Box<dyn Error>`. A genuinely
justified `expect()` on a true invariant (something that cannot fail given code already checked
earlier in the same function, for example) needs a comment explaining *why* it cannot fail — not
just that it's convenient. This is enforced by the workspace lint configuration
(`clippy::unwrap_used`, `clippy::expect_used` as warnings in `Cargo.toml`'s
`[workspace.lints.clippy]`), and a warning here should be treated as something to fix, not
something to suppress.

Library crates (`rsrewind-core`, `rsrewind-capture`, `rsrewind-storage`, `rsrewind-ocr`,
`rsrewind-query`) return typed errors via `thiserror`. `anyhow` is for the binary-level crates
(`rsrewind-cli`, `rsrewind-daemon`) only, where the error is being reported to a human or logged,
not matched on by another crate.

## Logging rule

`tracing` is the logging framework. **Never log OCR text, window titles, or any other captured
screen content at `info` level or above.** Log ids, counts, durations, and privacy *rule names*
(not what the rule matched against) instead. This is not a style preference — logs are not meant
to become a second, less-protected copy of someone's recorded history. See `PRIVACY.md`'s logging
section and `config.toml`'s `logging.log_captured_content` (off by default, and even when on, that
is a local debugging opt-in, not something this project's own code should rely on being set).

## No SQL outside storage/query

`rsrewind-storage` owns all writes to the database and its schema. `rsrewind-query` owns all
reads, through a read-only connection. No other crate — especially not `rsrewind-cli` or
`rsrewind-ui` — should construct SQL or open its own connection to `recall.db`. If a CLI command or
UI view needs a new way to read the data, add a typed function to `rsrewind-query` and call that;
see `ARCHITECTURE.md`'s query architecture section for why (principally: the FTS5 query-injection
surface that `parse_query` exists specifically to close, which only has one implementation to get
right if it's the only path in).

## UI never owns capture, storage, or OCR

`rsrewind-ui` depends on `rsrewind-core` and `rsrewind-query` only. It must never depend on
`rsrewind-capture`, `rsrewind-storage`, `rsrewind-ocr`, or `rsrewind-daemon`, and must never run on
the same process as the recorder (see `ARCHITECTURE.md`'s process model and UI boundary sections —
this is also why the UI can safely crash without affecting recording). A change that makes the UI
crate depend on one of those crates, even transitively, is a regression of a load-bearing
architectural boundary, not a convenience.

## Commit and PR conventions

- Explicit file paths in `git add`; never `git add -A` or `git add .` — a change should name what
  it touches.
- Commit messages and PR descriptions explain *why*, not just what — especially for anything
  touching Windows API quirks, concurrency, or an invariant that isn't obvious from the diff
  alone.
- Fill out every section of `.github/PULL_REQUEST_TEMPLATE.md`, including the synthetic-data
  clause: screenshots, fixtures, and test data in a PR must not contain real personal or customer
  content, given what this product records.
- A behavior change updates the relevant docs (`README.md`, `ARCHITECTURE.md`, `PRIVACY.md`,
  `ROADMAP.md`) in the same change, not as a follow-up — see `docs/standards.md`'s STD-003
  adoption.

## STD-001: orchestrator-driven, adversarially-reviewed development

This repository follows the company's STD-001 pattern (`corporate-strategy/standards/STD-001`,
and see `docs/dev-process.md` for this repo's own log and routing table):

1. **An orchestrator writes the contract first** — the architecture/spec and, where practical,
   the tests — before dispatching implementation work. `docs/mvp-contract.md` is this repository's
   example: written before the first vertical slice was implemented, so every implementer works
   against the same spec and every reviewer grades against it.
2. **Implementation is routed by risk, not uniformly.** Mechanical, tightly-specified work goes to
   a cheaper/faster tier; architecture, security, concurrency, migrations, and anything touching
   `unsafe` goes to the most capable tier regardless of how small the diff looks. See `CLAUDE.md`
   for this repo's specific routing table.
3. **Every nontrivial change gets adversarial review** — a second pass whose job is to find
   problems, not to confirm the first pass was fine. A review finding can be right about the
   problem and wrong about the fix; the orchestrator adjudicates the problem and chooses the fix,
   rather than blindly applying whatever the reviewer proposed.
4. **Reproduce before trusting, in both directions.** A finding is reproduced before it's accepted
   as real; a fix is reproduced (tests actually run, not assumed to pass) before it's marked done.
   A dispatched agent's own summary of what it did is a claim, not proof — the orchestrator
   verifies the actual tool calls and output, especially for anything touching concurrency,
   lifetime, or startup/shutdown ordering, where a diff read alone can miss a live-only bug.

See `docs/dev-process.md` for the log of actual rounds (unit, risk tier, model, outcome) this
repository accrues under this process.

## Release runbook

Shipping includes a documentation step, not a follow-up (STD-003 rule 3). In order:

1. **Version.** Bump `workspace.package.version` in `Cargo.toml` and refresh `Cargo.lock`.
2. **Changelog.** Move `[Unreleased]` into a dated `## [X.Y.Z] - YYYY-MM-DD` section. It becomes the
   GitHub release notes, so it must stand alone for someone who has not read the repository:
   a one-paragraph summary, `### Added` / `### Changed` / `### Fixed` / `### Security` as they apply,
   and **`### Known issues`** and **`### Upgrade notes`** (write "none" if none). Anything the release
   does not yet do safely goes in a warning block at the top, as 0.0.1 and 0.0.2 do.
3. **User-facing docs describe the current app.** Update the README's `**Current release: vX.Y.Z**`
   line and its CLI table, the website under `site/` (version, download links, any changed
   behavior, and screenshots if the UI visibly changed; screenshots must be synthetic), `PRIVACY.md`
   if recording, retention, deletion or export behavior changed, and `ARCHITECTURE.md` /
   `docs/design/*` if the design changed.
4. **Mechanical check.** `scripts/check-docs.sh` must pass: it compares the version in `Cargo.toml`,
   `CHANGELOG.md`, `README.md` and `site/`, rejects links to other versions, rejects third-party
   requests on the site, and checks referenced images exist. CI runs it on every PR.
5. **Standards and quarantine.** Reconcile `docs/standards.md`, and re-date or retire the items in
   CLAUDE.md's "Things to re-verify before trusting them".
6. **Gate.** PR green on Windows CI (`fmt`, `clippy -D warnings`, `test`, `build`). Run the Release
   workflow from `main` with `dry_run=true` when signing, packaging or the notes template changed.
7. **Tag.** Merge, then tag `vX.Y.Z` on the merge commit. The workflow tests, builds, signs the EXE
   and MSI, assembles the ZIP, writes `SHA256SUMS.txt`, and publishes a **prerelease** only if every
   signature and checksum verifies.
8. **Verify the published release** as a user would: download the assets, `sha256sum -c
   SHA256SUMS.txt`, `Get-AuthenticodeSignature` on the EXE and MSI, install the MSI on a real
   Windows machine, run `rsrewind --version` and `rsrewind doctor` in an interactive session.
9. **Promote** the prerelease to Latest only after step 8, and only when the release notes' Known
   issues are acceptable to ship under that label. Then confirm the Pages deploy shows the new version.
