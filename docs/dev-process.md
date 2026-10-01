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
