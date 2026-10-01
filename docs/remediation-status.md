# Astra review remediation — status and Windows hand-off

Review: `docs/reviews/2026-09-30-astra-ultra-mvp.md` (of `aa2c5e7`). Contract amendments:
`docs/mvp-contract.md` → "Review amendments". Last updated 2026-10-01.

Unit A was done on a Linux box where only `rsrewind-core`, `-storage` and `-query` build. Units B
and C touch Windows-only crates and were **not started**; this file is their brief.

## Status by finding

| # | Area | Status | Where / evidence |
|---|---|---|---|
| F1 | OCR resurrection / id reuse | **fixed (A)** | AUTOINCREMENT; `save_ocr`/`mark_ocr` take the expected media path. `ocr_from_a_deleted_state_cannot_attach_to_a_later_one`, `ids_are_never_reused_after_deletion` |
| F2 | privacy provenance of frames | **open (B)** | recorder/capture |
| F3 | pause is a barrier | **open (B)** | needs control generation, see below |
| F4 | fail closed on missing info | **open (B)** | capture/window.rs, idle.rs |
| F5 | queued writes recreate forgotten range | **storage side fixed (A)**; persist handling **open (B)** | `deletion_fences`, `StorageError::Fenced`. `a_forget_fences_its_interval_against_queued_writes` |
| F6 | window titles survive deletion | **fixed (A)** | `deleting_history_removes_window_titles_and_applications` |
| F7 | FTS/WAL remnants | **fixed (A)** for `recall.db`/WAL; backups reported, not rewritten | `forgotten_text_is_gone_from_the_database_file_and_wal` (mutation-checked: fails without FTS secure-delete) |
| F8 | orphan media / failed cleanup | **fixed (A)** for orphans/temp files in range; no durable unlink-retry queue (an unlink that fails is retried only by a later pass that covers its time) | `orphan_media_files_in_the_range_are_deleted_with_it`, `stale_temp_files_and_foreign_files_are_handled_correctly` |
| F9 | queue admission ≠ persisted | **open (B)** | |
| F10 | media path confinement | **fixed (A)**; Windows junction branch **unverified** | `media_paths_must_live_under_media_and_not_cross_links` (symlink, Linux only) |
| F11 | probe dumps pixels | **open (B)** | capture/examples/probe.rs |
| F12 | retention only on traffic | **open (B)** | persist.rs timer |
| F13 | WGC out-of-order drain | **open (B)** | wgc.rs |
| F14 | console close | **open (B)** | daemon/win.rs |
| F15 | GDI helper dims | **open (B)** | ocr/gdi_render.rs |
| F16 | concurrent migration backups | **fixed (A)** | `concurrent_migrations_keep_one_correct_backup` |
| F17 | `at()` | **fixed (A)** | `at_finds_a_long_span_buried_under_many_short_events` |
| F18 | timeline paging | **fixed (A)** | `TimelineCursor`; `paging_never_skips_observations_that_share_a_timestamp` |
| F19 | search time ranges | **fixed (A)** | three tests in `rsrewind-query/tests/search.rs` |
| F20 | `visual_detail` snapshot | **fixed (A)** | `visual_detail_is_one_snapshot_even_if_ocr_commits_mid_read` (mutation-checked) |
| F21 | HWND attribution | **open (B)** | |
| F22 | retention accounting | **fixed (A)** | `Store::disk_bytes`, compaction. `size_retention_counts_wal_and_orphan_media` |

Quality gate on Linux for the three portable crates: `cargo test` (core 21, storage 38, query
7+19) and `cargo clippy --all-targets -D warnings` pass. Nothing else was built.

## Unverified edits (never compiled — do this first on Windows)

- `crates/rsrewind-daemon/src/ocr_worker.rs` — `save_ocr`/`mark_ocr` now take
  `&item.relative_path`.
- `crates/rsrewind-cli/src/main.rs` `forget()` — new report fields; fence `until` is now `now+1ms`
  (was `now+60s`, which would have made the recorder drop a minute of legitimate frames).
- Run: `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets --all-features -- -D
  warnings`, `cargo test --workspace`. Expect other call sites of the changed Store/Query APIs
  (below) in `persist.rs`, `recorder.rs`, `doctor.rs`, `render.rs` to need touch-ups.

## API changes Unit B/C must adapt to

- `Store::save_ocr(id, expected_media_path, blocks, engine, ms)` and
  `Store::mark_ocr(id, expected_media_path, status, error)`; `PendingOcr.relative_path`.
  A mismatch returns `VisualStateMissing`.
- `StorageError::Fenced { at, since, until }` from `insert_visual_state` and `record_observation`.
- `StorageError::ContextMissing` from `record_observation` when a cached application/window id was
  deleted with its history.
- `DeleteReport` gains `orphan_files_deleted`, `wal_truncated`, `backups_surviving`.
- `Store::disk_bytes()` (new, pub); `live_db_bytes` removed.
- `QueryDb::recent(limit, after: Option<TimelineCursor>)`; `TimelineEntry.event_id`,
  `TimelineEntry::cursor()`; `rsrewind_core::TimelineCursor`. `--json` output gains `event_id`.
- `DataDir::resolve_media` now requires a `media/` prefix and depth ≥ 2.

## Unit B (capture / recorder) — TODO and tension points

TODO, in dependency order:

1. **`persist.rs` Fenced/ContextMissing handling.** On `Fenced`: delete the media file just
   written, invalidate the persisted state for that monitor, mark the current picture dirty so a
   later *safe* frame is stored. On `ContextMissing`: drop cached application/window ids, upsert
   again, retry once. Neither is a generic failure.
2. **F3 control generation.** `set_control` does not yet increment a generation, and
   `recorder_status` has no `applied_generation`; both are schema changes (edit `0001` in place,
   it is unreleased) plus `RecorderStatus`/`read_control` API. Recorder acks *after* the active
   tick finishes; CLI `pause` waits ~3 s.
3. F2/F4/F21/F9/F12/F13/F14/F15/F11 per the contract amendments.
4. Tests for F2/F3/F9 need deterministic barriers (fake clock / frame source), not sleeps — the
   review graded the recorder tests "F"; helper-boolean tests in `plan.rs` are not evidence.

Tension points found while doing Unit A:

- **Clock domain for fences.** Fences compare `captured_at` (wall-clock ms). F2 wants frames
  stamped with WGC capture time, which is QPC-relative. Convert to wall clock at capture, using
  one anchor, or a fence/forget will mis-classify frames near its boundary.
- **Fence vs. pause generation** are two separate mechanisms; do not collapse them. Fences guard
  *already-captured* content; the generation guards *future* capture.
- **VACUUM during retention.** `apply_retention` may run `VACUUM` when over the size cap. It holds
  the write lock for the whole rewrite; the OCR thread's connection can hit the 5 s busy timeout
  and log failed saves. Either schedule retention only when OCR is idle or switch to
  `auto_vacuum=INCREMENTAL` now (schema is still unreleased, so it is free to change).
- **Retention walks the whole media tree** (`disk_bytes`, orphan sweep) every run. Fine at
  thousands of files; revisit before v0.2 if retention runs hourly on 100k+ files.
- **Window/app deletion race.** Deletion removes *every* unreferenced window/application, so a
  persist thread that has upserted but not yet observed can get `ContextMissing`. Handled by
  item 1; it is by design, not a bug to "fix" with a grace period.
- **Migration lock.** `backups/.migrate.lock`: waiters give up after 120 s (`BackupFailed`,
  daemon refuses to start) and a lock older than 600 s is taken over. A multi-GB backup could in
  theory exceed either; acceptable for now, noted for v0.9 hardening.
- **`mark_ocr` on a deleted row** returns `VisualStateMissing`; `ocr_worker.rs` logs it as a
  warning. Harmless noise; downgrade to debug if it shows up.
- **`at()`** scans backwards for a covering span; with an enormous history and one span that
  outlives millions of later events it is O(n). Add an index/interval table if profiling says so.

## Unit C (WinUI viewer) — TODO and tension points

Nothing of Unit C survived: `crates/rsrewind-ui/src/lib.rs` is the 7-line stub, and no design
boards are in Git (only `docs/design/brief.md`). Build to the brief ("nostalgia at the edges,
clarity at the center"), reading only through `rsrewind-query`.

- Use `TimelineCursor`/`event_id` for timeline paging; do not page by timestamp.
- `visual_detail` is now a single snapshot; the UI can call it on selection without caveats.
- Search hits report the earliest *matching* observation (it may differ from the state's first
  sighting): "cue to the moment" must jump to that timestamp, not `captured_at`.
- The UI crate must still not depend (even transitively) on capture/storage/ocr/daemon. Note that
  `rsrewind-query` depends on `rsrewind-storage` (for `SCHEMA_VERSION` and `resolve_media_path`),
  so this is currently only true for the *direct* graph — the CLAUDE.md rule says "even
  transitively". Either move those two items into `rsrewind-core` or amend the rule; decide before
  the UI crate gets real dependencies.

## Still to do after B and C (from the handoff)

Live recorder validation on a throwaway data dir; a second Astra pass aimed at the areas listed in
the handoff (privacy races, deletion guarantees, stale writes, unsafe Windows API behaviour, DB
edge cases, pause semantics, crash/restart, multi-monitor, path traversal/reparse points, bounded
memory); then `docs/dev-process.md`, `ROADMAP.md`, `CHANGELOG.md`.
