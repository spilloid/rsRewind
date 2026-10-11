//! Marks and keyframes through the history facade, over stores built by the real writer
//! (docs/design/marks-keyframes.md). Raw SQL appears only to fake an older (v1) store.

use rsrewind_core::paths::media_relative_path;
use rsrewind_core::{
    ApplicationContext, BgraFrame, DataDir, MonitorId, MonitorInfo, SessionId, Timestamp,
    WindowContext,
};
use rsrewind_query::{History, KeyframeKind, QueryDb, SourceFilter};
use rsrewind_storage::media::{encode_webp, write_webp_exclusive};
use rsrewind_storage::{NewVisualState, Observation, Store};

type TestResult = Result<(), Box<dyn std::error::Error>>;
type Fallible<T> = Result<T, Box<dyn std::error::Error>>;

const T0: i64 = 1_790_000_000_000;
const SEC: i64 = 1_000;

struct Rec {
    store: Store,
    data: DataDir,
    session: SessionId,
    monitors: [MonitorId; 2],
}

fn monitor(name: &str, left: i32) -> MonitorInfo {
    MonitorInfo {
        device_name: name.into(),
        left,
        top: 0,
        width: 16,
        height: 8,
        dpi: 96,
        primary: left == 0,
    }
}

fn recorder(data: &DataDir) -> Fallible<Rec> {
    let store = Store::open(data)?;
    let session = store.begin_session(Timestamp(T0 - SEC), "host", "test")?;
    let monitors = [
        store.upsert_monitor(&monitor("m1", 0), Timestamp(T0))?,
        store.upsert_monitor(&monitor("m2", 16), Timestamp(T0))?,
    ];
    Ok(Rec {
        store,
        data: data.clone(),
        session,
        monitors,
    })
}

impl Rec {
    /// One observation of a fresh picture at `at` (ms after T0) on monitor `m`, in `app`/`title`,
    /// with dHash `fp`, lasting `secs`.
    fn moment(
        &self,
        at: i64,
        m: usize,
        app: &str,
        title: &str,
        fp: u64,
        secs: i64,
    ) -> Fallible<()> {
        let at = T0 + at;
        let monitor = self.monitors[m];
        let frame = BgraFrame {
            width: 16,
            height: 8,
            stride: 64,
            pixels: vec![(fp % 251) as u8; 16 * 8 * 4],
        };
        let bytes = encode_webp(&frame, 80)?;
        let relative = media_relative_path(Timestamp(at), monitor);
        write_webp_exclusive(&self.data, &relative, &bytes)?;
        let state = self.store.insert_visual_state(&NewVisualState {
            monitor,
            captured_at: Timestamp(at),
            media_path: relative,
            width: 16,
            height: 8,
            byte_size: bytes.len() as u64,
            fingerprint: Some(fp),
            ocr_enabled: false,
        })?;
        let application = self.store.upsert_application(
            &ApplicationContext {
                process_name: app.into(),
                exe_path: None,
            },
            Timestamp(at),
        )?;
        let window = self.store.upsert_window(
            application,
            &WindowContext {
                title: title.into(),
                class_name: None,
            },
            Timestamp(at),
        )?;
        for tick in 0..=secs {
            self.store.record_observation(&Observation {
                session: self.session,
                monitor,
                visual_state: state,
                application: Some(application),
                window: Some(window),
                at: Timestamp(at + tick * SEC),
                max_gap_ms: 2 * SEC,
            })?;
        }
        Ok(())
    }
}

const A: u64 = 0x0f0f_0f0f_0f0f_0f0f;

/// A morning: editor on m1 and mail on m2 side by side, a window switch, an app switch, a new
/// screen in the same window, then ten minutes of nothing, then work again.
fn morning(data: &DataDir) -> Fallible<Rec> {
    let rec = recorder(data)?;
    rec.moment(0, 0, "code", "main.rs", A, 1)?;
    rec.moment(500, 1, "mail", "Inbox", A ^ 1, 1)?;
    rec.moment(2_000, 0, "code", "main.rs", A ^ 0b11, 1)?; // an edit: nothing
    rec.moment(2_500, 1, "mail", "Inbox", A ^ 0b101, 1)?; // other monitor, same app: nothing
    rec.moment(30_000, 0, "code", "lib.rs", A ^ 0b111, 1)?; // window switch
    rec.moment(60_000, 0, "browser", "Docs", A ^ 0xff, 1)?; // app switch
    rec.moment(90_000, 0, "browser", "Docs", !A, 1)?; // a different screen
    rec.moment(700_000, 0, "browser", "Docs", !A, 1)?; // after 10 minutes of nothing
    Ok(rec)
}

#[test]
fn keyframes_are_the_moments_where_something_happened() -> TestResult {
    let tmp = tempfile::tempdir()?;
    let data = DataDir::new(tmp.path());
    let _rec = morning(&data)?;
    let history = History::open(&data);
    let keys = history.keyframes(SourceFilter::All, Timestamp(T0), Timestamp(T0 + 800_000))?;
    let got: Vec<(i64, KeyframeKind)> = keys.iter().map(|k| (k.at.0 - T0, k.kind)).collect();
    assert_eq!(
        got,
        [
            (30_000, KeyframeKind::WindowSwitch),
            (60_000, KeyframeKind::AppSwitch),
            (90_000, KeyframeKind::VisualChange),
            (700_000, KeyframeKind::Resume),
        ]
    );
    assert!(keys.iter().all(|k| k.source.is_none()));
    Ok(())
}

#[test]
fn stepping_hops_keyframe_to_keyframe_both_ways() -> TestResult {
    let tmp = tempfile::tempdir()?;
    let data = DataDir::new(tmp.path());
    let _rec = morning(&data)?;
    let history = History::open(&data);
    let step = |from: i64, forward: bool| -> Fallible<Option<i64>> {
        Ok(history
            .step_keyframe(SourceFilter::All, Timestamp(T0 + from), forward)?
            .map(|k| k.at.0 - T0))
    };
    assert_eq!(step(0, true)?, Some(30_000));
    assert_eq!(step(30_000, true)?, Some(60_000), "strictly after");
    assert_eq!(step(90_000, true)?, Some(700_000), "across the gap");
    assert_eq!(step(700_000, true)?, None, "nothing after the last");
    assert_eq!(step(700_000, false)?, Some(90_000));
    assert_eq!(step(30_000, false)?, None, "nothing before the first");
    // Stepping from far past the end still finds the last stop.
    assert_eq!(step(5 * 86_400_000, false)?, Some(700_000));
    // A source the filter excludes contributes nothing.
    let other = rsrewind_core::SourceId::from_bytes([3; 16]);
    assert_eq!(
        history.step_keyframe(SourceFilter::Remote(other), Timestamp(T0), true)?,
        None
    );
    Ok(())
}

#[test]
fn marks_are_read_from_the_folders_own_store() -> TestResult {
    let tmp = tempfile::tempdir()?;
    let data = DataDir::new(tmp.path());
    let rec = morning(&data)?;
    rec.store
        .add_mark(Timestamp(T0 + 60_000), "found the bug", Timestamp(T0))?;
    rec.store.add_mark(Timestamp(T0), "", Timestamp(T0))?;
    // A label written behind the writer's back is cleaned for display.
    rec.store
        .add_mark(Timestamp(T0 + 90_000), "x", Timestamp(T0))?;
    {
        let raw = rusqlite::Connection::open(data.database())?;
        raw.execute(
            "UPDATE marks SET label = 'evil\u{1b}[2J' WHERE label = 'x'",
            [],
        )?;
    }
    let history = History::open(&data);
    let marks = history.marks()?;
    let got: Vec<(i64, &str)> = marks
        .iter()
        .map(|m| (m.at.0 - T0, m.label.as_str()))
        .collect();
    assert_eq!(
        got,
        [(0, ""), (60_000, "found the bug"), (90_000, "evil[2J")]
    );
    Ok(())
}

#[test]
fn a_v1_store_still_reads_and_has_no_marks() -> TestResult {
    let tmp = tempfile::tempdir()?;
    let data = DataDir::new(tmp.path());
    let rec = morning(&data)?;
    drop(rec);
    // Turn it back into what a 0.0.x build left: no marks table, version 1.
    {
        let raw = rusqlite::Connection::open(data.database())?;
        raw.execute_batch("DROP TABLE marks; DELETE FROM schema_migrations WHERE version = 2;")?;
    }
    let db = QueryDb::open(&data)?;
    assert!(db.marks()?.is_empty());
    assert_eq!(db.recent(5, None)?.len(), 5);
    let history = History::open(&data);
    assert!(history.problems().is_empty(), "{:?}", history.problems());
    assert!(history.marks()?.is_empty());
    assert_eq!(history.own_schema(), Some(1));

    // The first mark after the upgrade migrates the store under the open reader, which must see
    // it without being reopened.
    let writer = Store::open(&data)?;
    writer.add_mark(Timestamp(T0 + 5), "after the upgrade", Timestamp(T0))?;
    let marks = history.marks()?;
    assert_eq!(marks.len(), 1);
    assert_eq!(marks[0].label, "after the upgrade");
    assert_eq!(
        history
            .keyframes(SourceFilter::All, Timestamp(T0), Timestamp(T0 + 800_000))?
            .len(),
        4
    );
    Ok(())
}
