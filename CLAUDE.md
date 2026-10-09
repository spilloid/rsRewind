# CLAUDE.md

Guidance for AI agents (Claude subagents, Codex/Astra, or anyone else) working in this
repository.

## Mission

rsRewind is a Windows-native, local-first screen-history recorder, written in Rust, with no
cloud component. It watches the screen, keeps a searchable visual/text record of what changed,
and gives the user that history back through a CLI and a native desktop UI — entirely on their
own machine. See `README.md` for the product pitch, `ARCHITECTURE.md` for how it's built, and
`docs/mvp-contract.md` for the contract the first vertical slice is implemented against.

## Hard rules

These are architectural invariants, not style preferences. A change that violates one of these is
out of scope for an ordinary PR and needs a design discussion first, not a code review comment.

**The "No" list** — none of the following belong in this repository, ever:
- No Tauri, Electron, or any browser runtime.
- No Python, no Docker.
- No required network service, no cloud dependency.
- Replication between installations is **file-based** (`rsrewind export` / `import` of `.rsseg`
  segments; see `docs/design/distributed.md`). Moving the files is the operator's job. A built-in
  transport would need an explicit, narrowly worded amendment of this list; do not add one
  incidentally.
- No listening socket, no local HTTP server (control/status go through SQLite tables — see
  `ARCHITECTURE.md`'s process model — never a pipe or socket).
- No telemetry, no "phone home," no network client code anywhere in the recording path.

**Privacy rules:**
- A privacy decision (`PrivacyPolicy::evaluate`) is checked **before** anything is persisted —
  never capture first and filter afterward.
- A monitor is skipped entirely when *any* visible window on it matches an exclusion rule, not
  just the foreground window.
- Where a platform cannot enforce that (its `Capabilities` lack the window list, or a field a rule
  needs), the recorder refuses to start unless the user sets `privacy.unenforced_ok = true`, and
  `status`/`doctor` then say "privacy rules NOT enforced". Unknown information (a failed window
  enumeration, unknown idle time) means do not record. Never fake a capability.
- The recorder's capture state is always discoverable (`rsrewind status`); there is no hidden or
  stealth mode, under any flag, ever.
- Uninstalling never silently deletes recording history. Deleting history is always a separate,
  explicit, documented action (see `docs/installer.md`).
- See `PRIVACY.md` for the full policy this code implements.

**Logging rule:** never log OCR text, window titles, or any other captured screen content at
`info` level or above. Log ids, counts, durations, and privacy *rule names* (not what a rule
matched against). See `PRIVACY.md`'s logging section.

**No SQL outside storage/query:** `rsrewind-storage` owns all writes and the schema;
`rsrewind-query` owns all reads, through a read-only connection. No other crate opens its own
connection to `recall.db` or constructs SQL — not the CLI, not the UI, not the daemon directly.

**UI never owns capture, storage, or OCR:** `rsrewind-ui` depends only on `rsrewind-core` and
`rsrewind-query`, runs as its own OS process, and must never depend on `rsrewind-capture`,
`rsrewind-storage`, `rsrewind-ocr`, or `rsrewind-daemon`, even transitively. This is what makes
"the UI crashing cannot stop recording" true by construction rather than by convention.

**Single exe:** one executable, `rsrewind.exe`, with subcommands (`daemon`, `start`, `stop`,
`status`, `pause`, `resume`, `search`, `recent`, `forget`, `export`, `import`, `sources`,
`doctor`, `ui`, `data-dir`) — not separate
binaries per concern. See `ARCHITECTURE.md`'s process model for why.

**No `unwrap()`/`expect()` in runtime code.** Tests may use `?` with `Box<dyn Error>`. A justified
`expect()` on a true invariant needs a `// SAFETY:`-style comment explaining why it cannot fail.
`unsafe` is permitted only inside Windows API wrapper functions, each block with its own
`// SAFETY:` comment (and, when another platform gets backends, only inside that platform's
equivalent OS API wrappers behind the platform seam).

## Build commands

```powershell
cargo build --workspace
cargo test --workspace
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

**On this machine**, `cargo`/`rustc` are installed via `rustup` but are not on the default shell
`PATH` for every tool invocation. Prefix commands with the toolchain's `bin` directory:

- PowerShell: `$env:PATH = "$env:USERPROFILE\.cargo\bin;$env:PATH"` then run cargo normally, or
  call the binaries directly via their full path.
- Git Bash / the Bash tool: `PATH="/c/Users/$USER/.cargo/bin:$PATH" cargo <command>` (confirmed
  working as `PATH="/c/Users/kubert/.cargo/bin:$PATH" cargo check --workspace` on this machine;
  the equivalent Windows path is `C:\Users\kubert\.cargo\bin`).

Verify `cargo --version` / `rustc --version` report 1.95+ before assuming the toolchain isn't
installed — it usually is, just not exported.

## Where things live

```
crates/rsrewind-core/      shared domain types, config, paths, privacy rules — implemented
crates/rsrewind-segment/   sealed history segment file format (replication unit) — implemented
crates/rsrewind-storage/   SQLite schema, migrations, media I/O, segment export/import — implemented
crates/rsrewind-query/     read-only search/timeline queries + cross-source history facade — implemented
crates/rsrewind-capture/   Windows capture backends; KDE Plasma backends (kwin.rs, wayland_idle.rs); change detection
                           and window shapes (portable)
crates/rsrewind-ocr/       Windows.Media.Ocr wrapper — Windows-only
crates/rsrewind-daemon/    recorder loop, persist/OCR threads, platform seam (platform.rs) — portable, tested on
                           fakes (src/tests/); Windows backends in windows_platform.rs, Plasma in linux_platform.rs; Unit B partly open
crates/rsrewind-cli/       rsrewind.exe, clap subcommands — implemented (recorder commands Windows and Plasma only)
crates/rsrewind-ui/        desktop UI: Iced chrome + custom wgpu rewind viewport — implemented; Windows-verified on synthetic history only
docs/mvp-contract.md       the orchestrator's pre-implementation contract for the first slice
docs/design/distributed.md probe / central-instance architecture, options, threat model, stages
docs/design/probe-visualizer.md  the plan to a full-featured probe/visualizer split (milestones V1-V6, gaps, tray)
docs/dev-process.md        STD-001 log: routing decisions and outcomes for this repo
docs/standards.md          company standards (STD-001..008) adoption status for this repo
docs/installer.md          MSI installer design and status
installer/                 WiX v5 source (rsrewind.wxs) — written, not yet built or tested
.github/workflows/ci.yml       fmt/clippy/test/build on every PR and push to main
.github/workflows/release.yml  tag-triggered release: build, sign (fail-closed), MSI, publish
```

Before editing `rsrewind-core`, read `docs/mvp-contract.md`'s ground rules: shared types are
frozen for the current slice. If a change there is actually needed, say so in your report instead
of editing it unilaterally — the orchestrator adjudicates.

## STD-001 routing (this repo's implementers and reviewers)

This repo follows the company's STD-001 adversarial-review pattern
(`corporate-strategy/standards/STD-001`; see `docs/dev-process.md` for the live log). The
orchestrator writes the contract/spec (and ideally tests) first, routes implementation work by
risk, and adjudicates every review finding itself rather than trusting a dispatcher's summary.

Routing table for this repo:

- **Haiku** — mechanical, tightly-specified work: an established pattern to mirror, a type or
  test shaped exactly like an existing one, boilerplate that doesn't require a design decision.
  Never shipped unreviewed.
- **Sonnet** — default implementer, and default adversarial reviewer of anything Haiku or the
  orchestrator wrote.
- **Opus** — escalation, required (regardless of how small the diff looks) for:
  - security-sensitive code (privacy-rule evaluation, anything touching what gets
    captured/persisted/logged)
  - concurrency (the capture/persist/OCR thread pipeline, channel backpressure, shared `Store`
    access)
  - SQLite migrations and schema changes
  - any `unsafe` block (Windows API wrappers)
- **Astra** (`gpt-6-astra` via the Codex CLI, reasoning effort `ultra`) — reserved for
  **adversarial review**, not implementation, because of its usage limits. Not on `PATH`; invoke
  via the full path to `codex.exe` (see the maintainer's own notes for the exact invocation —
  this is a scarce, rate-limited resource, so it should be spent on the highest-leverage review
  passes, not routine implementation or mechanical review).

## Standing review rules (from STD-001)

- A review finding can be right about the problem and wrong about the fix — adjudicate the
  problem, choose the fix yourself.
- Re-read diffs after a fix round; regressions enter there as often as anywhere else.
- When a reviewer indicts the spec/contract rather than the code, believe it.
- Review reads diffs, so it finds defects that live in diffs. Bugs in thread lifetime,
  startup/shutdown ordering, or cross-run state (exactly the kind of thing this daemon's design
  is full of) need live verification against the running recorder, not just a diff read.
- Verify before trusting, in both directions: reproduce a finding before accepting it as real,
  reproduce a fix before calling it closed.
- A green test suite is evidence about the tests, not the code — ask reviewers to grade the tests
  explicitly, especially for anything mocking SQLite or Windows API calls.
- A dispatcher's own report is a claim, not proof the underlying tool call happened as described.
- Cost/quality data (tokens, time, defects found vs. real) is a deliverable of a review round, not
  overhead to skip when busy.

## Things to re-verify before trusting them

Claims that cannot be checked mechanically live here with a date (STD-003 rule 4). A claim without a
date is not allowed; re-verify it or delete it.

- **2026-10-07:** Windows 10 support is unverified. Only Windows 11 (build 26200) has been run.
- **2026-10-07:** `rsrewind doctor` reports capture as failed when run from a non-interactive session
  (SSH: `GraphicsCaptureSession::IsSupported ... 0x80070424`) and passes in an interactive one. The
  installed v0.0.2 was last confirmed capturing from a scheduled task bound to the console session.
- **2026-10-07:** The rewind UI has been run on Windows with synthetic history only; real recorded
  history, multiple monitors, light mode and display scaling other than 125% are unverified.
- **2026-10-07:** The segment import path (`rsrewind import`) has had unit tests, mutation checks and
  one real Windows record-export-import run, and no independent security review.
- **2026-10-07:** The 8-hour acceptance run for v0.1.0 (ROADMAP) has not been run.
- **2026-10-07:** Installer upgrade and uninstall behavior in `docs/installer.md` has not been
  re-tested since v0.0.2; only a fresh per-user install was confirmed.
- **2026-10-08:** The Plasma recorder has run only on one machine (Plasma 6.7.5, one 2496x1664 screen
  at 135 %, Bazzite). Multiple screens, other scales, KWin restarts mid-run, and other compositors are
  untested. On excluded ticks KWin still takes the frame and the recorder drops it from memory.
- **2026-10-07:** The recorder-side privacy findings in `docs/remediation-status.md` (Unit B) are
  open; every statement about privacy guarantees applies to the design, not to a reviewed build.
