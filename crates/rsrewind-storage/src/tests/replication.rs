//! Segment export/import: the correctness boundaries of probe -> central replication.

use super::*;
use crate::{ExportOptions, ImportOutcome, MIN_SETTLE_MS, SourceRole, import_into_root};
use rsrewind_segment::{Manifest, Segment, write_segment};
use std::path::{Path, PathBuf};

const SETTLE: i64 = MIN_SETTLE_MS;

fn options(now: i64) -> ExportOptions<'static> {
    ExportOptions {
        label: "probe-1",
        platform: "test",
        app_version: "0.0.1",
        capabilities: &["ocr"],
        now: Timestamp(now),
        settle_ms: SETTLE,
        max_events: 1_000,
    }
}

fn open_segment(path: &Path) -> Result<Segment, Box<dyn std::error::Error>> {
    Ok(Segment::open(path)?)
}

fn central() -> Result<(tempfile::TempDir, DataDir), Box<dyn std::error::Error>> {
    let tmp = tempfile::tempdir()?;
    let root = DataDir::new(tmp.path().join("central"));
    Ok((tmp, root))
}

fn replica_store(root: &DataDir, segment: &Segment) -> Result<Store, Box<dyn std::error::Error>> {
    let dir = root
        .source_dir(&segment.manifest().source.id)
        .ok_or("bad source id")?;
    Ok(Store::open(&dir)?)
}

fn imported(outcome: ImportOutcome) -> Result<crate::ImportReport, Box<dyn std::error::Error>> {
    match outcome {
        ImportOutcome::Imported(report) => Ok(report),
        other => Err(format!("expected a fresh import, got {other:?}").into()),
    }
}

/// One observation with OCR at `at`, on the fixture's monitor, returning (state, event).
fn record(
    f: &Fixture,
    at: i64,
    seed: u8,
    text: &str,
) -> Result<(VisualStateId, i64), Box<dyn std::error::Error>> {
    let window = f.window("Teams.exe", "Chat")?;
    let state = f.persist(f.monitor, at, seed)?;
    let rel = f.rel(state)?;
    f.store
        .save_ocr(state, &rel, &[block(text, 0)], "test", 3)?;
    let event = f.observe(f.monitor, state, Some(window), at)?;
    Ok((state, event))
}

fn export_one(
    f: &Fixture,
    out: &Path,
    now: i64,
) -> Result<crate::ExportReport, Box<dyn std::error::Error>> {
    f.store
        .export_segment(out, &options(now))?
        .ok_or_else(|| "expected a segment".into())
}

// ----- identity ------------------------------------------------------------------------------

#[test]
fn a_store_mints_one_stable_random_identity() -> TestResult {
    let a = fixture()?;
    let b = fixture()?;
    let id = a.store.source_identity()?;
    assert_eq!(id.role, SourceRole::Origin);
    assert!(rsrewind_segment::is_valid_source_id(&id.id));
    assert_eq!(a.store.source_identity()?, id, "identity must be stable");
    assert_ne!(
        b.store.source_identity()?.id,
        id.id,
        "two installs share an id"
    );

    let Fixture {
        _tmp, data, store, ..
    } = a;
    drop(store);
    assert_eq!(
        Store::open(&data)?.source_identity()?.id,
        id.id,
        "lost across reopen"
    );
    Ok(())
}

// ----- round trip ----------------------------------------------------------------------------

#[test]
fn a_sealed_segment_imports_with_everything_intact() -> TestResult {
    let f = fixture()?;
    let (state, _) = record(&f, T0 + 1_000, 7, "quarterly budget")?;
    let original = std::fs::read(f.media_file(state)?)?;
    let out = f._tmp.path().join("outbox");
    let report = export_one(&f, &out, T0 + 10 * SETTLE)?;
    assert_eq!((report.seq, report.events, report.states), (1, 1, 1));

    let (_keep, root) = central()?;
    let segment = open_segment(&report.path)?;
    let result = imported(import_into_root(&root, &segment)?)?;
    assert_eq!((result.events, result.states_new), (1, 1));

    let replica = replica_store(&root, &segment)?;
    assert_eq!(replica.source_identity()?.id, f.store.source_identity()?.id);
    assert_eq!(replica.source_identity()?.role, SourceRole::Replica);
    // Pixels are byte-identical, the text is searchable, the context survived.
    let copied: String =
        replica
            .conn()
            .query_row("SELECT media_path FROM visual_states", [], |r| r.get(0))?;
    let copied_bytes = std::fs::read(
        root.source_dir(&result.source_id)
            .ok_or("dir")?
            .resolve_media(&copied)
            .ok_or("path")?,
    )?;
    assert_eq!(copied_bytes, original);
    assert_eq!(fts_matches(&replica, "\"quarterly\"")?.len(), 1);
    let (process, title): (String, String) = replica.conn().query_row(
        "SELECT a.process_name, w.title FROM events e
         JOIN applications a ON a.id = e.application_id
         JOIN windows w ON w.id = e.window_id",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    assert_eq!((process.as_str(), title.as_str()), ("Teams.exe", "Chat"));
    let (started, ended): (i64, i64) =
        replica
            .conn()
            .query_row("SELECT started_at, ended_at FROM events", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })?;
    assert_eq!((started, ended), (T0 + 1_000, T0 + 1_000));
    Ok(())
}

#[test]
fn importing_twice_changes_nothing() -> TestResult {
    let f = fixture()?;
    record(&f, T0 + 1_000, 1, "alpha")?;
    let report = export_one(&f, &f._tmp.path().join("outbox"), T0 + 10 * SETTLE)?;
    let (_keep, root) = central()?;
    let segment = open_segment(&report.path)?;
    imported(import_into_root(&root, &segment)?)?;

    let replica = replica_store(&root, &segment)?;
    let before = (
        replica
            .conn()
            .query_row("SELECT COUNT(*) FROM events", [], |r| r.get::<_, i64>(0))?,
        replica
            .conn()
            .query_row("SELECT COUNT(*) FROM visual_states", [], |r| {
                r.get::<_, i64>(0)
            })?,
        replica
            .conn()
            .query_row("SELECT COUNT(*) FROM sessions", [], |r| r.get::<_, i64>(0))?,
    );
    for _ in 0..3 {
        assert!(matches!(
            import_into_root(&root, &segment)?,
            ImportOutcome::AlreadyImported { seq: 1, .. }
        ));
    }
    let after = (
        replica
            .conn()
            .query_row("SELECT COUNT(*) FROM events", [], |r| r.get::<_, i64>(0))?,
        replica
            .conn()
            .query_row("SELECT COUNT(*) FROM visual_states", [], |r| {
                r.get::<_, i64>(0)
            })?,
        replica
            .conn()
            .query_row("SELECT COUNT(*) FROM sessions", [], |r| r.get::<_, i64>(0))?,
    );
    assert_eq!(before, after);
    assert_eq!(replica.imported_segments()?.len(), 1);
    Ok(())
}

#[test]
fn same_sequence_different_content_is_a_conflict_and_imports_nothing() -> TestResult {
    let f = fixture()?;
    record(&f, T0 + 1_000, 1, "alpha")?;
    let out = f._tmp.path().join("outbox");
    let report = export_one(&f, &out, T0 + 10 * SETTLE)?;
    let (_keep, root) = central()?;
    let genuine = open_segment(&report.path)?;
    imported(import_into_root(&root, &genuine)?)?;

    // A different file claiming the same (source, seq): a cloned or restored identity.
    let mut forged: Manifest = genuine.manifest().clone();
    forged.events[0].ended_at += 1;
    let forged_path = f._tmp.path().join("forged.rsseg");
    write_segment(&forged_path, &forged, |i| {
        genuine.read_blob(i).map_err(std::io::Error::other)
    })?;
    let result = import_into_root(&root, &open_segment(&forged_path)?);
    assert!(
        matches!(result, Err(StorageError::SegmentConflict { seq: 1, .. })),
        "{result:?}"
    );

    let replica = replica_store(&root, &genuine)?;
    let ended: i64 = replica
        .conn()
        .query_row("SELECT ended_at FROM events", [], |r| r.get(0))?;
    assert_eq!(ended, T0 + 1_000, "the conflicting segment altered history");
    Ok(())
}

// ----- attribution ---------------------------------------------------------------------------

#[test]
fn a_replica_refuses_history_from_another_source() -> TestResult {
    let a = fixture()?;
    let b = fixture()?;
    record(&a, T0 + 1_000, 1, "from a")?;
    record(&b, T0 + 1_000, 2, "from b")?;
    let seg_a = open_segment(&export_one(&a, &a._tmp.path().join("o"), T0 + 10 * SETTLE)?.path)?;
    let seg_b = open_segment(&export_one(&b, &b._tmp.path().join("o"), T0 + 10 * SETTLE)?.path)?;

    let (_keep, root) = central()?;
    imported(import_into_root(&root, &seg_a)?)?;
    // Force B's segment at A's store: the identity check must stop it.
    let store_a = replica_store(&root, &seg_a)?;
    let result = store_a.import_segment(&seg_b);
    assert!(
        matches!(result, Err(StorageError::SourceMismatch { .. })),
        "{result:?}"
    );
    // Through the normal entry point B gets its own, separate store.
    imported(import_into_root(&root, &seg_b)?)?;
    let dirs = std::fs::read_dir(root.sources_root())?.count();
    assert_eq!(dirs, 2);
    Ok(())
}

#[test]
fn a_locally_recorded_store_refuses_imports_and_a_replica_refuses_to_export() -> TestResult {
    let origin = fixture()?;
    let other = fixture()?;
    record(&other, T0 + 1_000, 1, "x")?;
    let seg =
        open_segment(&export_one(&other, &other._tmp.path().join("o"), T0 + 10 * SETTLE)?.path)?;
    // Mints the origin identity first, as a recorder would.
    origin.store.source_identity()?;
    assert!(matches!(
        origin.store.import_segment(&seg),
        Err(StorageError::SourceMismatch { .. })
    ));

    let (_keep, root) = central()?;
    imported(import_into_root(&root, &seg)?)?;
    let replica = replica_store(&root, &seg)?;
    assert!(matches!(
        replica.export_segment(&root.outbox(), &options(T0 + 10 * SETTLE)),
        Err(StorageError::NotAnOrigin(_))
    ));
    Ok(())
}

#[test]
fn hostile_source_ids_never_become_paths() {
    let root = DataDir::new("/data");
    for bad in ["..", "../x", "", "ABCDEF0123456789ABCDEF0123456789", "a/b"] {
        assert!(root.source_dir(bad).is_none(), "{bad:?}");
    }
}

// ----- sealing rules -------------------------------------------------------------------------

#[test]
fn nothing_is_sealed_until_it_has_settled() -> TestResult {
    let f = fixture()?;
    record(&f, T0 + 1_000, 1, "fresh")?;
    let out = f._tmp.path().join("outbox");
    // Cutoff is before the event's end: nothing eligible.
    assert!(
        f.store
            .export_segment(&out, &options(T0 + 1_000 + SETTLE - 1))?
            .is_none()
    );
    // And exactly at the boundary it is.
    let report = f
        .store
        .export_segment(&out, &options(T0 + 1_000 + SETTLE))?;
    assert!(report.is_some());
    // A settle time below the safety minimum is refused outright.
    let mut too_eager = options(T0 + 2 * SETTLE);
    too_eager.settle_ms = 1;
    assert!(matches!(
        f.store.export_segment(&out, &too_eager),
        Err(StorageError::InvalidArgument(_))
    ));
    Ok(())
}

#[test]
fn every_event_is_sealed_exactly_once_across_exports() -> TestResult {
    let f = fixture()?;
    let out = f._tmp.path().join("outbox");
    let mut all = Vec::new();
    for round in 0..3i64 {
        for i in 0..4i64 {
            let at = T0 + round * 10 * SETTLE + i * 10_000;
            let (_, event) = record(&f, at, (round * 4 + i) as u8, "text")?;
            all.push(event);
        }
        let now = T0 + round * 10 * SETTLE + 5 * SETTLE;
        let report = export_one(&f, &out, now)?;
        assert_eq!(report.seq as i64, round + 1);
        assert_eq!(report.events, 4);
    }
    // Nothing new settled: no segment, and no seq burned.
    assert!(
        f.store
            .export_segment(&out, &options(T0 + 100 * SETTLE))?
            .is_none()
    );

    let (_keep, root) = central()?;
    let mut paths: Vec<PathBuf> = std::fs::read_dir(&out)?
        .map(|e| e.map(|e| e.path()))
        .collect::<Result<_, _>>()?;
    paths.sort();
    assert_eq!(paths.len(), 3);
    let mut total = 0;
    for path in &paths {
        total += imported(import_into_root(&root, &open_segment(path)?)?)?.events;
    }
    assert_eq!(total, all.len());
    Ok(())
}

#[test]
fn a_long_unchanged_observation_does_not_hold_back_later_history() -> TestResult {
    let f = fixture()?;
    let out = f._tmp.path().join("outbox");
    let second = f
        .store
        .upsert_monitor(&monitor_info(r"\\.\DISPLAY2"), Timestamp(T0))?;
    // Monitor 1 shows one picture for an hour and keeps being extended...
    let long = f.persist(f.monitor, T0, 1)?;
    for step in 0..=720i64 {
        f.observe(f.monitor, long, None, T0 + step * 5_000)?;
    }
    // ...while monitor 2 changes twice early on and then settles.
    let s1 = f.persist(second, T0 + 1_000, 2)?;
    f.observe(second, s1, None, T0 + 1_000)?;
    let now = T0 + 720 * 5_000 + 1_000;
    let first = export_one(&f, &out, now)?;
    assert_eq!(first.events, 1, "only the settled event should leave");

    // Once the long observation finally settles it follows, exactly once.
    let later = export_one(&f, &out, now + SETTLE + 10_000)?;
    assert_eq!(later.events, 1);
    assert!(
        f.store
            .export_segment(&out, &options(now + 100 * SETTLE))?
            .is_none()
    );
    Ok(())
}

#[test]
fn a_big_backlog_splits_without_splitting_simultaneous_events() -> TestResult {
    let f = fixture()?;
    let out = f._tmp.path().join("outbox");
    let second = f
        .store
        .upsert_monitor(&monitor_info(r"\\.\DISPLAY2"), Timestamp(T0))?;
    // Ten instants, two monitors each: twenty events with pairwise-equal end times.
    for i in 0..10i64 {
        let at = T0 + i * 10_000;
        let a = f.persist(f.monitor, at, i as u8)?;
        f.observe(f.monitor, a, None, at)?;
        let b = f.persist(second, at, 100 + i as u8)?;
        f.observe(second, b, None, at)?;
    }
    let mut opts = options(T0 + 100 * SETTLE);
    opts.max_events = 5; // forces a cut in the middle of a pair
    let mut total = 0;
    let mut seen = std::collections::BTreeSet::new();
    while let Some(report) = f.store.export_segment(&out, &opts)? {
        let segment = open_segment(&report.path)?;
        for event in &segment.manifest().events {
            assert!(
                seen.insert((event.started_at, event.monitor)),
                "event sealed twice"
            );
        }
        total += report.events;
    }
    assert_eq!(total, 20, "events lost at a segment boundary");
    Ok(())
}

#[test]
fn a_crash_between_publishing_and_the_watermark_never_reseals_history() -> TestResult {
    let f = fixture()?;
    let out = f._tmp.path().join("outbox");
    record(&f, T0 + 1_000, 1, "one")?;
    let first = export_one(&f, &out, T0 + 10 * SETTLE)?;
    assert_eq!(first.seq, 1);

    // Simulate dying right after the file was published: the watermark never advanced.
    for key in ["export.cut_ms", "export.max_event_id", "export.next_seq"] {
        f.store
            .conn()
            .execute("DELETE FROM settings WHERE key = ?1", [key])?;
    }
    record(&f, T0 + 20 * SETTLE, 2, "two")?;
    let second = export_one(&f, &out, T0 + 30 * SETTLE)?;
    assert_eq!(second.seq, 2, "reused sequence 1 for different content");
    assert_eq!(second.events, 1, "the first event was sealed again");
    Ok(())
}

#[test]
fn a_corrupt_outbox_file_stops_export_instead_of_guessing() -> TestResult {
    let f = fixture()?;
    let out = f._tmp.path().join("outbox");
    record(&f, T0 + 1_000, 1, "one")?;
    let first = export_one(&f, &out, T0 + 10 * SETTLE)?;
    f.store
        .conn()
        .execute("DELETE FROM settings WHERE key = 'export.next_seq'", [])?;
    let mut bytes = std::fs::read(&first.path)?;
    let mid = bytes.len() / 2;
    bytes[mid] ^= 0xff;
    std::fs::write(&first.path, bytes)?;
    record(&f, T0 + 20 * SETTLE, 2, "two")?;
    assert!(matches!(
        f.store.export_segment(&out, &options(T0 + 30 * SETTLE)),
        Err(StorageError::OutboxCorrupt { .. })
    ));
    Ok(())
}

#[test]
fn history_forgotten_before_it_was_sealed_never_leaves_the_machine() -> TestResult {
    let f = fixture()?;
    record(&f, T0 + 1_000, 1, "secret plans")?;
    record(&f, T0 + 20_000, 2, "harmless")?;
    f.store
        .delete_range(Timestamp(T0), Timestamp(T0 + 10_000))?;
    let report = export_one(&f, &f._tmp.path().join("outbox"), T0 + 10 * SETTLE)?;
    assert_eq!(report.events, 1);
    let bytes = std::fs::read(&report.path)?;
    assert!(!bytes.windows(12).any(|w| w == b"secret plans"));
    Ok(())
}

#[test]
fn an_event_whose_image_is_gone_is_skipped_not_exported_dangling() -> TestResult {
    let f = fixture()?;
    let (state, _) = record(&f, T0 + 1_000, 1, "gone")?;
    record(&f, T0 + 20_000, 2, "kept")?;
    std::fs::remove_file(f.media_file(state)?)?;
    let report = export_one(&f, &f._tmp.path().join("outbox"), T0 + 10 * SETTLE)?;
    assert_eq!((report.events, report.skipped_missing_media), (1, 1));
    Ok(())
}

// ----- import validation and deletion --------------------------------------------------------

#[test]
fn a_state_reused_by_a_later_segment_maps_to_the_same_image() -> TestResult {
    let f = fixture()?;
    let out = f._tmp.path().join("outbox");
    let window = f.window("a.exe", "w")?;
    let state = f.persist(f.monitor, T0 + 1_000, 1)?;
    f.observe(f.monitor, state, Some(window), T0 + 1_000)?;
    let first = export_one(&f, &out, T0 + 5 * SETTLE)?;
    // The screen returns to the very same picture after a long gap: a new event, same state.
    f.observe(f.monitor, state, Some(window), T0 + 20 * SETTLE)?;
    let second = export_one(&f, &out, T0 + 30 * SETTLE)?;
    assert_eq!((first.events, second.events), (1, 1));

    let (_keep, root) = central()?;
    imported(import_into_root(&root, &open_segment(&first.path)?)?)?;
    let report = imported(import_into_root(&root, &open_segment(&second.path)?)?)?;
    assert_eq!((report.states_new, report.states_reused), (0, 1));
    let replica = replica_store(&root, &open_segment(&first.path)?)?;
    let distinct: i64 = replica.conn().query_row(
        "SELECT COUNT(DISTINCT visual_state_id) FROM events",
        [],
        |r| r.get(0),
    )?;
    assert_eq!(distinct, 1);
    Ok(())
}

#[test]
fn a_forget_on_the_central_side_holds_against_later_deliveries() -> TestResult {
    let f = fixture()?;
    let out = f._tmp.path().join("outbox");
    record(&f, T0 + 1_000, 1, "early")?;
    let first = export_one(&f, &out, T0 + 5 * SETTLE)?;
    record(&f, T0 + 3_000, 2, "also early")?;
    record(&f, T0 + 20 * SETTLE, 3, "late")?;
    let second = export_one(&f, &out, T0 + 40 * SETTLE)?;

    let (_keep, root) = central()?;
    let seg1 = open_segment(&first.path)?;
    imported(import_into_root(&root, &seg1)?)?;
    let replica = replica_store(&root, &seg1)?;
    replica.delete_range(Timestamp(T0), Timestamp(T0 + 10_000))?;
    assert_eq!(
        replica
            .conn()
            .query_row("SELECT COUNT(*) FROM events", [], |r| r.get::<_, i64>(0))?,
        0
    );

    // Segment 2 arrives afterwards and still contains an event from the forgotten interval.
    let report = imported(import_into_root(&root, &open_segment(&second.path)?)?)?;
    assert_eq!(report.skipped_fenced, 2, "event and image inside the fence");
    assert_eq!(report.events, 1);
    let times: Vec<i64> = {
        let mut stmt = replica.conn().prepare("SELECT started_at FROM events")?;
        stmt.query_map([], |r| r.get(0))?
            .collect::<Result<_, _>>()?
    };
    assert_eq!(times, vec![T0 + 20 * SETTLE]);
    Ok(())
}

#[test]
fn a_local_forget_after_export_does_not_reach_the_central_copy() -> TestResult {
    let f = fixture()?;
    record(&f, T0 + 1_000, 1, "kept centrally")?;
    let report = export_one(&f, &f._tmp.path().join("outbox"), T0 + 5 * SETTLE)?;
    let (_keep, root) = central()?;
    let seg = open_segment(&report.path)?;
    imported(import_into_root(&root, &seg)?)?;
    f.store
        .delete_range(Timestamp(T0), Timestamp(T0 + 100_000))?;
    assert_eq!(f.count("SELECT COUNT(*) FROM events")?, 0);
    let replica = replica_store(&root, &seg)?;
    assert_eq!(
        replica
            .conn()
            .query_row("SELECT COUNT(*) FROM events", [], |r| r.get::<_, i64>(0))?,
        1
    );
    Ok(())
}

#[test]
fn an_image_that_is_not_a_webp_of_the_declared_size_is_not_imported() -> TestResult {
    let f = fixture()?;
    record(&f, T0 + 1_000, 1, "good")?;
    record(&f, T0 + 20_000, 2, "bad")?;
    let report = export_one(&f, &f._tmp.path().join("outbox"), T0 + 10 * SETTLE)?;
    let genuine = open_segment(&report.path)?;
    // Same manifest, but the second image is garbage of the same length.
    let forged_path = f._tmp.path().join("forged.rsseg");
    write_segment(&forged_path, genuine.manifest(), |i| {
        if i == 1 {
            Ok(vec![0xAB; genuine.manifest().states[1].blob_len as usize])
        } else {
            genuine.read_blob(i).map_err(std::io::Error::other)
        }
    })?;
    let (_keep, root) = central()?;
    let result = imported(import_into_root(&root, &open_segment(&forged_path)?)?)?;
    assert_eq!(result.skipped_invalid_image, 1);
    assert_eq!(result.skipped_dangling, 1);
    assert_eq!((result.events, result.states_new), (1, 1));
    // No file for the bad image was left behind.
    let files = walk_files(&root.sources_root());
    assert_eq!(
        files
            .iter()
            .filter(|p| p.extension().is_some_and(|e| e == "webp"))
            .count(),
        1
    );
    Ok(())
}

#[test]
fn re_importing_after_a_crash_adopts_the_images_already_on_disk() -> TestResult {
    let f = fixture()?;
    record(&f, T0 + 1_000, 1, "x")?;
    let report = export_one(&f, &f._tmp.path().join("outbox"), T0 + 5 * SETTLE)?;
    let seg = open_segment(&report.path)?;
    let (_keep, root) = central()?;
    imported(import_into_root(&root, &seg)?)?;

    // Wipe the rows but keep the images: what a crash after the file writes would leave.
    let replica = replica_store(&root, &seg)?;
    replica.conn().execute_batch(
        "DELETE FROM events; DELETE FROM ocr_blocks; DELETE FROM ocr_fts;
         DELETE FROM visual_states; DELETE FROM settings WHERE key LIKE 'import.seq.%';",
    )?;
    let again = imported(import_into_root(&root, &seg)?)?;
    assert_eq!((again.events, again.states_new), (1, 1));
    Ok(())
}

#[test]
fn a_session_that_ends_later_is_updated_not_duplicated() -> TestResult {
    let f = fixture()?;
    let out = f._tmp.path().join("outbox");
    record(&f, T0 + 1_000, 1, "a")?;
    let first = export_one(&f, &out, T0 + 5 * SETTLE)?;
    f.store
        .end_session(f.session, Timestamp(T0 + 30 * SETTLE))?;
    record(&f, T0 + 20 * SETTLE, 2, "b")?;
    let second = export_one(&f, &out, T0 + 40 * SETTLE)?;

    let (_keep, root) = central()?;
    let seg1 = open_segment(&first.path)?;
    imported(import_into_root(&root, &seg1)?)?;
    imported(import_into_root(&root, &open_segment(&second.path)?)?)?;
    let replica = replica_store(&root, &seg1)?;
    let (count, ended): (i64, Option<i64>) =
        replica
            .conn()
            .query_row("SELECT COUNT(*), MAX(ended_at) FROM sessions", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })?;
    assert_eq!((count, ended), (1, Some(T0 + 30 * SETTLE)));
    Ok(())
}

fn walk_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                out.extend(walk_files(&path));
            } else {
                out.push(path);
            }
        }
    }
    out
}

// ----- forget and the outbox -----------------------------------------------------------------

#[test]
fn sealed_events_in_a_range_are_counted_before_they_are_deleted() -> TestResult {
    let f = fixture()?;
    let out = f._tmp.path().join("outbox");
    record(&f, T0 + 1_000, 1, "sealed")?;
    export_one(&f, &out, T0 + 5 * SETTLE)?;
    record(&f, T0 + 20 * SETTLE, 2, "not yet sealed")?;
    let all = (Timestamp(T0), Timestamp(T0 + 100 * SETTLE));
    assert_eq!(f.store.count_sealed_events(all.0, all.1)?, 1);
    assert_eq!(
        f.store
            .count_sealed_events(Timestamp(T0 + 10 * SETTLE), all.1)?,
        0
    );
    let fresh = fixture()?;
    record(&fresh, T0 + 1_000, 3, "never exported")?;
    assert_eq!(fresh.store.count_sealed_events(all.0, all.1)?, 0);
    assert_eq!(fresh.store.export_status()?.cut_ms, 0);
    Ok(())
}

#[test]
fn the_outbox_scan_reports_time_ranges_and_quarantines_bad_files() -> TestResult {
    let f = fixture()?;
    let out = f._tmp.path().join("outbox");
    record(&f, T0 + 1_000, 1, "a")?;
    let first = export_one(&f, &out, T0 + 5 * SETTLE)?;
    std::fs::write(out.join("seg-deadbeef-0000000009.rsseg"), b"junk")?;
    let scan = crate::scan_outbox(&out)?;
    assert_eq!(scan.segments.len(), 1);
    assert_eq!(scan.segments[0].path, first.path);
    assert_eq!(
        (scan.segments[0].first_ms, scan.segments[0].last_ms),
        (T0 + 1_000, T0 + 1_000)
    );
    assert_eq!(scan.unreadable.len(), 1);
    assert_eq!(
        crate::scan_outbox(&f._tmp.path().join("nowhere"))?,
        crate::OutboxScan::default()
    );
    Ok(())
}
