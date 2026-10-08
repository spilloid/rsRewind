# Dev process log

Records every round where implementation or adversarial review work is routed per STD-001 (see
`corporate-strategy/standards/STD-001`, and `CLAUDE.md`'s routing table for this repo's specific
tiers: Haiku / Sonnet / Opus / Astra). The pattern itself — an orchestrator writing the contract
first, routing implementers by risk, and adjudicating review findings itself rather than trusting
a dispatcher's summary — is adapted from the same precedent already in use on other repositories
in this portfolio. This file starts here and accrues real entries going forward; it is not a
retroactive account of work that predates it.

## Routing (current)

See `CLAUDE.md` for the authoritative, up-to-date version of this table — duplicated here briefly
for log context:

- **Haiku** — mechanical, tightly-specified work, an established pattern to mirror.
- **Sonnet** — default implementer and default adversarial reviewer.
- **Opus** — security-sensitive code, concurrency, SQLite migrations, any `unsafe` block —
  regardless of diff size.
- **Astra** (`gpt-6-astra`, Codex CLI, `ultra` effort) — adversarial review only, reserved for
  the highest-leverage passes because of its usage limits.

These are starting priors, not fixed law — revise whichever rule the log below stops supporting,
the same discipline STD-001 asks of every adopting repo.

## Log

### 2026-09-30 — MVP bootstrap

*To be completed by the orchestrator once the parallel implementation workstreams for this
vertical slice (capture, storage, OCR, query, daemon, CLI, documentation) land and are
reconciled. This entry's header is in place now so the log format is established before the
first real row is written; filling in the table below is the orchestrator's job, not this
documentation pass's.*

Context: `docs/mvp-contract.md` was written as the pre-implementation contract for the first
vertical slice, and implementation of the crates it specifies was dispatched to multiple parallel
agents/worktrees, with this documentation-and-scaffolding work (README, ARCHITECTURE, ROADMAP,
PRIVACY, CONTRIBUTING, CHANGELOG, CLAUDE.md, CI/release workflows, installer design) as one of
those parallel units.

| Unit | Scope | Tier routed | Model | Outcome | Notes |
|---|---|---|---|---|---|
| TODO | `rsrewind-core` (domain types, config, privacy, paths) | TODO | TODO | TODO | orchestrator to fill in from the actual implementer's worktree/report |
| TODO | `rsrewind-capture` | TODO | TODO | TODO | |
| TODO | `rsrewind-storage` + `rsrewind-query` | TODO | TODO | TODO | per `docs/mvp-contract.md`, same owner for both |
| TODO | `rsrewind-ocr` | TODO | TODO | TODO | |
| TODO | `rsrewind-daemon` | TODO | TODO | TODO | |
| TODO | `rsrewind-cli` | TODO | TODO | TODO | |
| TODO | `rsrewind-ui` | TODO | TODO | TODO | |
| — | Docs/CI/release/installer scaffolding (this unit) | n/a — documentation task, not a code-risk tier | Sonnet (this agent) | self-reported complete; **not yet adversarially reviewed** | Built from `docs/mvp-contract.md` and `crates/rsrewind-core/src/*`; every claim about unimplemented crates is marked explicitly rather than assumed. Needs a reviewer pass once the orchestrator reconciles this against the real crate implementations, per STD-001's "a dispatcher's own report is a claim, not proof" rule. |
| TODO | Adversarial review of the above, once implementation units land | Astra (adversarial review tier) or Opus | TODO | TODO | reserve Astra for this pass per its usage-limit guidance in `CLAUDE.md`, not for routine implementation review |

**Status after bootstrap:** unfilled — see `ROADMAP.md`'s "Status after bootstrap" section, which
carries the same TODO placeholders for the orchestrator to reconcile once all parallel units are
in.


### 2026-10-01 — Astra remediation, Unit A (storage/query), session hand-over

Orchestration notes: the previous session ended before any remediation commit was pushed
(`origin/feat/mvp-bootstrap` was still `906ce21`); nothing from Units A/B/C survived, so A was
redone here. This session ran on Linux, so only the portable crates (core/storage/query) could be
built and tested; Units B and C were deliberately not started and are briefed in
`docs/remediation-status.md`.

| Unit | Scope | Tier routed | Model | Outcome | Notes |
|---|---|---|---|---|---|
| A | storage + query + core paths (F1, F5s, F6–F8, F10, F16–F20, F22) | Opus-tier work (migrations, deletion/erasure, schema) done by the orchestrator session itself | Sonnet 5.5 | 7 + 6 regression tests written first and confirmed red on `906ce21`; all green; clippy clean on the three crates | **Not yet adversarially reviewed.** Mutation-checked two tests (FTS secure-delete, read transaction); one first mutation was invalid (same connection shares the transaction) and was redone. |
| B | capture/recorder (F2–F4, F9, F11–F15, F21) | Opus (concurrency, unsafe, security) | — | **not started** | needs Windows to build and verify |
| C | WinUI viewer | Sonnet | — | **not started** | needs Windows |

Defects noted along the way: the CLI's `forget` padded the fence `until` by 60 s into the future,
which with a real fence would have dropped a minute of legitimate frames (fixed); a
`resolve_media_path` test compared `/` against `\` separators and failed on non-Windows (made
portable).


### 2026-10-06 — Probe / central-instance foundation (stage 1)

Orchestration notes: architecture analysis and the smallest foundation for "a probe seals history,
a central instance imports it", done in one Sonnet 5.5 session on Linux (only the portable crates
build here). Design: `docs/design/distributed.md`.

| Unit | Scope | Tier routed | Model | Outcome | Notes |
|---|---|---|---|---|---|
| D1 | `rsrewind-segment` format + 11 tests | Sonnet | Sonnet 5.5 | green, clippy clean | truncation at every boundary, bit-flips, hostile lengths and ids |
| D2 | storage export/import + 21 replication tests; small refactors of `insert_visual_state`, `save_ocr`, `upsert_window` into shared helpers | **Opus-tier** (security-sensitive input, schema-adjacent) done by the orchestrator session | Sonnet 5.5 | existing 38 storage tests unchanged and green, 21 new | mutation-checked: disabling the idempotency guard and weakening the sealing selection each fail tests |
| D3 | CLI `export`/`import`/`sources` + real-binary test; query end-to-end test | Sonnet | Sonnet 5.5 | green | |

Follow-up D4 (stage 1b): `forget` + outbox cleanup, `doctor` replication check, 3 tests, same tier and outcome.

Windows verification (2026-10-06, kubert, Windows 11, rustc 1.98.1): `cargo fmt --check`, `cargo build
--workspace`, `cargo test --workspace` and `cargo clippy --workspace --all-targets --all-features -- -D
warnings` all pass, including the capture/OCR/daemon crates. Real end to end with the actual recorder
(WGC capture, Windows OCR, `idle_after_secs` raised so an unattended box still records): 6 screens
recorded, sealed, imported into a second data folder, a repeat import was a no-op, and `search zebra`
on the replica returned OCR snippets. Finding: with default settings an unattended box records
only `recorder_start`/`idle_start`/`recorder_stop` after 5 minutes of no input, as designed.

**Not yet adversarially reviewed** — owed an Opus or Astra pass before release, aimed at: import of
hostile segments (path/identity handling, resource limits), the sealing selection under clock
changes, crash windows between publish and watermark, and the refactored shared insert helpers.
Not run on Windows. Pre-existing and untouched: `cargo check --workspace` fails on Linux (17
errors from Windows-only dependencies) and `rsrewind-cli` has two dead-code warnings on non-Windows.

### 2026-10-06 — History facade and the Iced + wgpu UI

- **Routing:** single implementing agent, decision by the maintainer: Rust only, Iced chrome, custom
  wgpu viewport (supersedes the WinUI 3 plan; ARCHITECTURE.md "Why Iced + wgpu").
- **Facade first, test-driven.** `rsrewind-query` lost its `rsrewind-storage` dependency
  (`SCHEMA_VERSION`, media path resolution moved to core); `ui_boundary.rs` enforces the UI graph via
  `cargo metadata`. `History` merges local + replica stores with a total cross-source cursor
  `(started_at, source, event_id)`; 10 facade tests over fixtures built only with the real
  record/export/import path, with deliberately colliding timestamps and row ids. Mutation-checked:
  five mutants (cursor projection x2, per-source detail lookup, replica identity check, core cursor
  order) each killed; one mutant made the paging test spin forever, so the test gained a guard.
- **UI.** Pure, unit-tested modules (`timeline::model` layout/camera/culling/hit-testing,
  `timeline::cache` byte-bounded LRU, `thumb` box-filter downscale, worker coalescing/queue) are
  testable on Linux with plain `rustc --test`; the iced/wgpu crate itself is built, clippy'd
  (`-D warnings`) and tested (22 tests) on kubert.
- **Visual verification** on kubert with synthetic history (3 sources x 150 moments), driven by a
  no-password scheduled task in the logged-on session; screenshots captured only the rsRewind
  window's client rectangle (PrintWindow returns black for wgpu's flip-model swapchain, so
  CopyFromScreen with the window in front). Search, cue, wheel and drag scrubbing, keyboard step and
  source filter all exercised; the process stayed alive throughout (~175 MB working set).
  `rsrewind ui` returns in ~350 ms leaving one separate window process. Fixed from screenshots:
  clipped transport row, overlapping lane labels, unused space under the floor; dropped thumbnail
  requests are now retried. Not verified: real recorded history, multi-monitor, light mode, DPI
  other than 125 %.

### 2026-10-08 — Platform seam (Linux port, step 1)

- **Routing:** Opus implementer (privacy-sensitive capture path and thread pipeline, STD-001), on
  branch `feat/platform-seam`; board motion `rsrewind-linux-wayland-port` provisional answers
  honoured (fail closed without `privacy.unenforced_ok`, persistent "NOT enforced" badge in
  `status`/`doctor`, no `window_titles` claim, no network code, control in SQLite). No Linux
  capture/OCR yet.
- **What changed:** `rsrewind-daemon/src/platform.rs` traits (frame source, capture backend,
  screen context with `Capabilities`, idle clock, clock, lifecycle/single instance, OCR backend);
  recorder/persist/OCR loops un-gated; Windows adapters in `windows_platform.rs`;
  `Capabilities`/`SessionCapabilities` and `privacy.unenforced_ok` in core; per-session
  capabilities in `settings` and export narrowing in storage; CLI badge. No schema change.
- **Tests:** 11 tick-level + 3 end-to-end recorder tests on deterministic fakes (plus one ignored
  test pinning F9), 2 storage tests, 1 CLI test. Mutation-checked: 11 recorder mutants killed, 2
  survived and documented (`docs/remediation-status.md`), 1 storage mutant killed. Building the
  end-to-end test found that on the fake clock the capture loop outruns the real persist thread
  and drops jobs on the 4-deep queue (correct behaviour, but it made the run
  scheduling-dependent); that test uses a deeper queue via `run_with_queue`.
- **Windows verification (kubert, rustc 1.98.1):** `cargo fmt --check`, `build --workspace`,
  `test --workspace` (daemon 21 passed + 1 ignored, storage 63, cli 12+1+3, ui 22, ...) and
  `clippy --workspace --all-targets --all-features -D warnings` all green. Real recorder smoke on a
  throwaway data dir with `idle_after_secs = 86400`: 4 screens, 63 OCR lines, `recent`/`search
  platypus` find the Notepad text, `export --settle-minutes 1` sealed 6 events / 4 states, import
  into a second root, `search platypus` on the replica finds it. `status`/`doctor` output carried
  no new lines (rules enforced). All data, logs and tasks deleted afterwards.
- **Harness finding:** piping `rsrewind start` through `cmd /c ... | Out-File` hangs until the
  daemon exits, because the detached daemon inherits the pipe handle; redirect `start` to a file.
- **Linux:** core, segment, storage, query, capture (portable part), daemon and cli build, test
  and clippy clean; the OCR and UI crates are not built there (Windows-only dependencies / not
  needed).

