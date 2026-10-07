//! The milestone, end to end: a probe records, seals a segment, a central instance imports it,
//! and the *unchanged* query layer answers from the replica. No SQL, no replication knowledge on
//! the read side.

use rsrewind_core::paths::media_relative_path;
use rsrewind_core::{
    ApplicationContext, BgraFrame, DataDir, MonitorInfo, OcrBlock, SearchQuery, Timestamp,
    WindowContext,
};
use rsrewind_query::QueryDb;
use rsrewind_segment::Segment;
use rsrewind_storage::media::{encode_webp, write_webp_exclusive};
use rsrewind_storage::{
    ExportOptions, ImportOutcome, MIN_SETTLE_MS, NewVisualState, Observation, Store,
    import_into_root,
};

type TestResult = Result<(), Box<dyn std::error::Error>>;

const T0: i64 = 1_790_000_000_000;

fn frame(seed: u8) -> BgraFrame {
    let (width, height) = (32u32, 16u32);
    let mut pixels = Vec::new();
    for y in 0..height {
        for x in 0..width {
            pixels.extend_from_slice(&[
                seed.wrapping_add(x as u8),
                seed.wrapping_add(y as u8),
                seed,
                255,
            ]);
        }
    }
    BgraFrame {
        width,
        height,
        stride: width * 4,
        pixels,
    }
}

#[test]
fn a_probes_history_is_searchable_on_the_central_instance_without_duplication() -> TestResult {
    let tmp = tempfile::tempdir()?;
    let probe = DataDir::new(tmp.path().join("probe"));
    let central = DataDir::new(tmp.path().join("central"));
    let outbox = tmp.path().join("outbox");

    // --- the probe records two moments ----------------------------------------------------
    let store = Store::open(&probe)?;
    let session = store.begin_session(Timestamp(T0), "FRONT-DESK-PC", "0.0.1")?;
    let monitor = store.upsert_monitor(
        &MonitorInfo {
            device_name: r"\\.\DISPLAY1".into(),
            left: 0,
            top: 0,
            width: 1920,
            height: 1080,
            dpi: 96,
            primary: true,
        },
        Timestamp(T0),
    )?;
    let app = store.upsert_application(
        &ApplicationContext {
            process_name: "Chrome.exe".into(),
            exe_path: None,
        },
        Timestamp(T0),
    )?;
    let window = store.upsert_window(
        app,
        &WindowContext {
            title: "Konica printer - Settings".into(),
            class_name: None,
        },
        Timestamp(T0),
    )?;
    let mut frames = Vec::new();
    for (i, text) in ["Konica bizhub C360", "Invoice 4471"].iter().enumerate() {
        let at = T0 + (i as i64) * 30_000;
        let bytes = encode_webp(&frame(i as u8 * 40), 80)?;
        let relative = media_relative_path(Timestamp(at), monitor);
        write_webp_exclusive(&probe, &relative, &bytes)?;
        let id = store.insert_visual_state(&NewVisualState {
            monitor,
            captured_at: Timestamp(at),
            media_path: relative.clone(),
            width: 32,
            height: 16,
            byte_size: bytes.len() as u64,
            fingerprint: None,
            ocr_enabled: true,
        })?;
        store.save_ocr(
            id,
            &relative,
            &[OcrBlock {
                text: (*text).into(),
                x: 0.0,
                y: 0.0,
                width: 10.0,
                height: 4.0,
                confidence: None,
                line_index: 0,
            }],
            "test",
            1,
        )?;
        store.record_observation(&Observation {
            session,
            monitor,
            visual_state: id,
            application: Some(app),
            window: Some(window),
            at: Timestamp(at),
            max_gap_ms: 5_000,
        })?;
        frames.push(bytes);
    }

    // --- seal, "transfer" (a plain file copy: transport is not our business), import --------
    let report = store
        .export_segment(
            &outbox,
            &ExportOptions {
                label: "FRONT-DESK-PC",
                platform: "test",
                app_version: "0.0.1",
                capabilities: &["ocr", "window_titles"],
                now: Timestamp(T0 + 10 * MIN_SETTLE_MS),
                settle_ms: MIN_SETTLE_MS,
                max_events: 100,
            },
        )?
        .ok_or("nothing sealed")?;
    let delivered = tmp.path().join("delivered.rsseg");
    std::fs::copy(&report.path, &delivered)?;
    let segment = Segment::open(&delivered)?;
    let source = segment.manifest().source.id.clone();
    assert!(matches!(
        import_into_root(&central, &segment)?,
        ImportOutcome::Imported(_)
    ));
    // A retry, a duplicate delivery, a second copy: all harmless.
    for _ in 0..2 {
        assert!(matches!(
            import_into_root(&central, &segment)?,
            ImportOutcome::AlreadyImported { .. }
        ));
    }

    // --- the central instance reads it through the ordinary query API ------------------------
    let replica = central.source_dir(&source).ok_or("source dir")?;
    let db = QueryDb::open(&replica)?;
    let hits = db.search(&SearchQuery {
        text: "konica".into(),
        limit: 10,
        ..SearchQuery::default()
    })?;
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].application.as_deref(), Some("Chrome.exe"));
    assert_eq!(
        hits[0].window_title.as_deref(),
        Some("Konica printer - Settings")
    );
    assert_eq!(hits[0].timestamp, Timestamp(T0));
    assert_eq!(std::fs::read(&hits[0].media_path)?, frames[0]);

    let recent = db.recent(10, None)?;
    assert_eq!(
        recent.len(),
        2,
        "duplicate deliveries must not duplicate history"
    );
    let detail = db
        .visual_detail(recent[1].visual_state_id)?
        .ok_or("detail")?;
    assert_eq!(detail.ocr_text, "Konica bizhub C360");
    assert_eq!(
        db.at(Timestamp(T0 + 30_000))?.map(|e| e.started_at),
        Some(Timestamp(T0 + 30_000))
    );
    Ok(())
}
