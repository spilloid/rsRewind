# Astra (gpt-6-astra, ultra, read-only) adversarial review of feat/mvp-bootstrap @ aa2c5e7

Recorded verbatim. Adjudication and fixes: docs/mvp-contract.md "Review amendments" and docs/dev-process.md.

Reviewed `feat/mvp-bootstrap` at `aa2c5e7`, covering all requested crates and the contract, excluding `rsrewind-ui`. No files were changed.

1. **Critical — OCR can resurrect forgotten text onto a different screenshot.**  
   Locations: [0001_initial.sql:44](C:/Users/kubert/Documents/GitHub/rsrewind/crates/rsrewind-storage/src/sql/0001_initial.sql:44), [store.rs:550](C:/Users/kubert/Documents/GitHub/rsrewind/crates/rsrewind-storage/src/store.rs:550).

   `visual_states.id` is a reusable SQLite rowid. Suppose OCR is processing state 42, `forget` deletes it and all later states, and persistence inserts a new screenshot as 42. `save_ocr` checks only that 42 exists, then attaches the forgotten screenshot’s text to the replacement. The same identity problem affects cached observation IDs and `mark_ocr`.

   **Fix:** use identities that cannot be reused, and validate an immutable job identity/generation when saving results. I reproduced the resurrection using the checked-in schema in memory; SQLite documents this [rowid reuse behavior](https://sqlite.org/autoinc.html).

2. **High — Privacy decisions are not associated with the frames they authorize.**  
   Locations: [recorder.rs:214](C:/Users/kubert/Documents/GitHub/rsrewind/crates/rsrewind-daemon/src/recorder.rs:214), [recorder.rs:274](C:/Users/kubert/Documents/GitHub/rsrewind/crates/rsrewind-daemon/src/recorder.rs:274), [wgc.rs:266](C:/Users/kubert/Documents/GitHub/rsrewind/crates/rsrewind-capture/src/wgc.rs:266).

   Window enumeration precedes retrieval from an independently running WGC pool. An excluded window can disappear before enumeration while its last frame remains buffered; the now-allowed tick persists that excluded frame. Conversely, an excluded window can appear after enumeration and be present in the retrieved frame. Clearing `last_frame` during an excluded tick does not invalidate subsequent arrivals in WGC’s slot. Frame timestamps are discarded before the daemon can check them.

   **Fix:** carry capture timestamps and privacy generations through the pipeline, invalidate buffered frames across privacy transitions, and reject frames whose capture interval cannot be established as allowed. **The contract’s “check privacy, then retrieve frame” design also needs revision.**

3. **High — `pause` reports success before recording is actually paused.**  
   Locations: [main.rs:358](C:/Users/kubert/Documents/GitHub/rsrewind/crates/rsrewind-cli/src/main.rs:358), [recorder.rs:108](C:/Users/kubert/Documents/GitHub/rsrewind/crates/rsrewind-daemon/src/recorder.rs:108).

   The recorder reads control once, then processes the tick. If `pause` commits after that read, the CLI immediately prints “Paused,” but the ongoing tick can retrieve and enqueue pixels captured after that message. Persistence never checks a control generation.

   **Fix:** make pause an acknowledged barrier: stop accepting frames, invalidate unauthorized work, and acknowledge completion before claiming recording has stopped. Polling a desired-state row alone cannot provide that guarantee; this is also a contract gap.

4. **High — Missing privacy information permits recording.**  
   Locations: [window.rs:219](C:/Users/kubert/Documents/GitHub/rsrewind/crates/rsrewind-capture/src/window.rs:219), [window.rs:238](C:/Users/kubert/Documents/GitHub/rsrewind/crates/rsrewind-capture/src/window.rs:238), [window.rs:329](C:/Users/kubert/Documents/GitHub/rsrewind/crates/rsrewind-capture/src/window.rs:329), [idle.rs:17](C:/Users/kubert/Documents/GitHub/rsrewind/crates/rsrewind-capture/src/idle.rs:17).

   Failed enumeration returns a partial or empty list; failed title reads become empty titles; failed process resolution becomes `"unknown"`. Those values ordinarily match no exclusion, so the monitor is allowed. A failed idle query similarly reports “active.” For example, an enumeration failure omitting a visible password manager results in its pixels being stored.

   **Fix:** distinguish “known empty/allowed” from “could not determine,” propagate uncertainty, and skip affected captures. The contract’s degraded-information API needs an explicit privacy policy for failures.

5. **High — Queued writes can recreate a forgotten range after deletion succeeds.**  
   Locations: [main.rs:442](C:/Users/kubert/Documents/GitHub/rsrewind/crates/rsrewind-cli/src/main.rs:442), [persist.rs:112](C:/Users/kubert/Documents/GitHub/rsrewind/crates/rsrewind-daemon/src/persist.rs:112), [store.rs:701](C:/Users/kubert/Documents/GitHub/rsrewind/crates/rsrewind-storage/src/store.rs:701).

   A `NewState` captured inside the requested range can be queued or encoding when another process runs `forget`. Deletion finds no row for it and returns successfully. Persistence subsequently writes its file, row, observation, and OCR. Extending `until` by one minute does not prevent later insertion.

   **Fix:** coordinate deletion with the writer and maintain a durable deletion generation/range fence checked by subsequent writes. The deletion contract currently omits this cross-process ordering requirement.

6. **High — Forgotten window titles remain in ordinary database rows indefinitely.**  
   Location: [store.rs:793](C:/Users/kubert/Documents/GitHub/rsrewind/crates/rsrewind-storage/src/store.rs:793).

   Deletion removes events, visual states, OCR, and files, but never removes `windows`, `applications`, or sessions. Record a uniquely titled sensitive document, then forget its entire history: `SELECT title FROM windows` still returns that title. Retention has the same omission.

   **Fix:** delete unreferenced captured metadata transactionally and invalidate corresponding persistence caches. **This is also a contract defect:** its deletion list omits these tables despite the CLI promising removal of window history.

7. **High — Successful logical deletion leaves recoverable OCR in FTS storage, WAL, and backups.**  
   Locations: [store.rs:845](C:/Users/kubert/Documents/GitHub/rsrewind/crates/rsrewind-storage/src/store.rs:845), [store.rs:1000](C:/Users/kubert/Documents/GitHub/rsrewind/crates/rsrewind-storage/src/store.rs:1000), [migrations.rs:158](C:/Users/kubert/Documents/GitHub/rsrewind/crates/rsrewind-storage/src/migrations.rs:158).

   Neither SQLite secure deletion nor FTS5 secure deletion is configured. There is no deletion-specific WAL checkpoint/truncation, and existing migration backups are untouched. Thus an empty search result is not evidence that forgotten text has disappeared from application-managed storage.

   I reproduced a deleted token remaining in FTS shadow blobs even with ordinary `PRAGMA secure_delete=ON`. FTS5 requires its [separate secure-delete setting](https://sqlite.org/fts5.html#the_secure_delete_configuration_option); [WAL preserves earlier page versions](https://sqlite.org/wal.html).

   **Fix:** define and implement an erasure protocol covering FTS, ordinary pages, WAL, and backups, with coordinated readers/writers. **The contract specifies logical deletion but the CLI promises permanence.**

8. **High — Interrupted or failed cleanup leaves screenshots that future `forget` calls cannot remove.**  
   Locations: [persist.rs:118](C:/Users/kubert/Documents/GitHub/rsrewind/crates/rsrewind-daemon/src/persist.rs:118), [store.rs:852](C:/Users/kubert/Documents/GitHub/rsrewind/crates/rsrewind-storage/src/store.rs:852).

   Two concrete paths leave untracked pixels:

   - A crash or insertion failure after publishing a WebP but before inserting its row.
   - A crash after deletion commits but before unlinking files, or an unlink failure.

   Subsequent deletion and retention discover files through database rows, so neither retries these files. `doctor` only reports them. Repeating `forget` can therefore succeed while the screenshot remains.

   **Fix:** persist file-creation/deletion intents and recover them after restart; maintain a durable deletion retry queue. File-before-row ordering is appropriate, but the contract also needs orphan recovery.

9. **High — Queue admission is mistaken for successful persistence, causing wrong spans and silent recording gaps.**  
   Locations: [recorder.rs:295](C:/Users/kubert/Documents/GitHub/rsrewind/crates/rsrewind-daemon/src/recorder.rs:295), [recorder.rs:324](C:/Users/kubert/Documents/GitHub/rsrewind/crates/rsrewind-daemon/src/recorder.rs:324), [persist.rs:114](C:/Users/kubert/Documents/GitHub/rsrewind/crates/rsrewind-daemon/src/persist.rs:114), [persist.rs:255](C:/Users/kubert/Documents/GitHub/rsrewind/crates/rsrewind-daemon/src/persist.rs:255).

   If changed frame B is dropped because the queue is full while A was previously persisted, the next tick without another WGC frame computes no fingerprint and extends A—even though cached/current pixels are B. The advertised retry does not happen.

   Conversely, an accepted B immediately updates the capture fingerprint. If writing B fails, persistence clears its current state but capture keeps sending ineffective extensions. Retention also clears every persistence current state without notifying capture, potentially stopping observations on unchanged monitors indefinitely.

   **Fix:** retain dirty candidates for retry, acknowledge successful commits, and propagate failures/deletions/retention invalidations back to capture.

10. **Medium — Media confinement checks strings, not the filesystem destination.**  
    Locations: [paths.rs:78](C:/Users/kubert/Documents/GitHub/rsrewind/crates/rsrewind-core/src/paths.rs:78), [media.rs:106](C:/Users/kubert/Documents/GitHub/rsrewind/crates/rsrewind-storage/src/media.rs:106).

    A path such as `media/link/secret.webp` passes validation when `link` is a junction pointing outside the data folder. Reads, writes, and deletion then follow it outside the root. A tampered row can also name `config.toml`, which passes validation and can be removed by `forget`, losing configured exclusions on the next startup.

    **Fix:** restrict media paths to the media subtree and enforce resolved containment using filesystem handles/reparse-point checks that avoid check/use races. The existing traversal tests cover only lexical escapes.

11. **Medium — The capture probe writes unrestricted screen pixels outside the data folder.**  
    Location: [probe.rs:104](C:/Users/kubert/Documents/GitHub/rsrewind/crates/rsrewind-capture/examples/probe.rs:104).

    Running the documented capture example while a password manager is visible writes its raw pixels to `%TEMP%\rsrewind-probe-m0.bgra`. It applies neither exclusions nor pause/idle controls and performs no cleanup. This affects the diagnostic example, not the daemon.

    **Fix:** make pixel dumping explicitly opt-in, apply privacy rules, and place any retained artifact under the managed data folder.

12. **Medium — Retention stops running when recording is paused, idle, or continuously excluded.**  
    Location: [persist.rs:72](C:/Users/kubert/Documents/GitHub/rsrewind/crates/rsrewind-daemon/src/persist.rs:72).

    Retention deadlines are checked only after receiving a job. After the initial pause/idle transition, capture can send no further jobs for days. The persist thread remains blocked in `recv`, retaining expired history despite the hourly-retention contract.

    **Fix:** use a receive timeout tied to the retention deadline and run maintenance independently of capture traffic.

13. **Medium — Concurrent WGC drains can deliver an older frame after a newer one.**  
    Locations: [wgc.rs:230](C:/Users/kubert/Documents/GitHub/rsrewind/crates/rsrewind-capture/src/wgc.rs:230), [wgc.rs:400](C:/Users/kubert/Documents/GitHub/rsrewind/crates/rsrewind-capture/src/wgc.rs:400).

    The callback can dequeue A and be descheduled before parking it. The capture thread then dequeues, parks, and takes newer B. When the callback resumes, the slot is empty, so its timestamp comparison accepts A. The next tick receives A and can record a visual regression.

    **Fix:** use one drain owner or retain a synchronized timestamp high-water mark that includes already-delivered frames, not just the currently parked frame.

14. **Medium — Closing the console bypasses the intended graceful shutdown.**  
    Location: [win.rs:93](C:/Users/kubert/Documents/GitHub/rsrewind/crates/rsrewind-daemon/src/win.rs:93).

    The handler sets an atomic and immediately returns `TRUE` for every control event. For `CTRL_CLOSE_EVENT`, Windows terminates the process when the handler returns; it does not grant the main loop another cleanup interval. Session closure, final heartbeat, and queue draining can therefore be skipped. This follows the documented [HandlerRoutine behavior](https://learn.microsoft.com/en-us/windows/console/handlerroutine).

    **Fix:** distinguish Ctrl+C from close/shutdown events, and keep the close handler waiting for a bounded cleanup-completion signal.

15. **Medium — The public GDI rendering helper can construct an out-of-bounds mutable slice.**  
    Location: [gdi_render.rs:37](C:/Users/kubert/Documents/GitHub/rsrewind/crates/rsrewind-ocr/src/gdi_render.rs:37).

    `render_text_to_frame("x", 1, u32::MAX)` casts height to `-1`, then negates it to produce a valid one-row DIB. However, `byte_len` uses the original unsigned height and constructs a roughly 16 GiB slice over the tiny allocation. `bits.fill` writes beyond it. This is reachable through a safe public helper, although the daemon does not call it.

    **Fix:** validate dimensions with checked signed conversions and checked allocation-size arithmetic before calling GDI or constructing slices.

16. **Medium — Concurrent migrations can overwrite the only pre-migration backup.**  
    Location: [migrations.rs:163](C:/Users/kubert/Documents/GitHub/rsrewind/crates/rsrewind-storage/src/migrations.rs:163).

    Backup filename selection uses `exists()` followed by an ordinary backup open. Two processes can select the same millisecond filename before either creates it. One can finish its backup and migration before the other opens the selected destination; the latter then overwrites the pre-migration backup with the migrated database.

    **Fix:** reserve backup destinations exclusively and serialize the backup/version-check/migration sequence across processes. The per-migration SQL lock does not protect backup selection.

17. **Medium — `at()` returns the wrong observation after 64 intervening events.**  
    Location: [query/lib.rs:207](C:/Users/kubert/Documents/GitHub/rsrewind/crates/rsrewind-query/src/lib.rs:207).

    Keep monitor A unchanged for ten minutes while monitor B produces more than 64 short observations. Ask for a time covered only by A after B stops. A is outside the 64-row candidate window, so `at()` returns an ended B observation instead. I reproduced this with the checked-in query shape in memory.

    **Fix:** query covering spans directly; only fall back to the nearest preceding observation when no covering span exists.

18. **Medium — Timeline pagination loses observations sharing a timestamp.**  
    Location: [query/lib.rs:192](C:/Users/kubert/Documents/GitHub/rsrewind/crates/rsrewind-query/src/lib.rs:192).

    Multiple monitors use the same tick timestamp. If a page ends between their observations, the next page’s strict `started_at < before` skips the remaining observations at that timestamp. With two monitors and `limit=1`, this occurs immediately; I reproduced it in memory.

    **Fix:** paginate by `(started_at, event_id)` and expose that cursor. **The contract’s timestamp-only paging API is insufficient for lossless pagination.**

19. **Medium — Search filters miss content displayed during the requested interval.**  
    Locations: [query/lib.rs:74](C:/Users/kubert/Documents/GitHub/rsrewind/crates/rsrewind-query/src/lib.rs:74), [query/lib.rs:84](C:/Users/kubert/Documents/GitHub/rsrewind/crates/rsrewind-query/src/lib.rs:84), [mvp-contract.md:125](C:/Users/kubert/Documents/GitHub/rsrewind/docs/mvp-contract.md:125).

    A screenshot first observed at 09:00 and continuously displayed until 10:00 disappears from `search --since 09:30`, because filtering examines only its first observation’s start. Later application/window observations of the same visual state are similarly ignored.

    **Fix:** qualify states using matching observation spans and choose context from a matching observation. **This is a contract/design defect implemented consistently by the code and tests.**

20. **Medium — `visual_detail()` can combine different database snapshots.**  
    Location: [query/lib.rs:227](C:/Users/kubert/Documents/GitHub/rsrewind/crates/rsrewind-query/src/lib.rs:227).

    Metadata, OCR status, and joined text are read in one statement; blocks are read in another without a transaction. If OCR commits between them, the result can contain `pending`, empty text, and completed blocks. Deletion or replacement can produce other inconsistent combinations.

    **Fix:** read the complete detail under one read transaction. Read-only connection flags do not make multiple statements one snapshot.

21. **Medium — Foreground context is assigned by process identity instead of window identity.**  
    Location: [recorder.rs:458](C:/Users/kubert/Documents/GitHub/rsrewind/crates/rsrewind-daemon/src/recorder.rs:458).

    Put two windows belonging to one process on different monitors and focus one. Both monitors satisfy the PID comparison, so both observations receive the focused window’s title, even though one screenshot depicts its sibling window.

    **Fix:** retain the foreground HWND or equivalent window identity and associate that actual window’s rectangle with each monitor.

22. **Medium — Size retention does not enforce the advertised database-plus-media footprint.**  
    Locations: [store.rs:742](C:/Users/kubert/Documents/GitHub/rsrewind/crates/rsrewind-storage/src/store.rs:742), [store.rs:911](C:/Users/kubert/Documents/GitHub/rsrewind/crates/rsrewind-storage/src/store.rs:911).

    Size enforcement subtracts free database pages and ignores WAL and untracked media. After a large database is pruned, its physical file can remain above a newly lowered cap indefinitely; the algorithm sees only its smaller live-page count and performs no compaction. Failed file deletion also reduces accounting without freeing those bytes.

    **Fix:** distinguish logical usage from disk footprint, account for pending/orphan files and WAL, and reclaim database space when enforcing a disk budget.

The following areas look sound within the reviewed paths:

- SQL values are parameterized; FTS operators and embedded quotes are safely escaped.
- Normal daemon logging does not directly emit captured titles, OCR text, or pixel buffers at info+.
- Pixel channels and the OCR batch are bounded; the title cache has an explicit cap.
- Normal mapped-texture copying respects row pitch, and owned Win32 handles generally use RAII.
- The pinned `windows-core` implementation supports the documented implicit-MTA fallback.
- Migration SQL/version updates are transactional, and backup failure prevents migration.
- UTC storage and media naming avoid DST ambiguity; local parsing explicitly handles missing and repeated times.

## Tests

**Grade: D overall; B for pure helpers and sequential storage/query coverage, F for recorder privacy/concurrency coverage.**

The suite meaningfully tests escaping, matching, geometry, basic migrations, spans, and logical deletion. It does not substantiate the strongest privacy guarantees:

- [plan.rs:127](C:/Users/kubert/Documents/GitHub/rsrewind/crates/rsrewind-daemon/src/plan.rs:127) tests decisions from supplied booleans, not buffered frames, control acknowledgements, queues, or persistence.
- [idle.rs:48](C:/Users/kubert/Documents/GitHub/rsrewind/crates/rsrewind-capture/src/idle.rs:48) asserts a bound guaranteed by the function’s `u32` arithmetic; it cannot detect broken idle detection.
- [tests.rs:898](C:/Users/kubert/Documents/GitHub/rsrewind/crates/rsrewind-storage/src/tests.rs:898) permits deleting every screenshot: an empty remainder is a valid newest suffix and satisfies the cap.
- [tests.rs:673](C:/Users/kubert/Documents/GitHub/rsrewind/crates/rsrewind-storage/src/tests.rs:673) calls OCR replacement “atomic” but injects no failure during replacement.

The most valuable additions are deterministic barriers around pause/exclusion/forget, OCR deletion-and-ID-reuse tests, queue-full and failed-write recovery, crash recovery at every file/row boundary, physical FTS/WAL/backup erasure checks, junction confinement, and multi-monitor timeline tests.

Validation here used source inspection and file-free SQLite reproductions, not a rebuilt Cargo suite. One prebuilt renderer test passed, but the checked-in [assertion](C:/Users/kubert/Documents/GitHub/rsrewind/crates/rsrewind-cli/src/render.rs:115) expects ellipses absent from its fixture and current renderer; that artifact cannot establish this source revision’s correctness.

**Overall verdict:** This revision is not ready to record sensitive desktops. Its component-level foundations are useful, but privacy decisions lack frame provenance, cross-process controls lack synchronization, and deletion lacks a complete lifecycle for identities, metadata, files, and database remnants. Address those boundaries before treating a green suite—or an empty search after `forget`—as evidence that the recorder honors its privacy promises.