# Company standards status

Reconciled 2026-10-08 against `corporate-strategy/standards/STD-001` through `STD-008` and `main` at
v0.0.4 (Linux CI job added; nothing else changed). Re-verify these claims
against the live repo when the standards or the implementation change — this table is itself
subject to STD-003 (doc freshness): a claim here that goes stale is a bug, not a detail.

| Standard | Status | Evidence / reason |
|---|---|---|
| STD-001 adversarial review | adopted, review debt | `docs/dev-process.md` has a real log (MVP bootstrap, Astra remediation unit A, replication, UI). The Astra review covered the first slice only: the replication/import path and the UI have **not** been adversarially reviewed. Operator direction 2026-10-07: the full review ritual is not required for this repo; major decisions are instead filed as proposed motions in `corporate-strategy/board/proposals/` (`rsrewind-portfolio-status`, `rsrewind-network-transport`, `rsrewind-linux-wayland-port`). |
| STD-002 PR template | adopted | `.github/PULL_REQUEST_TEMPLATE.md` has the required sections and a synthetic-data clause. `check.sh std-002` passes against this repo (run 2026-10-07 on a temporary copy of the checker with rsRewind added to its repo list). |
| STD-003 doc freshness | adopted | Rule 1: `scripts/check-docs.sh` (run by CI and the Pages workflow) compares the version in `Cargo.toml`, `CHANGELOG.md`, `README.md` and `site/`. Rule 2: stated in CONTRIBUTING. Rule 3: the numbered release runbook in CONTRIBUTING includes the docs step. Rule 4: dated quarantine list in CLAUDE.md. History: the README claimed "nothing works yet" for the whole of v0.0.1 and was corrected on 2026-10-07. The company `check.sh` reports UNKNOWN for std-003/std-007-changelog here because it cannot read a Cargo workspace version, not because of a failure. |
| STD-004 identity architecture | n/a | No accounts, sessions or user identity. Replication uses random per-installation source ids, which identify a data folder, not a person. |
| STD-005 agent surface | not-yet | `--json` output on `search`, `recent`, `status`, `doctor`, `export`, `import`, `sources` is a stable, typed surface, but there is no agent-specific authorization surface. |
| STD-006 secret handling | adopted for release signing | Azure OIDC federated credential, no stored client secret (`docs/code-signing.md`). Probe authentication would introduce a new secret class; proposed, not built (motion `rsrewind-network-transport`). |
| STD-007 repo metadata | adopted (license, changelog) / not-yet (attribution) | `LICENSE` is MIT, Joseph Spillers, 2026 (`check.sh std-007-license` passes). `CHANGELOG.md` is Keep a Changelog and its newest release equals `Cargo.toml`. Commits and PRs carry a `Co-Authored-By` / "Generated with Claude Code" line per the session default; the company-wide attribution position is still undecided. |
| STD-008 UI capture validation | not-yet (partial evidence) | The desktop UI exists, and its screenshots on the website are captured from the running Windows build on synthetic history. There is no scripted assertion harness: the UI is a custom-drawn wgpu surface, so Playwright-style DOM overflow checks do not apply. The closest equivalent today is that layout, camera, culling and view math are pure functions with unit tests (`rsrewind-ui/src/timeline/model.rs`). A desktop screenshot-assertion harness remains a candidate (corporate-strategy D-0044), not a standard. `check.sh std-008-ci` fails for this repo, as it would for any repo without a `capture-*.mjs`. |

## Portfolio status

rsRewind is not yet in `corporate-strategy/state/products/` or `standards/check.sh`'s repo list. Where it sits
(toy, tracked non-commercial line, or product) is a proposed motion, not a decision. Until it is decided,
this file is the only record of standards adoption for this repo.
