## What changed

<!-- What problem does this solve? What would break or be missing without this change? -->

## How it was checked

- [ ] `cargo fmt --all -- --check` passes
- [ ] `cargo clippy --workspace --all-targets --all-features -- -D warnings` passes, zero warnings
- [ ] `cargo test --workspace` passes
- [ ] `cargo build --workspace --release` succeeds
- [ ] No `unwrap()`/`expect()` added in runtime code (or any added `expect()` has a comment
      explaining why the invariant cannot fail)
- [ ] No new network code (no HTTP client, no listening socket, no telemetry) anywhere in the
      recording path
- [ ] No SQL added outside `rsrewind-storage` (writes) / `rsrewind-query` (reads)
- [ ] If this touches `rsrewind-ui`: it still depends only on `rsrewind-core` and
      `rsrewind-query` — no new dependency on `rsrewind-capture`, `rsrewind-storage`,
      `rsrewind-ocr`, or `rsrewind-daemon`
- [ ] If this touches logging: no OCR text, window titles, or other captured screen content is
      logged at `info` level or above
- [ ] Relevant docs updated in this same PR (`README.md`, `ARCHITECTURE.md`, `PRIVACY.md`,
      `ROADMAP.md`, `CHANGELOG.md`) if behavior changed
- [ ] Screenshots, sample data, and test fixtures in this PR contain no real personal or customer
      content — only synthetic/placeholder windows, titles, and OCR text

## Evidence

<!-- Logs, screenshots, or a short reproduction. For anything touching the capture/persist/OCR
     thread pipeline or startup/shutdown ordering, prefer evidence from actually running the
     recorder over a diff read alone — see CLAUDE.md's standing review rules. -->
