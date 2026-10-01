//! Store tests. Fixtures are built through the public `Store` / `media` API; raw SQL appears only
//! in assertions and in the few places that deliberately tamper with a database.

use crate::media::{decode_webp, encode_webp, write_webp_exclusive};
use crate::migrations::{MIGRATIONS, Migration};
use crate::{
    NewVisualState, Observation, OcrStatus, RecorderStatus, SCHEMA_VERSION, StorageError, Store,
};
use rsrewind_core::paths::media_relative_path;
use rsrewind_core::{
    ApplicationContext, ApplicationId, BgraFrame, CaptureState, DataDir, EventKind, MonitorId,
    MonitorInfo, OcrBlock, SessionId, Timestamp, VisualStateId, WindowContext, WindowId,
};
use rusqlite::Connection;
use std::collections::BTreeMap;

type TestResult = Result<(), Box<dyn std::error::Error>>;

const T0: i64 = 1_790_000_000_000;
const GAP: i64 = 5_000;

struct Fixture {
    _tmp: tempfile::TempDir,
    data: DataDir,
    store: Store,
    session: SessionId,
    monitor: MonitorId,
}

fn monitor_info(name: &str) -> MonitorInfo {
    MonitorInfo {
        device_name: name.into(),
        left: 0,
        top: 0,
        width: 1920,
        height: 1080,
        dpi: 96,
        primary: true,
    }
}

fn fixture() -> Result<Fixture, Box<dyn std::error::Error>> {
    let tmp = tempfile::tempdir()?;
    let data = DataDir::new(tmp.path().join("rsRewind"));
    let store = Store::open(&data)?;
    let session = store.begin_session(Timestamp(T0), "host", "0.1.0")?;
    let monitor = store.upsert_monitor(&monitor_info(r"\\.\DISPLAY1"), Timestamp(T0))?;
    Ok(Fixture {
        _tmp: tmp,
        data,
        store,
        session,
        monitor,
    })
}

fn frame(width: u32, height: u32, seed: u8) -> BgraFrame {
    let mut pixels = Vec::with_capacity((width * height * 4) as usize);
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

impl Fixture {
    fn persist(&self, monitor: MonitorId, at: i64, seed: u8) -> crate::Result<VisualStateId> {
        let bytes = encode_webp(&frame(16, 8, seed), 80)?;
        let relative = media_relative_path(Timestamp(at), monitor);
        write_webp_exclusive(&self.data, &relative, &bytes)?;
        self.store.insert_visual_state(&NewVisualState {
            monitor,
            captured_at: Timestamp(at),
            media_path: relative,
            width: 16,
            height: 8,
            byte_size: bytes.len() as u64,
            fingerprint: Some(u64::MAX - u64::from(seed)),
            ocr_enabled: true,
        })
    }

    fn observe(
        &self,
        monitor: MonitorId,
        visual_state: VisualStateId,
        window: Option<(ApplicationId, WindowId)>,
        at: i64,
    ) -> crate::Result<i64> {
        self.store
            .record_observation(&Observation {
                session: self.session,
                monitor,
                visual_state,
                application: window.map(|w| w.0),
                window: window.map(|w| w.1),
                at: Timestamp(at),
                max_gap_ms: GAP,
            })
            .map(|e| e.0)
    }

    fn window(&self, process: &str, title: &str) -> crate::Result<(ApplicationId, WindowId)> {
        let app = self.store.upsert_application(
            &ApplicationContext {
                process_name: process.into(),
                exe_path: None,
            },
            Timestamp(T0),
        )?;
        let window = self.store.upsert_window(
            app,
            &WindowContext {
                title: title.into(),
                class_name: None,
            },
            Timestamp(T0),
        )?;
        Ok((app, window))
    }

    fn count(&self, sql: &str) -> rusqlite::Result<i64> {
        self.store.conn().query_row(sql, [], |row| row.get(0))
    }

    fn media_file(
        &self,
        id: VisualStateId,
    ) -> Result<std::path::PathBuf, Box<dyn std::error::Error>> {
        let relative: String = self.store.conn().query_row(
            "SELECT media_path FROM visual_states WHERE id = ?1",
            [id.0],
            |row| row.get(0),
        )?;
        self.data
            .resolve_media(&relative)
            .ok_or_else(|| "unsafe path".into())
    }
}

fn block(text: &str, line_index: u32) -> OcrBlock {
    OcrBlock {
        text: text.into(),
        x: 1.0,
        y: 2.0 + line_index as f32 * 10.0,
        width: 100.0,
        height: 9.0,
        confidence: None,
        line_index,
    }
}

fn fts_matches(store: &Store, expr: &str) -> rusqlite::Result<Vec<i64>> {
    let mut stmt = store
        .conn()
        .prepare("SELECT rowid FROM ocr_fts WHERE ocr_fts MATCH ?1 ORDER BY rowid")?;
    stmt.query_map([expr], |row| row.get(0))?.collect()
}

const TEST_V2: Migration = Migration {
    version: 2,
    name: "test_extra_table",
    sql: "CREATE TABLE test_extra (x INTEGER);",
};

// ----- migrations --------------------------------------------------------------------------------

#[test]
fn migrates_from_empty_with_required_pragmas() -> TestResult {
    let f = fixture()?;
    assert_eq!(f.store.schema_version()?, SCHEMA_VERSION);
    for table in [
        "sessions",
        "monitors",
        "applications",
        "windows",
        "visual_states",
        "events",
        "ocr_blocks",
        "ocr_fts",
        "control",
        "recorder_status",
        "settings",
        "schema_migrations",
    ] {
        let n = f.store.conn().query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE name = ?1",
            [table],
            |row| row.get::<_, i64>(0),
        )?;
        assert_eq!(n, 1, "{table}");
    }
    let conn = f.store.conn();
    let mode: String = conn.query_row("PRAGMA journal_mode", [], |r| r.get(0))?;
    assert_eq!(mode, "wal");
    assert_eq!(
        conn.query_row("PRAGMA synchronous", [], |r| r.get::<_, i64>(0))?,
        1
    );
    assert_eq!(
        conn.query_row("PRAGMA foreign_keys", [], |r| r.get::<_, i64>(0))?,
        1
    );
    assert_eq!(
        conn.query_row("PRAGMA busy_timeout", [], |r| r.get::<_, i64>(0))?,
        5000
    );
    // A brand-new database has nothing worth backing up.
    assert_eq!(std::fs::read_dir(f.data.backups())?.count(), 0);
    Ok(())
}

#[test]
fn reopen_is_idempotent() -> TestResult {
    let f = fixture()?;
    let data = f.data.clone();
    drop(f.store);
    for _ in 0..3 {
        let store = Store::open(&data)?;
        assert_eq!(store.schema_version()?, SCHEMA_VERSION);
        let applied: i64 =
            store
                .conn()
                .query_row("SELECT COUNT(*) FROM schema_migrations", [], |r| r.get(0))?;
        assert_eq!(applied, MIGRATIONS.len() as i64);
        let sessions: i64 = store
            .conn()
            .query_row("SELECT COUNT(*) FROM sessions", [], |r| r.get(0))?;
        assert_eq!(sessions, 1);
    }
    let reopened = Store::open_existing(&data)?;
    assert_eq!(reopened.schema_version()?, SCHEMA_VERSION);
    // Nothing was pending, so nothing was backed up.
    assert_eq!(std::fs::read_dir(data.backups())?.count(), 0);
    Ok(())
}

#[test]
fn open_existing_never_creates_a_database() -> TestResult {
    let tmp = tempfile::tempdir()?;
    let data = DataDir::new(tmp.path().join("nothing-here"));
    assert!(matches!(
        Store::open_existing(&data),
        Err(StorageError::DatabaseMissing(_))
    ));
    assert!(!data.database().exists());
    Ok(())
}

#[test]
fn refuses_a_newer_schema() -> TestResult {
    let f = fixture()?;
    f.store.conn().execute(
        "INSERT INTO schema_migrations (version, name, applied_at) VALUES (99, 'future', 0)",
        [],
    )?;
    let data = f.data.clone();
    drop(f.store);
    for result in [Store::open(&data), Store::open_existing(&data)] {
        match result {
            Err(StorageError::SchemaTooNew { found, supported }) => {
                assert_eq!((found, supported), (99, SCHEMA_VERSION));
            }
            other => panic!("expected SchemaTooNew, got {other:?}"),
        }
    }
    Ok(())
}

#[test]
fn backs_up_before_migrating_a_database_with_data() -> TestResult {
    let f = fixture()?;
    let data = f.data.clone();
    drop(f.store);

    let migrations = [MIGRATIONS[0], TEST_V2];
    let (store, outcome) = Store::open_with_migrations(&data, &migrations, false)?;
    assert_eq!((outcome.from, outcome.to), (1, 2));
    assert_eq!(store.schema_version()?, 2);
    store
        .conn()
        .execute("INSERT INTO test_extra (x) VALUES (1)", [])?;

    let backup = outcome.backup.ok_or("no backup was made")?;
    assert!(backup.starts_with(data.backups()));
    let name = backup.file_name().and_then(|n| n.to_str()).unwrap_or("");
    assert!(
        name.starts_with("recall-v1-") && name.ends_with(".db"),
        "{name}"
    );

    // The backup is a complete, openable copy of the pre-migration database.
    let copy = Connection::open(&backup)?;
    let version: i64 = copy.query_row("SELECT MAX(version) FROM schema_migrations", [], |r| {
        r.get(0)
    })?;
    assert_eq!(version, 1);
    let sessions: i64 = copy.query_row("SELECT COUNT(*) FROM sessions", [], |r| r.get(0))?;
    assert_eq!(sessions, 1);
    let has_extra: i64 = copy.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE name = 'test_extra'",
        [],
        |r| r.get(0),
    )?;
    assert_eq!(has_extra, 0);
    let check: String = copy.query_row("PRAGMA quick_check", [], |r| r.get(0))?;
    assert_eq!(check, "ok");

    // Re-opening at v2 migrates nothing and makes no second backup.
    drop(copy);
    drop(store);
    let (_, again) = Store::open_with_migrations(&data, &migrations, false)?;
    assert_eq!(again.backup, None);
    let backups = std::fs::read_dir(data.backups())?
        .map(|e| e.map(|e| e.path()))
        .collect::<std::io::Result<Vec<_>>>()?;
    let db_files = backups
        .iter()
        .filter(|p| p.extension().is_some_and(|e| e == "db"))
        .count();
    assert_eq!(db_files, 1, "{backups:?}");
    // And the v1 binary now refuses the v2 database.
    assert!(matches!(
        Store::open(&data),
        Err(StorageError::SchemaTooNew { found: 2, .. })
    ));
    Ok(())
}

#[test]
fn failed_backup_leaves_the_database_unmigrated() -> TestResult {
    let f = fixture()?;
    let data = f.data.clone();
    drop(f.store);
    // A file where the backups directory should be makes the backup impossible.
    std::fs::remove_dir_all(data.backups())?;
    std::fs::write(data.backups(), b"not a directory")?;

    let result = Store::open_with_migrations(&data, &[MIGRATIONS[0], TEST_V2], false);
    assert!(
        matches!(result, Err(StorageError::BackupFailed(_))),
        "{result:?}"
    );

    let conn = Connection::open(data.database())?;
    let version: i64 = conn.query_row("SELECT MAX(version) FROM schema_migrations", [], |r| {
        r.get(0)
    })?;
    assert_eq!(version, 1);
    let has_extra: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE name = 'test_extra'",
        [],
        |r| r.get(0),
    )?;
    assert_eq!(has_extra, 0);
    Ok(())
}

#[test]
fn rejects_out_of_order_migration_lists() -> TestResult {
    let tmp = tempfile::tempdir()?;
    let data = DataDir::new(tmp.path());
    data.ensure()?;
    let result = Store::open_with_migrations(&data, &[TEST_V2], true);
    assert!(matches!(result, Err(StorageError::InvalidArgument(_))));
    Ok(())
}

// ----- upserts -----------------------------------------------------------------------------------

#[test]
fn upserts_are_stable() -> TestResult {
    let f = fixture()?;
    let mut moved = monitor_info(r"\\.\DISPLAY1");
    moved.left = -1920;
    moved.dpi = 144;
    assert_eq!(
        f.store.upsert_monitor(&moved, Timestamp(T0 + 1))?,
        f.monitor
    );
    let (left, dpi): (i64, i64) = f.store.conn().query_row(
        "SELECT \"left\", dpi FROM monitors WHERE id = ?1",
        [f.monitor.0],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    assert_eq!((left, dpi), (-1920, 144));

    let first = f.store.upsert_application(
        &ApplicationContext {
            process_name: "Teams.exe".into(),
            exe_path: None,
        },
        Timestamp(T0),
    )?;
    let second = f.store.upsert_application(
        &ApplicationContext {
            process_name: "teams.EXE".into(),
            exe_path: Some(r"C:\Teams\Teams.exe".into()),
        },
        Timestamp(T0 + 1),
    )?;
    assert_eq!(first, second);
    let third = f.store.upsert_application(
        &ApplicationContext {
            process_name: "TEAMS.exe".into(),
            exe_path: None,
        },
        Timestamp(T0 + 2),
    )?;
    assert_eq!(first, third);
    let exe: Option<String> = f.store.conn().query_row(
        "SELECT exe_path FROM applications WHERE id = ?1",
        [first.0],
        |r| r.get(0),
    )?;
    assert_eq!(
        exe.as_deref(),
        Some(r"C:\Teams\Teams.exe"),
        "known path kept"
    );

    let no_class = WindowContext {
        title: "Chat".into(),
        class_name: None,
    };
    let w1 = f.store.upsert_window(first, &no_class, Timestamp(T0))?;
    let w2 = f.store.upsert_window(first, &no_class, Timestamp(T0 + 1))?;
    assert_eq!(w1, w2, "NULL class names must not duplicate windows");
    let with_class = WindowContext {
        title: "Chat".into(),
        class_name: Some("Chrome_WidgetWin_1".into()),
    };
    let w3 = f.store.upsert_window(first, &with_class, Timestamp(T0))?;
    assert_ne!(w1, w3);
    assert_eq!(
        f.store.upsert_window(first, &with_class, Timestamp(T0))?,
        w3
    );
    Ok(())
}

// ----- observations ------------------------------------------------------------------------------

#[test]
fn unchanged_screen_extends_one_event() -> TestResult {
    let f = fixture()?;
    let win = f.window("code.exe", "main.rs")?;
    let vs = f.persist(f.monitor, T0, 1)?;
    let e1 = f.observe(f.monitor, vs, Some(win), T0)?;
    let e2 = f.observe(f.monitor, vs, Some(win), T0 + 1_000)?;
    let e3 = f.observe(f.monitor, vs, Some(win), T0 + 1_000 + GAP)?; // exactly at the gap limit
    assert_eq!((e1, e2), (e1, e3));
    let (start, end): (i64, i64) = f.store.conn().query_row(
        "SELECT started_at, ended_at FROM events WHERE id = ?1",
        [e1],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    assert_eq!((start, end), (T0, T0 + 1_000 + GAP));
    // A clock stepping backwards extends nothing and shrinks nothing.
    assert_eq!(f.observe(f.monitor, vs, Some(win), T0 + 10)?, e1);
    let end: i64 =
        f.store
            .conn()
            .query_row("SELECT ended_at FROM events WHERE id = ?1", [e1], |r| {
                r.get(0)
            })?;
    assert_eq!(end, T0 + 1_000 + GAP);
    assert_eq!(f.count("SELECT COUNT(*) FROM events")?, 1);
    Ok(())
}

#[test]
fn changes_and_gaps_start_new_events() -> TestResult {
    let f = fixture()?;
    let editor = f.window("code.exe", "main.rs")?;
    let browser = f.window("firefox.exe", "docs")?;
    let vs1 = f.persist(f.monitor, T0, 1)?;
    let vs2 = f.persist(f.monitor, T0 + 50, 2)?;

    let a = f.observe(f.monitor, vs1, Some(editor), T0)?;
    // Gap larger than max_gap_ms.
    let b = f.observe(f.monitor, vs1, Some(editor), T0 + GAP + 1)?;
    assert_ne!(a, b);
    // Different window, same pixels.
    let c = f.observe(f.monitor, vs1, Some(browser), T0 + GAP + 2)?;
    assert_ne!(b, c);
    // Different visual state, same window.
    let d = f.observe(f.monitor, vs2, Some(browser), T0 + GAP + 3)?;
    assert_ne!(c, d);
    // Focus information disappearing is a change too.
    let e = f.observe(f.monitor, vs2, None, T0 + GAP + 4)?;
    assert_ne!(d, e);
    assert_eq!(f.observe(f.monitor, vs2, None, T0 + GAP + 5)?, e);
    assert_eq!(f.count("SELECT COUNT(*) FROM events")?, 5);
    Ok(())
}

#[test]
fn spans_are_tracked_per_monitor_and_session() -> TestResult {
    let f = fixture()?;
    let second = f
        .store
        .upsert_monitor(&monitor_info(r"\\.\DISPLAY2"), Timestamp(T0))?;
    let win = f.window("code.exe", "main.rs")?;
    let vs1 = f.persist(f.monitor, T0, 1)?;
    let vs2 = f.persist(second, T0, 2)?;
    let a = f.observe(f.monitor, vs1, Some(win), T0)?;
    let other = f.observe(second, vs2, Some(win), T0 + 100)?;
    // Activity on monitor 2 does not break monitor 1's span.
    assert_eq!(f.observe(f.monitor, vs1, Some(win), T0 + 200)?, a);
    assert_eq!(f.observe(second, vs2, Some(win), T0 + 300)?, other);

    // A new session never extends the previous session's events.
    let next = f
        .store
        .begin_session(Timestamp(T0 + 400), "host", "0.1.0")?;
    let fresh = f.store.record_observation(&Observation {
        session: next,
        monitor: f.monitor,
        visual_state: vs1,
        application: Some(win.0),
        window: Some(win.1),
        at: Timestamp(T0 + 500),
        max_gap_ms: GAP,
    })?;
    assert_ne!(fresh.0, a);
    Ok(())
}

#[test]
fn markers_do_not_disturb_observations() -> TestResult {
    let f = fixture()?;
    let vs = f.persist(f.monitor, T0, 1)?;
    let a = f.observe(f.monitor, vs, None, T0)?;
    let marker = f.store.record_marker(
        f.session,
        EventKind::Paused,
        Timestamp(T0 + 10),
        Some(serde_json::json!({ "until": T0 + 60_000 })),
    )?;
    assert_ne!(marker.0, a);
    let meta: String = f.store.conn().query_row(
        "SELECT metadata_json FROM events WHERE id = ?1",
        [marker.0],
        |r| r.get(0),
    )?;
    assert!(meta.contains("until"));
    assert!(matches!(
        f.store
            .record_marker(f.session, EventKind::Observation, Timestamp(T0), None),
        Err(StorageError::InvalidArgument(_))
    ));
    Ok(())
}

#[test]
fn observing_a_deleted_visual_state_is_a_typed_error() -> TestResult {
    let f = fixture()?;
    let result = f.observe(f.monitor, VisualStateId(424_242), None, T0);
    assert!(matches!(result, Err(StorageError::VisualStateMissing(_))));
    Ok(())
}

// ----- visual states and OCR ---------------------------------------------------------------------

#[test]
fn visual_state_rows_require_a_written_safe_file() -> TestResult {
    let f = fixture()?;
    let mut state = NewVisualState {
        monitor: f.monitor,
        captured_at: Timestamp(T0),
        media_path: media_relative_path(Timestamp(T0), f.monitor),
        width: 16,
        height: 8,
        byte_size: 10,
        fingerprint: None,
        ocr_enabled: true,
    };
    assert!(matches!(
        f.store.insert_visual_state(&state),
        Err(StorageError::InvalidArgument(_))
    ));
    for bad in [
        "../escape.webp",
        "C:/Windows/x.webp",
        "media\\x.webp",
        "/abs.webp",
    ] {
        state.media_path = bad.into();
        assert!(
            matches!(
                f.store.insert_visual_state(&state),
                Err(StorageError::InvalidMediaPath(_))
            ),
            "{bad}"
        );
    }
    assert_eq!(f.count("SELECT COUNT(*) FROM visual_states")?, 0);
    Ok(())
}

#[test]
fn pending_ocr_is_oldest_first_and_skips_disabled() -> TestResult {
    let f = fixture()?;
    let late = f.persist(f.monitor, T0 + 2_000, 1)?;
    let early = f.persist(f.monitor, T0, 2)?;
    let bytes = encode_webp(&frame(4, 4, 3), 50)?;
    let relative = media_relative_path(Timestamp(T0 + 1_000), f.monitor);
    write_webp_exclusive(&f.data, &relative, &bytes)?;
    f.store.insert_visual_state(&NewVisualState {
        monitor: f.monitor,
        captured_at: Timestamp(T0 + 1_000),
        media_path: relative,
        width: 4,
        height: 4,
        byte_size: bytes.len() as u64,
        fingerprint: None,
        ocr_enabled: false,
    })?;

    let pending = f.store.next_pending_ocr(10)?;
    let ids: Vec<_> = pending.iter().map(|p| p.id).collect();
    assert_eq!(ids, vec![early, late]);
    assert!(
        pending[0].media_path.is_absolute() || pending[0].media_path.starts_with(f.data.root())
    );
    assert!(pending[0].media_path.is_file());
    assert_eq!((pending[0].width, pending[0].height), (16, 8));
    assert_eq!(f.store.next_pending_ocr(1)?.len(), 1);

    f.store
        .mark_ocr(early, OcrStatus::Failed, Some("engine exploded"))?;
    assert_eq!(
        f.store
            .next_pending_ocr(10)?
            .iter()
            .map(|p| p.id)
            .collect::<Vec<_>>(),
        vec![late]
    );
    assert!(matches!(
        f.store.mark_ocr(VisualStateId(999), OcrStatus::Done, None),
        Err(StorageError::VisualStateMissing(_))
    ));

    // A tampered row pointing outside the data directory is never handed to the OCR worker.
    f.store.conn().execute(
        "UPDATE visual_states SET media_path = '../../evil.webp' WHERE id = ?1",
        [late.0],
    )?;
    assert!(f.store.next_pending_ocr(10)?.is_empty());
    let status: String = f.store.conn().query_row(
        "SELECT ocr_status FROM visual_states WHERE id = ?1",
        [late.0],
        |r| r.get(0),
    )?;
    assert_eq!(status, "failed");
    Ok(())
}

#[test]
fn fts5_is_available_and_save_ocr_is_atomic_and_replaces() -> TestResult {
    let f = fixture()?;
    let vs = f.persist(f.monitor, T0, 1)?;
    let other = f.persist(f.monitor, T0 + 1, 2)?;
    f.store.save_ocr(
        vs,
        &[
            block("quarterly budget review", 1),
            block("Café meeting notes", 0),
        ],
        "test-engine",
        42,
    )?;
    f.store
        .save_ocr(other, &[block("unrelated words", 0)], "test-engine", 7)?;

    assert_eq!(fts_matches(&f.store, "budget")?, vec![vs.0]);
    assert_eq!(
        fts_matches(&f.store, "cafe")?,
        vec![vs.0],
        "diacritics folded"
    );
    assert_eq!(fts_matches(&f.store, "\"budget review\"")?, vec![vs.0]);
    assert_eq!(fts_matches(&f.store, "quart*")?, vec![vs.0]);
    let text: String =
        f.store
            .conn()
            .query_row("SELECT text FROM ocr_fts WHERE rowid = ?1", [vs.0], |r| {
                r.get(0)
            })?;
    assert_eq!(
        text, "Café meeting notes\nquarterly budget review",
        "line order"
    );
    let (status, engine, ms): (String, String, i64) = f.store.conn().query_row(
        "SELECT ocr_status, ocr_engine, ocr_ms FROM visual_states WHERE id = ?1",
        [vs.0],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    assert_eq!(
        (status.as_str(), engine.as_str(), ms),
        ("done", "test-engine", 42)
    );

    // Re-running OCR replaces, never accumulates.
    f.store
        .save_ocr(vs, &[block("rewritten", 0)], "test-engine", 1)?;
    assert!(fts_matches(&f.store, "budget")?.is_empty());
    assert_eq!(fts_matches(&f.store, "rewritten")?, vec![vs.0]);
    assert_eq!(
        f.count(&format!(
            "SELECT COUNT(*) FROM ocr_blocks WHERE visual_state_id = {}",
            vs.0
        ))?,
        1
    );
    assert_eq!(f.count("SELECT COUNT(*) FROM ocr_fts")?, 2);

    assert!(matches!(
        f.store
            .save_ocr(VisualStateId(9_999), &[block("x", 0)], "e", 1),
        Err(StorageError::VisualStateMissing(_))
    ));
    assert_eq!(f.count("SELECT COUNT(*) FROM ocr_fts")?, 2);
    Ok(())
}

// ----- control and status ------------------------------------------------------------------------

#[test]
fn control_and_status_round_trip() -> TestResult {
    let f = fixture()?;
    assert_eq!(f.store.read_control()?, CaptureState::Recording);
    for state in [
        CaptureState::Paused {
            until: Some(Timestamp(T0 + 5)),
        },
        CaptureState::Paused { until: None },
        CaptureState::Stopped,
        CaptureState::Error,
        CaptureState::Recording,
    ] {
        f.store.set_control(state)?;
        assert_eq!(f.store.read_control()?, state);
    }

    assert_eq!(f.store.read_status()?, None);
    let mut counters = BTreeMap::new();
    counters.insert("frames_dropped".to_string(), 3);
    let status = RecorderStatus {
        pid: 1234,
        started_at: Timestamp(T0),
        heartbeat_at: Timestamp(T0 + 5_000),
        state: CaptureState::Paused {
            until: Some(Timestamp(T0 + 60_000)),
        },
        counters,
    };
    f.store.write_status(&status)?;
    assert_eq!(f.store.read_status()?, Some(status.clone()));
    let newer = RecorderStatus {
        heartbeat_at: Timestamp(T0 + 10_000),
        state: CaptureState::Recording,
        ..status
    };
    f.store.write_status(&newer)?;
    assert_eq!(f.store.read_status()?, Some(newer));
    assert_eq!(f.count("SELECT COUNT(*) FROM recorder_status")?, 1);
    Ok(())
}

// ----- deletion and retention --------------------------------------------------------------------

#[test]
fn delete_range_removes_rows_fts_and_files_but_keeps_shared_states() -> TestResult {
    let f = fixture()?;
    let win = f.window("code.exe", "secret.txt")?;
    // `shared` is shown inside the range and again after it.
    let shared = f.persist(f.monitor, T0, 1)?;
    let inside = f.persist(f.monitor, T0 + 20_000, 2)?;
    let outside = f.persist(f.monitor, T0 + 100_000, 3)?;
    f.store
        .save_ocr(shared, &[block("shared words", 0)], "e", 1)?;
    f.store
        .save_ocr(inside, &[block("password hunter2", 0)], "e", 1)?;
    f.store
        .save_ocr(outside, &[block("later words", 0)], "e", 1)?;
    let shared_early = f.observe(f.monitor, shared, Some(win), T0)?;
    let inside_event = f.observe(f.monitor, inside, Some(win), T0 + 20_000)?;
    let shared_late = f.observe(f.monitor, shared, Some(win), T0 + 90_000)?;
    let outside_event = f.observe(f.monitor, outside, Some(win), T0 + 100_000)?;
    // An unreferenced state captured inside the range (crash between file write and observation).
    let stray = f.persist(f.monitor, T0 + 30_000, 4)?;

    let inside_file = f.media_file(inside)?;
    let stray_file = f.media_file(stray)?;
    let shared_file = f.media_file(shared)?;
    assert!(inside_file.is_file());

    let report = f
        .store
        .delete_range(Timestamp(T0 - 1), Timestamp(T0 + 50_000))?;
    assert_eq!(report.events_deleted, 2);
    assert_eq!(report.visual_states_deleted, 2);
    assert_eq!(report.files_deleted, 2);
    assert!(report.file_errors.is_empty());
    assert!(report.bytes_freed > 0);

    let ids = |sql: &str| -> rusqlite::Result<Vec<i64>> {
        let mut stmt = f.store.conn().prepare(sql)?;
        stmt.query_map([], |r| r.get(0))?.collect()
    };
    assert_eq!(
        ids("SELECT id FROM events ORDER BY id")?,
        vec![shared_late, outside_event]
    );
    assert!(!ids("SELECT id FROM events")?.contains(&shared_early));
    assert!(!ids("SELECT id FROM events")?.contains(&inside_event));
    assert_eq!(
        ids("SELECT id FROM visual_states ORDER BY id")?,
        vec![shared.0, outside.0]
    );
    assert!(fts_matches(&f.store, "hunter2")?.is_empty());
    assert_eq!(fts_matches(&f.store, "shared")?, vec![shared.0]);
    assert_eq!(
        f.count(&format!(
            "SELECT COUNT(*) FROM ocr_blocks WHERE visual_state_id = {}",
            inside.0
        ))?,
        0
    );
    assert!(!inside_file.exists());
    assert!(!stray_file.exists());
    assert!(shared_file.is_file(), "shared state's file survives");
    assert!(f.store.integrity_check()?.is_empty());
    Ok(())
}

#[test]
fn delete_range_reports_missing_files_without_failing() -> TestResult {
    let f = fixture()?;
    let vs = f.persist(f.monitor, T0, 1)?;
    f.observe(f.monitor, vs, None, T0)?;
    std::fs::remove_file(f.media_file(vs)?)?;
    let report = f.store.delete_range(Timestamp(T0), Timestamp(T0 + 1))?;
    assert_eq!(
        (
            report.visual_states_deleted,
            report.files_deleted,
            report.files_missing
        ),
        (1, 0, 1)
    );
    // An empty or inverted range deletes nothing.
    let empty = f.store.delete_range(Timestamp(T0 + 5), Timestamp(T0))?;
    assert_eq!(empty, crate::DeleteReport::default());
    Ok(())
}

#[test]
fn retention_by_age_removes_only_old_history() -> TestResult {
    let f = fixture()?;
    let day = 86_400_000;
    let now = T0 + 10 * day;
    let old = f.persist(f.monitor, T0, 1)?;
    let recent = f.persist(f.monitor, now - day, 2)?;
    f.observe(f.monitor, old, None, T0)?;
    f.observe(f.monitor, recent, None, now - day)?;
    let old_file = f.media_file(old)?;

    let report = f.store.apply_retention(7, 0, Timestamp(now))?;
    assert_eq!(
        (report.events_deleted, report.visual_states_deleted),
        (1, 1)
    );
    assert!(!old_file.exists());
    assert_eq!(f.count("SELECT COUNT(*) FROM visual_states")?, 1);
    // 0 disables both rules.
    let none = f.store.apply_retention(0, 0, Timestamp(now + 100 * day))?;
    assert_eq!(none.events_deleted, 0);
    assert_eq!(f.count("SELECT COUNT(*) FROM events")?, 1);
    Ok(())
}

#[test]
fn retention_by_size_removes_oldest_first() -> TestResult {
    let f = fixture()?;
    let mut ids = Vec::new();
    for i in 0..6u8 {
        let at = T0 + i64::from(i) * 60_000;
        let vs = f.persist(f.monitor, at, i * 40)?;
        f.observe(f.monitor, vs, None, at)?;
        ids.push(vs);
    }
    let stats = f.store.stats()?;
    let live_db = f.store.live_db_bytes()?;
    assert_eq!(stats.visual_states, 6);
    // Room for the database plus a bit less than three images: the three oldest must go.
    let sizes: Vec<i64> = {
        let mut stmt = f
            .store
            .conn()
            .prepare("SELECT byte_size FROM visual_states ORDER BY captured_at")?;
        stmt.query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?
    };
    let keep: i64 = sizes[3..].iter().sum();
    let cap = live_db + keep as u64;
    let report = f.store.apply_retention(0, cap, Timestamp(T0 + 3_600_000))?;
    assert!(report.visual_states_deleted >= 3, "{report:?}");
    let remaining: Vec<i64> = {
        let mut stmt = f
            .store
            .conn()
            .prepare("SELECT id FROM visual_states ORDER BY captured_at")?;
        stmt.query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?
    };
    // Whatever survived is the newest suffix.
    let expected: Vec<i64> = ids[ids.len() - remaining.len()..]
        .iter()
        .map(|v| v.0)
        .collect();
    assert_eq!(remaining, expected);
    assert!(f.store.live_db_bytes()? + f.store.media_bytes()? <= cap);
    Ok(())
}

// ----- diagnostics -------------------------------------------------------------------------------

#[test]
fn stats_and_orphans() -> TestResult {
    let f = fixture()?;
    let a = f.persist(f.monitor, T0, 1)?;
    let b = f.persist(f.monitor, T0 + 1_000, 2)?;
    f.observe(f.monitor, a, None, T0)?;
    f.observe(f.monitor, b, None, T0 + 1_000)?;
    f.store.save_ocr(a, &[block("hello", 0)], "e", 1)?;

    let stats = f.store.stats()?;
    assert_eq!(stats.sessions, 1);
    assert_eq!(stats.events, 2);
    assert_eq!(stats.observations, 2);
    assert_eq!(stats.visual_states, 2);
    assert_eq!(stats.ocr_blocks, 1);
    assert_eq!(stats.pending_ocr, 1);
    assert_eq!(stats.oldest, Some(Timestamp(T0)));
    assert_eq!(stats.newest, Some(Timestamp(T0 + 1_000)));
    assert!(stats.db_bytes > 0 && stats.media_bytes > 0);

    assert!(f.store.integrity_check()?.is_empty());
    let clean = f.store.find_orphans(10)?;
    assert!(
        clean.rows_missing_file.is_empty() && clean.files_without_row.is_empty(),
        "{clean:?}"
    );

    std::fs::remove_file(f.media_file(b)?)?;
    let stray = f.data.media_root().join("2026").join("stray.webp");
    std::fs::create_dir_all(stray.parent().ok_or("no parent")?)?;
    std::fs::write(&stray, b"x")?;
    let report = f.store.find_orphans(10)?;
    assert_eq!(report.rows_missing_file.len(), 1);
    assert_eq!(report.rows_missing_file[0].0, b);
    assert_eq!(report.files_without_row, vec![stray]);
    Ok(())
}

// ----- media -------------------------------------------------------------------------------------

#[test]
fn webp_round_trip_respects_stride_and_dimensions() -> TestResult {
    // 3x2 pure-colour frame with 8 bytes of row padding filled with garbage.
    let (w, h, stride) = (37u32, 21u32, 37 * 4 + 8);
    let mut pixels = vec![0xEEu8; (stride * h) as usize];
    for y in 0..h {
        for x in 0..w {
            let at = (y * stride + x * 4) as usize;
            pixels[at..at + 4].copy_from_slice(&[200, 40, 10, 255]); // B G R A
        }
    }
    let strided = BgraFrame {
        width: w,
        height: h,
        stride,
        pixels,
    };
    let bytes = encode_webp(&strided, 90)?;
    assert_eq!(&bytes[0..4], b"RIFF");
    assert_eq!(&bytes[8..12], b"WEBP");
    let decoded = decode_webp(&bytes)?;
    assert_eq!(
        (decoded.width, decoded.height, decoded.stride),
        (w, h, w * 4)
    );
    assert_eq!(decoded.pixels.len(), (w * h * 4) as usize);
    // Lossy, but a flat colour comes back close and channel order is preserved (no padding
    // garbage bleeding in, no R/B swap).
    let centre = ((h / 2 * w + w / 2) * 4) as usize;
    let px = &decoded.pixels[centre..centre + 4];
    for (got, want) in px.iter().zip([200u8, 40, 10, 255]) {
        assert!(got.abs_diff(want) <= 12, "{px:?}");
    }
    Ok(())
}

#[test]
fn encode_rejects_bad_frames() {
    let empty = BgraFrame {
        width: 0,
        height: 0,
        stride: 0,
        pixels: vec![],
    };
    assert!(encode_webp(&empty, 80).is_err());
    let short = BgraFrame {
        width: 4,
        height: 4,
        stride: 16,
        pixels: vec![0; 10],
    };
    assert!(encode_webp(&short, 80).is_err());
    let huge = BgraFrame {
        width: 20_000,
        height: 1,
        stride: 80_000,
        pixels: vec![0; 80_000],
    };
    assert!(encode_webp(&huge, 80).is_err());
    assert!(decode_webp(b"definitely not webp").is_err());
}

#[test]
fn exclusive_write_never_overwrites_and_leaves_no_temp_files() -> TestResult {
    let tmp = tempfile::tempdir()?;
    let data = DataDir::new(tmp.path());
    let relative = "media/2026/09/30/a.webp";
    let path = write_webp_exclusive(&data, relative, b"first")?;
    assert_eq!(Some(path.clone()), data.resolve_media(relative));
    assert!(matches!(
        write_webp_exclusive(&data, relative, b"second"),
        Err(StorageError::MediaExists(_))
    ));
    assert_eq!(std::fs::read(&path)?, b"first");
    let names: Vec<_> = std::fs::read_dir(path.parent().ok_or("no parent")?)?
        .map(|e| e.map(|e| e.file_name()))
        .collect::<std::io::Result<_>>()?;
    assert_eq!(names, vec![std::ffi::OsString::from("a.webp")]);

    for bad in [
        "../escape.webp",
        "media/../../escape.webp",
        "C:/Windows/escape.webp",
        "/escape.webp",
        "media\\escape.webp",
        "",
    ] {
        assert!(
            matches!(
                write_webp_exclusive(&data, bad, b"x"),
                Err(StorageError::InvalidMediaPath(_))
            ),
            "{bad}"
        );
    }
    assert!(
        !tmp.path()
            .parent()
            .ok_or("no parent")?
            .join("escape.webp")
            .exists()
    );
    Ok(())
}
