//! Gaps read from a real store: sessions, observations and markers written through
//! `rsrewind_storage` as the recorder writes them, then read through `QueryDb::gaps` and, after
//! export and import, through `History::gaps` on a central data folder.

use rsrewind_core::paths::media_relative_path;
use rsrewind_core::{
    BgraFrame, CaptureState, DataDir, EventKind, Gap, GapReason, MonitorInfo, SourceId, Timestamp,
};
use rsrewind_query::{History, QueryDb, SourceFilter};
use rsrewind_segment::Segment;
use rsrewind_storage::media::{encode_webp, write_webp_exclusive};
use rsrewind_storage::{
    ExportOptions, MIN_SETTLE_MS, NewVisualState, Observation, RecorderStatus, Store,
    import_into_root,
};

type Fallible<T> = Result<T, Box<dyn std::error::Error>>;

const T0: i64 = 1_790_000_000_000;
const SEC: i64 = 1_000;
const MIN_GAP: i64 = 30 * SEC;

fn frame(seed: u8) -> BgraFrame {
    BgraFrame {
        width: 4,
        height: 4,
        stride: 16,
        pixels: (0..64).map(|i| seed.wrapping_add(i)).collect(),
    }
}

/// One picture on screen from `from` to `to` (seconds after T0), observed every second.
fn hold(
    data: &DataDir,
    store: &Store,
    session: rsrewind_core::SessionId,
    from: i64,
    to: i64,
    seed: u8,
) -> Fallible<()> {
    let monitor = store.upsert_monitor(
        &MonitorInfo {
            device_name: "eDP-1".into(),
            left: 0,
            top: 0,
            width: 4,
            height: 4,
            dpi: 96,
            primary: true,
        },
        Timestamp(T0),
    )?;
    let bytes = encode_webp(&frame(seed), 80)?;
    let at = Timestamp(T0 + from * SEC);
    let relative = media_relative_path(at, monitor);
    write_webp_exclusive(data, &relative, &bytes)?;
    let state = store.insert_visual_state(&NewVisualState {
        monitor,
        captured_at: at,
        media_path: relative,
        width: 4,
        height: 4,
        byte_size: bytes.len() as u64,
        fingerprint: None,
        ocr_enabled: false,
    })?;
    for second in from..=to {
        store.record_observation(&Observation {
            session,
            monitor,
            visual_state: state,
            application: None,
            window: None,
            at: Timestamp(T0 + second * SEC),
            max_gap_ms: 5 * SEC,
        })?;
    }
    Ok(())
}

fn at(seconds: i64) -> Timestamp {
    Timestamp(T0 + seconds * SEC)
}

/// Session 1: recording 0-20 s, idle from 20 s, stopped cleanly at 60 s.
/// Session 2: started at 300 s, recording to 320 s, paused at 320 s and still paused.
fn record(data: &DataDir) -> Fallible<Store> {
    let store = Store::open(data)?;
    let one = store.begin_session(at(0), "host", "test")?;
    store.record_marker(one, EventKind::RecorderStart, at(0), None)?;
    hold(data, &store, one, 0, 20, 1)?;
    store.record_marker(one, EventKind::IdleStart, at(20), None)?;
    store.record_marker(one, EventKind::RecorderStop, at(60), None)?;
    store.end_session(one, at(60))?;

    let two = store.begin_session(at(300), "host", "test")?;
    store.record_marker(two, EventKind::RecorderStart, at(300), None)?;
    hold(data, &store, two, 300, 320, 2)?;
    store.record_marker(two, EventKind::Paused, at(320), None)?;
    Ok(store)
}

fn reasons(gaps: &[Gap]) -> Vec<(i64, i64, GapReason)> {
    gaps.iter()
        .map(|g| ((g.from.0 - T0) / SEC, (g.to.0 - T0) / SEC, g.reason))
        .collect()
}

/// A recorder heartbeat written now, as a running recorder writes one every 5 s.
fn beat(store: &Store, state: CaptureState) -> Fallible<()> {
    beat_ago(store, state, 0)
}

fn beat_ago(store: &Store, state: CaptureState, ago_ms: i64) -> Fallible<()> {
    let now = Timestamp(Timestamp::now().0 - ago_ms);
    store.write_status(&RecorderStatus {
        pid: 1,
        started_at: now,
        heartbeat_at: now,
        state,
        counters: Default::default(),
    })?;
    Ok(())
}

#[test]
fn a_store_explains_its_own_gaps() -> Fallible<()> {
    let dir = tempfile::tempdir()?;
    let data = DataDir::new(dir.path());
    let store = record(&data)?;
    // Without a heartbeat the still-open session 2 is a recorder that died at its last write.
    let dead = QueryDb::open(&data)?.gaps(at(0), at(600), MIN_GAP)?;
    assert_eq!(reasons(&dead)[2], (320, 600, GapReason::RecorderDied));
    // A fresh heartbeat says it is running (and paused); a final "stopped" beat does not.
    beat(&store, CaptureState::Stopped)?;
    let stopped = QueryDb::open(&data)?.gaps(at(0), at(600), MIN_GAP)?;
    assert_eq!(reasons(&stopped)[2].2, GapReason::RecorderDied);
    // A heartbeat a minute old is a recorder that stopped beating.
    beat_ago(&store, CaptureState::Recording, 60 * SEC)?;
    let stale = QueryDb::open(&data)?.gaps(at(0), at(600), MIN_GAP)?;
    assert_eq!(reasons(&stale)[2].2, GapReason::RecorderDied);
    beat(&store, CaptureState::Paused { until: None })?;
    let db = QueryDb::open(&data)?;
    let gaps = db.gaps(at(0), at(600), MIN_GAP)?;
    assert_eq!(
        reasons(&gaps),
        vec![
            (20, 60, GapReason::Idle),
            (60, 300, GapReason::RecorderOff),
            (320, 600, GapReason::Paused),
        ]
    );
    assert!(gaps.iter().all(|g| g.source.is_none()));

    // A range that starts inside the idle stretch knows it was idle (markers before the range).
    assert_eq!(
        reasons(&db.gaps(at(30), at(100), MIN_GAP)?),
        vec![(30, 60, GapReason::Idle), (60, 100, GapReason::RecorderOff)]
    );
    // Gap JSON is stable, snake_case, and omits the local source.
    let json = serde_json::to_value(&gaps[1])?;
    assert_eq!(json["reason"], "recorder_off");
    assert!(json.get("source").is_none());
    Ok(())
}

#[test]
fn imported_history_keeps_its_gaps_and_stops_at_the_last_delivery() -> Fallible<()> {
    let probe_dir = tempfile::tempdir()?;
    let probe = DataDir::new(probe_dir.path());
    let store = record(&probe)?;
    let outbox = tempfile::tempdir()?;
    let central_dir = tempfile::tempdir()?;
    let central = DataDir::new(central_dir.path());
    let mut source = None;
    while let Some(report) = store.export_segment(
        outbox.path(),
        &ExportOptions {
            label: "probe",
            platform: "test",
            app_version: "0.0.4",
            capabilities: &[],
            now: at(3_600),
            settle_ms: MIN_SETTLE_MS,
            max_events: 1_000,
        },
    )? {
        let segment = Segment::open(&report.path)?;
        source = SourceId::parse(&segment.manifest().source.id);
        import_into_root(&central, &segment)?;
    }
    let source = source.ok_or("nothing exported")?;

    let history = History::open(&central);
    let gaps = history.gaps(SourceFilter::All, at(0), at(3_600), MIN_GAP)?;
    // The probe's last write was the pause at 320 s: after it, history has not arrived, which is
    // not a gap the probe had.
    assert_eq!(
        reasons(&gaps),
        vec![(20, 60, GapReason::Idle), (60, 300, GapReason::RecorderOff)]
    );
    assert!(gaps.iter().all(|g| g.source == Some(source)));
    Ok(())
}
