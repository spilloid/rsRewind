# Changelog

All notable changes to this project are documented in this file. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project intends to follow
[Semantic Versioning](https://semver.org/) once it has a first release.

## [Unreleased]

## [0.0.1] - 2026-10-01

The MVP bootstrap: the architecture contract and the shared domain-type foundation the rest of
the implementation is built against. No end-user-facing functionality ships yet — the
`rsrewind.exe` binary is currently a placeholder.

### Fixed (Astra review, storage/query — recorder fixes still open)

- Storage/query findings F1, F5 (storage side), F6, F7, F8, F10, F16–F20, F22: ids never reused;
  deletion fences; deletion also removes window titles and applications, orphan media and stale
  temp files, and is physical inside `recall.db` (secure_delete, FTS5 secure-delete, WAL
  truncate); media paths confined to `media/` without symlinks/junctions; serialised migrations
  with exclusive backup names; size retention counts the real disk footprint; `at()`, timeline
  paging (`TimelineCursor`), search time-range semantics and `visual_detail` snapshots corrected.
  See `docs/remediation-status.md`.

### Added

- `docs/mvp-contract.md` — the orchestrator's architecture contract for the first vertical slice,
  covering the crate graph, process model, storage schema, capture/OCR/query pipeline design, and
  threading model, written before implementation per this repo's STD-001 process.
- Cargo workspace scaffolding: `rsrewind-core`, `rsrewind-capture`, `rsrewind-storage`,
  `rsrewind-ocr`, `rsrewind-query`, `rsrewind-daemon`, `rsrewind-cli` (binary: `rsrewind.exe`),
  and `rsrewind-ui` crates, with workspace-level lint policy (`clippy::unwrap_used`,
  `clippy::expect_used`, `clippy::dbg_macro`, `clippy::todo` as warnings) and a pinned toolchain
  (`rust-toolchain.toml`: stable, edition 2024, `x86_64-pc-windows-msvc`).
- `rsrewind-core`: the shared domain vocabulary every other crate builds on —
  `Config`/`CaptureConfig`/`StorageConfig`/`OcrConfig`/`LoggingConfig` (TOML config with
  unknown-key rejection and full defaults), `Timestamp` (UTC millisecond precision),
  typed row ids (`EventId`, `VisualStateId`, `SessionId`, `MonitorId`, `ApplicationId`,
  `WindowId`), the event model (`EventKind`, `MonitorInfo`, `ApplicationContext`,
  `WindowContext`, `FocusContext`, `BgraFrame`, `OcrBlock`, `CaptureState`), `PrivacyPolicy` and
  its case-insensitive process/title glob matching, `DataDir` and the
  `%LOCALAPPDATA%\rsRewind` layout (with path-escape-safe relative media path resolution), and
  the query-facing types (`SearchQuery`, `SearchHit`, `TimelineEntry`, `VisualDetail`) that will
  back the CLI's `--json` output. Fully unit-tested.
- Project documentation: `README.md`, `ARCHITECTURE.md`, `ROADMAP.md`, `PRIVACY.md`,
  `CONTRIBUTING.md`, `SECURITY.md`, `CLAUDE.md`, `docs/dev-process.md`, `docs/standards.md`,
  `docs/installer.md`.
- CI (`.github/workflows/ci.yml`): format, lint, test and release-profile build on
  `windows-latest` for every pull request and push to `main`.
- Release pipeline scaffolding (`.github/workflows/release.yml`): build, a fail-closed code
  signing gate modeled on the company's Azure Artifact Signing pattern (see
  `docs/code-signing` notes in `CLAUDE.md`), MSI packaging, checksums, and a prerelease GitHub
  Release — **not yet exercised**, since the signing identity, variables and secrets this
  workflow expects do not exist yet for this repository.
- MSI installer design (`installer/rsrewind.wxs`, WiX Toolset v5): per-user install, Start Menu
  shortcut, optional startup registration, major-upgrade handling — **written but not yet built
  or tested**; see `docs/installer.md`.
- `.github/PULL_REQUEST_TEMPLATE.md` per STD-002, including a synthetic-data clause adapted to
  this project's domain (no real personal or customer content in screenshots or test fixtures).

### Known gaps at this point in history

- `rsrewind-capture`, `rsrewind-storage`, `rsrewind-ocr`, `rsrewind-query`, `rsrewind-daemon`, and
  `rsrewind-ui` are scaffolded (crate + `Cargo.toml` exist) but not yet implemented.
- `rsrewind-cli`'s `main.rs` is a placeholder; none of the subcommands described in `README.md`
  exist yet.
- No code signing identity is configured for this repository; `.github/workflows/release.yml`
  will refuse to publish until it is.
- No encryption at rest (see `PRIVACY.md`).

[Unreleased]: https://github.com/spilloid/rsRewind/compare/main...HEAD
