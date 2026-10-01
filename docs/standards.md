# Company standards status

Reconciled 2026-09-30 against `corporate-strategy/standards/STD-001` through `STD-008` and this
branch (`feat/mvp-bootstrap` plus the MVP-bootstrap documentation pass). Re-verify these claims
against the live repo when the standards or the implementation change — this table is itself
subject to STD-003 (doc freshness): a claim here that goes stale is a bug, not a detail.

| Standard | Status | Evidence / reason |
|---|---|---|
| STD-001 adversarial review | adopted (early) | `docs/dev-process.md` defines the routing table and log; the first entry (2026-09-30, MVP bootstrap) is a header awaiting the orchestrator's reconciliation — no completed, reviewed unit is recorded yet, so this is newer and thinner than the portfolio's mature reference repos. |
| STD-002 PR template | adopted | `.github/PULL_REQUEST_TEMPLATE.md` has the required "What changed" / "How it was checked" / "Evidence" sections and a synthetic-data clause adapted to this domain. |
| STD-003 doc freshness | not-yet (rule 1 should pass; rules 2-4 unverified over time) | `README.md`'s stated status and `Cargo.toml`'s `workspace.package.version` (0.1.0) are consistent as of this writing, but no release has happened yet to prove the practice holds over time; most of the documentation set (this file included) describes a design contract more than confirmed shipped behavior, which is marked explicitly throughout rather than left implicit. |
| STD-004 identity architecture | n/a | rsRewind has no accounts, sessions, or identity surface of any kind — it is a single-user, local-only tool with no server component. |
| STD-005 agent surface | not-yet | The CLI's `--json` output and `rsrewind-core` query types are *designed* to be a stable agent-facing integration surface (see `ARCHITECTURE.md`'s query architecture section and v0.8.0 in `ROADMAP.md`), but no agent-facing approval/authorization surface exists yet — there's no CLI subcommand implemented at all yet, let alone one with agent-specific controls. |
| STD-006 secret handling | adopted for release signing | The `release` GitHub environment stores the Azure identifiers and signing variables used by `.github/workflows/release.yml`. Authentication uses a repository-specific OIDC federated credential, with no Azure client secret. See `docs/code-signing.md`; no secret is committed here. |
| STD-007 repo metadata | adopted (license, changelog) / not-yet (company-wide conventions) | `LICENSE` states MIT, copyright Joseph Spillers, 2026 (one consistent string within this repo). `CHANGELOG.md` exists in Keep a Changelog format with an `[Unreleased]` section describing the bootstrap. This repo's commits carry a `Co-Authored-By` attribution line per the orchestrating session's instructions; the company-wide AI-attribution convention remains undecided portfolio-wide per STD-007 itself, so this is a local choice, not a claim of company-wide policy. |
| STD-008 UI capture validation | not-yet | The WinUI 3 desktop UI (`rsrewind-ui`) is not yet implemented, so there is nothing to run capture/overflow validation against. Tracked as not-yet rather than n/a because this repo does plan a UI (unlike, say, a CLI-only tool) — this should move to "adopted" once `rsrewind-ui` exists and has the equivalent of the other repos' capture-assertion coverage. |
