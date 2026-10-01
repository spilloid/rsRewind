# MVP contract (bootstrap, 2026-09-30)

This is the orchestrator's contract for the first vertical slice, written before implementation
per STD-001: every implementer works against it, every reviewer grades against it. It is a working
document; `ARCHITECTURE.md` is the durable description of what was actually built.

## Ground rules (all crates)

- Stable Rust, edition 2024, `x86_64-pc-windows-msvc`. Workspace lints apply: no `unwrap()` /
  `expect()` in runtime code (tests may use `?` with `Box<dyn Error>`; a justified `expect` on a
  true invariant needs a comment saying why it cannot fail).
- Library crates expose typed errors (`thiserror`). `anyhow` only in `rsrewind-cli` / `-daemon`
  top level.
- `tracing` for logs. **Never log OCR text, window titles, or file contents at `info` or above.**
  Log ids, counts, durations, rule names.
- `unsafe` only in Windows API wrappers, each block with a `// SAFETY:` comment.
- No network code of any kind. No listening sockets, no HTTP, no telemetry.
- `cargo fmt`, `cargo clippy --all-targets -- -D warnings` and `cargo test` must pass for the crate
  you own before you report done. Report the exact commands you ran and their output tails.
- Comments explain *why* (Windows quirks, invariants), not what.
- Shared types live in `rsrewind-core` and are frozen for this slice. If you need a change, say so
  in your report instead of editing core; the orchestrator adjudicates.

## Crate graph

```
core  <- capture
core  <- ocr
core  <- storage <- query
core, capture, ocr, storage           <- daemon
core, storage, query, daemon, ui      <- cli  (bin: rsrewind.exe)
core, query                           <- ui   (windows-reactor; windows-core 0.100 line)
```

Process model (decision, documented in ARCHITECTURE.md): **one executable, `rsrewind.exe`, with
subcommands.** `rsrewind daemon` is the long-running recorder; `rsrewind ui` is a separate process
that only reads through `rsrewind-query`; everything else is a short-lived CLI invocation. The UI
crashing cannot stop recording because they are different processes. Control (pause/resume) and
status flow through SQLite (`control` and `recorder_status` tables), not sockets or pipes.

## rsrewind-storage (owner: storage implementer)

SQLite via `rusqlite` (bundled, FTS5 on). One `Store` = one connection. The daemon opens two
(persist thread, OCR thread); WAL + `busy_timeout(5s)` make that safe.

On open: `journal_mode=WAL`, `synchronous=NORMAL`, `foreign_keys=ON`, `busy_timeout=5000`.
Migrations: ordered, embedded SQL; `schema_migrations(version INTEGER PRIMARY KEY, name, applied_at)`.
**Backup before migrating** any database that already has data and pending migrations: SQLite
online backup API to `backups/recall-v{from}-{utc}.db`, and refuse to migrate if the backup fails.
Refuse to open a database whose schema version is newer than the binary knows.

Schema v1 (timestamps = INTEGER unix ms UTC; media paths relative, forward slashes):

```
sessions(id, started_at, ended_at NULL, hostname, app_version)
monitors(id, device_name UNIQUE, width, height, left, top, dpi, primary_monitor, last_seen_at)
applications(id, process_name UNIQUE COLLATE NOCASE, exe_path NULL, first_seen_at)
windows(id, application_id FK, title, class_name NULL, first_seen_at,
        UNIQUE(application_id, title, class_name))
visual_states(id, monitor_id FK, captured_at, media_path UNIQUE, width, height, byte_size,
              fingerprint INTEGER, ocr_status TEXT CHECK in (pending, done, failed, skipped),
              ocr_error NULL, ocr_engine NULL, ocr_ms NULL)
events(id, session_id FK, kind TEXT, started_at, ended_at, monitor_id NULL FK,
       visual_state_id NULL FK, application_id NULL FK, window_id NULL FK, metadata_json NULL)
ocr_blocks(id, visual_state_id FK ON DELETE CASCADE, line_index, text, x, y, width, height,
           confidence NULL)
ocr_fts  = fts5(text, tokenize='unicode61 remove_diacritics 2')  -- rowid = visual_states.id
control(id INTEGER PRIMARY KEY CHECK(id=1), state TEXT, paused_until NULL, updated_at)
recorder_status(id CHECK(id=1), pid, started_at, heartbeat_at, state, counters_json)
settings(key PRIMARY KEY, value)
```

Indexes on every FK and on `events(started_at)`, `events(kind, started_at)`,
`visual_states(captured_at)`, `visual_states(ocr_status)`.

API (names may be refined; semantics may not):

- `Store::open(&DataDir) -> Result<Store>` (create + migrate), `Store::open_existing(&DataDir)`
  (error if missing; used by CLI so `status` never creates a database by accident).
- `begin_session`, `end_session`.
- `upsert_monitor(&MonitorInfo, seen_at) -> MonitorId`; `upsert_application(&ApplicationContext,
  at) -> ApplicationId`; `upsert_window(ApplicationId, &WindowContext, at) -> WindowId`.
- `insert_visual_state(NewVisualState) -> VisualStateId` (ocr_status = pending, or skipped if OCR
  disabled). The **file is written by the caller first** (`media::write_webp_exclusive`, temp file
  + rename, never overwrite); the row is inserted after, so a crash leaves at worst an orphan file,
  never a row pointing at nothing.
- `record_observation(Observation) -> EventId`: if the session's most recent observation for the
  same monitor has the same `(visual_state, application, window)` and `at - ended_at <=
  max_gap_ms`, extend its `ended_at`; otherwise insert a new event. This is how unchanged screens
  cost one row, not one row per tick.
- `record_marker(session, EventKind, at, Option<serde_json::Value>)`.
- `next_pending_ocr(limit) -> Vec<PendingOcr { id, media_path(abs), width, height }>` oldest first.
- `save_ocr(id, &[OcrBlock], engine, ms)`: one transaction — delete old blocks/FTS row, insert
  blocks, insert FTS row with lines joined by `\n`, set status done.
- `mark_ocr(id, status, error)`.
- Control/status: `read_control() -> CaptureState`, `set_control(CaptureState)`,
  `write_status(RecorderStatus)`, `read_status() -> Option<RecorderStatus>`.
- `delete_range(since, until) -> DeleteReport`: delete events in range, then visual states no
  longer referenced by any event, their blocks, FTS rows, and files (files after commit; a failed
  file delete is reported, not fatal).
- `apply_retention(retention_days, max_bytes, now) -> DeleteReport`: oldest-first.
- `stats() -> StorageStats` (counts, db bytes, media bytes, pending OCR, oldest/newest).
- `integrity_check() -> Vec<String>` (`PRAGMA quick_check`), `schema_version()`,
  `find_orphans(limit) -> OrphanReport` (rows whose file is missing; files with no row).

`media` module: `encode_webp(&BgraFrame, quality) -> Vec<u8>` (lossy via `webp` crate; BGRA→RGBA
conversion), `decode_webp(&[u8]) -> BgraFrame`, `write_webp_exclusive(&DataDir, relative, bytes)`.

Tests (required): migrations from empty; re-open is idempotent; newer-schema refusal; backup is
made before a migration on a non-empty DB (simulate with a v0 fixture or a test-only migration);
observation span extension vs. new row (gap, different window, different visual state);
FTS insert + match; delete_range removes rows, FTS and files and leaves shared visual states
referenced by surviving events; retention by age and by size; resolve paths stay in root;
encode/decode round trip dimensions.

## rsrewind-query (owner: storage implementer — same agent, it owns the schema)

Opens its own read-only connection (`SQLITE_OPEN_READ_ONLY`, `query_only=ON`).

- `parse_query(&str) -> FtsExpr`: user text → safe FTS5 MATCH string. Every bare token becomes a
  quoted phrase with `"` doubled; `"exact phrase"` stays a phrase; trailing `*` on a token keeps
  prefix search; operators/colons/parentheses in user text are treated as literal text, never FTS
  syntax. Empty or whitespace-only → no FTS (time/app filters only). Unit-tested heavily
  (injection attempts: `NEAR(`, `col:`, `"`, `-`, `^`, `AND OR NOT`).
- `search(&SearchQuery) -> Vec<SearchHit>`: FTS match joined to the *earliest* observation event
  of each visual state for timestamp/app/window/monitor; filters on time range, app (exact,
  case-insensitive, `.exe` optional), title substring; order by bm25 then recency; snippet via
  `snippet(ocr_fts, 0, '[', ']', '…', 12)`; default limit 20, max 200.
- `recent(limit, before: Option<Timestamp>) -> Vec<TimelineEntry>` newest first.
- `visual_detail(VisualStateId) -> Option<VisualDetail>`.
- `at(Timestamp) -> Option<TimelineEntry>` — the observation covering or nearest before a time.

Tests: parse_query table; search over a fixture DB built through `Store` (not raw SQL) covering
ranking, filters, snippets, unicode, and that the UI/CLI never need to write SQL.

## rsrewind-capture (owner: capture implementer)

All Windows. Thin unsafe wrappers, logic outside them testable.

- `monitors() -> Result<Vec<(MonitorHandle, MonitorInfo)>>` via `EnumDisplayMonitors` +
  `GetMonitorInfoW` + `GetDpiForMonitor`. Process must be per-monitor-v2 DPI aware (expose
  `enable_dpi_awareness()` for the daemon to call first thing).
- `MonitorCapturer`: Windows.Graphics.Capture per monitor (`IGraphicsCaptureItemInterop::
  CreateForMonitor`, free-threaded `Direct3D11CaptureFramePool`, cursor off, border off where the
  OS allows — failing to disable the border is not an error). `latest_frame(&mut self) ->
  Result<Option<BgraFrame>>` returns the newest frame since the last call, or `None` if Windows
  produced no new frame (WGC only delivers frames when content changes — that is our first,
  free change detector). Copy via a staging texture; respect `RowPitch`. Recreate on device lost /
  closed item; surface a typed error the daemon can count and recover from.
- `foreground() -> Option<FocusContext>`: `GetForegroundWindow`, `GetWindowTextW`,
  `GetClassNameW`, `GetWindowThreadProcessId`, `OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION)` +
  `QueryFullProcessImageNameW`. Access denied (elevated targets) → process name from what we can
  get, `exe_path: None`; never fail the tick.
- `visible_windows() -> Vec<VisibleWindow { process_name, title, rect, monitor: device_name }>`
  for privacy: top-level, visible, not cloaked (`DwmGetWindowAttribute(DWMWA_CLOAKED)`), non-zero
  area. The daemon excludes a monitor's frame when **any** visible window on it matches a privacy
  rule, not just the foreground one.
- `idle_millis() -> u64` via `GetLastInputInfo` (handle tick wraparound).
- `change` module (pure): `Fingerprint::from_frame(&BgraFrame)` — grayscale downscale to 64×36
  (area average), plus a 64-bit dHash; `ChangeDetector { threshold }::is_meaningful(prev, next)`:
  fraction of fingerprint cells whose luma differs by more than a small epsilon > threshold.
  Replaceable behind a small trait. Tests: identical frames, single-pixel noise, cursor-sized
  change below threshold, text-scroll above threshold, different sizes = changed, all-black.

## rsrewind-ocr (owner: OCR implementer)

- `OcrEngine::new(language: Option<&str>) -> Result<OcrEngine>`:
  `Windows.Media.Ocr.OcrEngine::TryCreateFromLanguage` or `TryCreateFromUserProfileLanguages`;
  typed error when no OCR language pack is installed (doctor reports it).
- `available_languages() -> Vec<String>`.
- `recognize(&BgraFrame) -> Result<OcrOutput { blocks: Vec<OcrBlock>, text: String,
  elapsed_ms }>`: `SoftwareBitmap` (Bgra8, premultiplied) from the frame; respect
  `OcrEngine::MaxImageDimension` by downscaling and scaling boxes back; one `OcrBlock` per
  `OcrLine` with the union of its word rects; `line_index` in reading order; `confidence: None`.
  Blocking `.get()` on the async op is fine — callers run it on a dedicated worker thread (MTA).
- Pure helpers (`union_rect`, scale-back math, line text normalization) unit-tested.
- An `#[ignore]` integration test that renders known text into a frame (or loads a checked-in
  tiny PNG/WebP fixture) and asserts the words come back, runnable on a machine with an OCR
  language installed.

## rsrewind-daemon (orchestrator / integration)

Threads, bounded `std::sync::mpsc::sync_channel`s, no async runtime:

```
capture thread  (tick = 1/fps) -- try_send --> persist thread (owns Store #1)
                                                  |  encode WebP, write file, insert rows
                                                  '-- try_send wake --> OCR thread (Store #2)
                                                                         pulls next_pending_ocr
```

- Capture never blocks: a full persist queue drops the candidate and increments a counter.
- OCR backlog lives in the database (`ocr_status = pending`), not in memory, so a backlog survives
  restarts and memory stays bounded. The OCR thread runs at below-normal priority.
- Each tick: read control state (pause/expiry), idle check, privacy check per monitor, then
  frames → change detection → persist or extend observation.
- Single instance: named mutex `Local\rsRewind.Recorder`.
- Heartbeat + counters to `recorder_status` every 5 s. Retention hourly.
- Ctrl+C / console close / `rsrewind stop` → graceful shutdown (end session, final heartbeat).

## rsrewind-cli

`clap` derive. `rsrewind daemon` (foreground), `start` (spawn detached daemon, no console
window), `stop`, `status [--json]`, `pause [--minutes N]`, `resume`, `search <text> [--app]
[--title] [--since] [--until] [--limit] [--json]`, `recent [--limit] [--json]`, `doctor [--json]`,
`ui`, `data-dir`. Human output matches the README example; `--json` emits the core types.

## rsrewind-ui

`windows-reactor` 0.100 (`Component`/`View`). Requires Windows App Runtime 2.4 — if missing,
reactor's own bootstrap shows the install prompt; the CLI must keep working regardless. Layout per
the brief: search box top, recent/day list left, results center, selected screenshot
(`Image` with `EncodedImage` WebP bytes) + timestamp/app/title/OCR text right. All data through
`rsrewind-query` on `spawn_background`; no SQL in the UI crate.

## Review amendments (2026-09-30, after Astra ultra review of aa2c5e7)

The review (`docs/reviews/2026-09-30-astra-ultra-mvp.md`) found the contract itself wrong in
several places. These amendments supersede the sections above where they conflict. Finding
numbers refer to that review. Every fix lands with a test that fails on aa2c5e7.

**Identity (F1).** `visual_states` and `events` ids are never reused (`INTEGER PRIMARY KEY
AUTOINCREMENT`). Schema v1 is unreleased, so `0001_initial.sql` is edited in place rather than
migrated. OCR results are saved against `(id, media_path)`: if the row's path differs, the result
is discarded.

**Privacy provenance (F2, F4, F21).** A frame may be stored only if the windows on its monitor
were checked *both before and after* the frame was taken and both checks allowed it, and the
frame was produced after the most recent excluded check on that monitor (frames carry their WGC
capture time; a monitor's "clear since" mark is reset by any exclusion). Any check that cannot be
completed (enumeration failure, unreadable process, title read error while title rules exist,
idle query failure) is treated as excluded for that tick: **unknown means do not record.**
Subject attribution uses window identity (HWND), not process id.

**Pause is a barrier (F3).** `set_control` increments a control generation. The recorder writes
the generation it has applied into its heartbeat after the tick that read it finishes. `rsrewind
pause` waits (bounded, ~3 s) for that acknowledgement before saying "Paused"; if the recorder is
running and does not acknowledge, it says so instead of claiming success.

**Deletion is a lifecycle, not a row delete (F5–F8, F22).**
- `delete_range` records a durable *deletion fence* (since, until). Inserting a visual state or
  observation whose time falls inside any fence fails with a typed error; the writer then removes
  the file it wrote. Fences older than the retention horizon may be pruned.
- Deleting history also removes `windows` and `applications` rows no longer referenced by any
  event, and writers drop their caches when told (`StorageError` / feedback).
- SQLite `secure_delete` is on, FTS5 `secure-delete` is on, and a deletion ends with
  `wal_checkpoint(TRUNCATE)`. Pre-migration backups are not rewritten; `forget` names any that
  exist so the user can remove them.
- Media files are recognised by name (timestamp + monitor) as well as by row: deletion and
  retention also remove unreferenced media files in their time range, and stale temp files.
- Size retention counts the database, its WAL and every file under `media/`, not only live rows.

**Writer feedback (F9, F12).** Capture treats a frame as persisted only after the persist thread
acknowledges the commit; failures and invalidations (deletion, retention, fences) flow back and
force a re-persist of the current picture. A frame that changed but could not be queued stays
dirty until it is stored. Maintenance (retention) runs on a timer, not only when jobs arrive.

**Paths (F10).** Stored media paths must start with `media/`; no component of the resolved path
may be a reparse point (symlink/junction).

**Query semantics (F17–F20).** `at(t)` returns the observation covering `t` (latest start among
covering spans), else the nearest earlier one. Timeline paging uses a `(started_at, event_id)`
cursor. Search time filters match any observation span of a visual state that overlaps the range,
and the reported time/app/window come from the earliest *matching* observation. Multi-statement
reads (`visual_detail`) run in one read transaction.

**Smaller (F11, F13–F16).** The capture probe dumps pixels only with an explicit flag. WGC
delivery keeps a timestamp high-water mark across both drain paths. Console close/logoff waits
(bounded) for shutdown to finish. The GDI test helper validates dimensions. Backup destinations are
reserved exclusively before the backup is written.
