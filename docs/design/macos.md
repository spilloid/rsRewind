# macOS client contract — 2026-10-10

Implement in an isolated branch; preserve the operator's unrelated working-tree edits.
Native Apple Silicon client, one rsrewind executable with daemon/UI/menu-bar subcommands.
Use Apple's screen-recording authorization and ScreenCaptureKit on macOS 14+;
never use the removed CGDisplayCreateImage path, capture files or a shell screenshot helper.
Native AppKit menu bar mirrors the shared tray model, CLI-only actions and two-click forget.
Reuse the portable recorder, perceptual change detection, duration coalescing, compressed
media, retention and SQLite boundaries. No schema changes, no approximate-hash global dedup
that might conflate screens. Report the actual storage behavior and remaining limitations.
Native OS bridges may use Swift compiled statically into the binary by Xcode on cloud CI.
Native OCR should use Vision and stay in memory. No captured content in error messages/logs.
Privacy checks happen before persistence; unavailable window/process/title/idle information
must fail closed. Advertise only actual capabilities. Permission refusal must be actionable.
The menu bar must distinguish recording, paused, stopped and unknown, and disclose privacy gaps.
Data lives in the existing private Application Support folder. Login startup uses a per-user
LaunchAgent with safely encoded argument array; disabling it preserves recordings.
CI uses a hosted ARM64 macOS runner, verifies arm64 architecture, runs tests/clippy and packages
an ad-hoc signed app and archive artifact. No Developer ID/notarization claim without credentials.
Cloud runners cannot validate an interactive user's TCC grant or desktop recording; document
that manual acceptance gap rather than calling CI an end-to-end capture test.
STD-001: contract first, independent adversarial review, adjudicate/reproduce findings, honest log.
STD-003: update docs with dated evidence. STD-006: no tokens or captured private content in CI.
STD-008: native UI runtime screenshots require a real desktop; retain an explicit unverified gate.
