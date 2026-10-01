//! End-to-end query tests. The fixture database is built only through `rsrewind_storage::Store`
//! and read only through `QueryDb`: if a consumer would need SQL to do something, these tests
//! would need it too.

use rsrewind_core::paths::media_relative_path;
use rsrewind_core::{
    ApplicationContext, BgraFrame, DataDir, MonitorId, MonitorInfo, OcrBlock, SearchQuery,
    SessionId, Timestamp, VisualStateId, WindowContext,
};
use rsrewind_query::{DEFAULT_LIMIT, MAX_LIMIT, QueryDb, QueryError};
use rsrewind_storage::media::{encode_webp, write_webp_exclusive};
use rsrewind_storage::{NewVisualState, Observation, Store};
use std::path::Path;

type TestResult = Result<(), Box<dyn std::error::Error>>;
type Fallible<T> = Result<T, Box<dyn std::error::Error>>;

const T0: i64 = 1_790_000_000_000;
const MIN: i64 = 60_000;

struct Fixture {
    _tmp: tempfile::TempDir,
    data: DataDir,
    store: Store,
    session: SessionId,
    m1: MonitorId,
    m2: MonitorId,
    seed: u8,
}

fn monitor(name: &str, left: i32) -> MonitorInfo {
    MonitorInfo {
        device_name: name.into(),
        left,
        top: 0,
        width: 1920,
        height: 1080,
        dpi: 96,
        primary: left == 0,
    }
}

impl Fixture {
    fn new() -> Fallible<Self> {
        let tmp = tempfile::tempdir()?;
        let data = DataDir::new(tmp.path().join("rsRewind"));
        let store = Store::open(&data)?;
        let session = store.begin_session(Timestamp(T0), "host", "test")?;
        let m1 = store.upsert_monitor(&monitor(r"\\.\DISPLAY1", 0), Timestamp(T0))?;
        let m2 = store.upsert_monitor(&monitor(r"\\.\DISPLAY2", 1920), Timestamp(T0))?;
        Ok(Self {
            _tmp: tmp,
            data,
            store,
            session,
            m1,
            m2,
            seed: 0,
        })
    }

    /// Persists a screen (file + row), observes it for `app`/`title` at `at`, and OCRs `lines`.
    fn screen(
        &mut self,
        monitor: MonitorId,
        at: i64,
        app: &str,
        title: &str,
        lines: &[&str],
    ) -> Fallible<VisualStateId> {
        self.seed = self.seed.wrapping_add(1);
        let frame = BgraFrame {
            width: 8,
            height: 4,
            stride: 32,
            pixels: vec![self.seed; 128],
        };
        let bytes = encode_webp(&frame, 50)?;
        let relative = media_relative_path(Timestamp(at), monitor);
        write_webp_exclusive(&self.data, &relative, &bytes)?;
        let id = self.store.insert_visual_state(&NewVisualState {
            monitor,
            captured_at: Timestamp(at),
            media_path: relative.clone(),
            width: 8,
            height: 4,
            byte_size: bytes.len() as u64,
            fingerprint: None,
            ocr_enabled: true,
        })?;
        self.observe(monitor, id, at, app, title)?;
        if !lines.is_empty() {
            let blocks: Vec<OcrBlock> = lines
                .iter()
                .enumerate()
                .map(|(i, text)| OcrBlock {
                    text: (*text).into(),
                    x: 10.0,
                    y: 20.0 * i as f32,
                    width: 300.0,
                    height: 18.0,
                    confidence: None,
                    line_index: i as u32,
                })
                .collect();
            self.store.save_ocr(id, &relative, &blocks, "test", 3)?;
        }
        Ok(id)
    }

    fn observe(
        &self,
        monitor: MonitorId,
        id: VisualStateId,
        at: i64,
        app: &str,
        title: &str,
    ) -> Fallible<()> {
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
        self.store.record_observation(&Observation {
            session: self.session,
            monitor,
            visual_state: id,
            application: Some(application),
            window: Some(window),
            at: Timestamp(at),
            max_gap_ms: 5_000,
        })?;
        Ok(())
    }

    fn query(&self) -> Result<QueryDb, QueryError> {
        QueryDb::open(&self.data)
    }
}

struct Standard {
    f: Fixture,
    sync: VisualStateId,
    sheet: VisualStateId,
    code: VisualStateId,
    chat: VisualStateId,
    pending: VisualStateId,
}

fn standard() -> Fallible<Standard> {
    let mut f = Fixture::new()?;
    let (m1, m2) = (f.m1, f.m2);
    let sync = f.screen(
        m1,
        T0,
        "Teams.exe",
        "Weekly Sync – Ünïcode Title",
        &["quarterly budget review", "alice and bob"],
    )?;
    let sheet = f.screen(
        m1,
        T0 + MIN,
        "firefox.exe",
        "Budget spreadsheet",
        &["budget budget budget", "spreadsheet totals"],
    )?;
    let code = f.screen(
        m2,
        T0 + 2 * MIN,
        "Code.exe",
        "main.rs",
        &["fn main() { println!(\"Café crème\"); }", "東京 タワー"],
    )?;
    let chat = f.screen(m1, T0 + 3 * MIN, "Teams.exe", "Chat", &["lunch plans"])?;
    // The sync screen comes back later; its search timestamp stays the first sighting.
    f.observe(
        m1,
        sync,
        T0 + 4 * MIN,
        "Teams.exe",
        "Weekly Sync – Ünïcode Title",
    )?;
    let pending = f.screen(m1, T0 + 5 * MIN, "notepad.exe", "notes.txt", &[])?;
    Ok(Standard {
        f,
        sync,
        sheet,
        code,
        chat,
        pending,
    })
}

fn search(db: &QueryDb, text: &str) -> Result<Vec<VisualStateId>, QueryError> {
    Ok(db
        .search(&SearchQuery {
            text: text.into(),
            ..SearchQuery::default()
        })?
        .into_iter()
        .map(|h| h.visual_state_id)
        .collect())
}

#[test]
fn open_requires_an_existing_current_database() -> TestResult {
    let tmp = tempfile::tempdir()?;
    let data = DataDir::new(tmp.path().join("missing"));
    assert!(matches!(
        QueryDb::open(&data),
        Err(QueryError::DatabaseMissing(_))
    ));
    assert!(
        !data.database().exists(),
        "query must never create a database"
    );
    Ok(())
}

#[test]
fn opens_read_only_while_the_writer_is_open_and_after_it_closed() -> TestResult {
    let s = standard()?;
    {
        let db = s.f.query()?;
        assert_eq!(search(&db, "budget")?.len(), 2);
    }
    let data = s.f.data.clone();
    let _tmp = s.f._tmp;
    drop(s.f.store);
    // The last writer closing checkpoints and removes -wal/-shm; read-only opens must still work.
    let db = QueryDb::open(&data)?;
    assert_eq!(search(&db, "budget")?.len(), 2);
    Ok(())
}

#[test]
fn ranks_by_bm25_with_snippets_and_metadata() -> TestResult {
    let s = standard()?;
    let db = s.f.query()?;
    let hits = db.search(&SearchQuery {
        text: "budget".into(),
        ..SearchQuery::default()
    })?;
    let ids: Vec<_> = hits.iter().map(|h| h.visual_state_id).collect();
    // Three occurrences beat one.
    assert_eq!(ids, vec![s.sheet, s.sync]);
    assert!(hits[0].rank <= hits[1].rank, "lower bm25 is better");

    let sheet = &hits[0];
    assert!(sheet.snippet.contains("[budget]"), "{}", sheet.snippet);
    assert_eq!(sheet.application.as_deref(), Some("firefox.exe"));
    assert_eq!(sheet.window_title.as_deref(), Some("Budget spreadsheet"));
    assert_eq!(sheet.monitor.as_deref(), Some(r"\\.\DISPLAY1"));
    assert_eq!(sheet.timestamp, Timestamp(T0 + MIN));
    assert!(sheet.timestamp_utc.ends_with('Z') && sheet.timestamp_utc.contains('T'));
    assert!(Path::new(&sheet.media_path).is_absolute());
    assert!(Path::new(&sheet.media_path).is_file());
    assert!(Path::new(&sheet.media_path).starts_with(s.f.data.root()));

    let sync = &hits[1];
    assert_eq!(sync.timestamp, Timestamp(T0), "earliest observation wins");
    assert!(sync.snippet.contains("[budget]"));

    // JSON shape for `--json` consumers.
    let json = serde_json::to_value(&hits[0])?;
    assert!(json.get("visual_state_id").is_some() && json.get("snippet").is_some());
    Ok(())
}

#[test]
fn equal_rank_falls_back_to_recency() -> TestResult {
    let mut f = Fixture::new()?;
    let m1 = f.m1;
    let older = f.screen(m1, T0, "a.exe", "x", &["identical words here"])?;
    let newer = f.screen(m1, T0 + MIN, "a.exe", "x", &["identical words here"])?;
    let db = f.query()?;
    assert_eq!(search(&db, "identical")?, vec![newer, older]);
    Ok(())
}

#[test]
fn phrase_prefix_and_unicode() -> TestResult {
    let s = standard()?;
    let db = s.f.query()?;
    assert_eq!(search(&db, "\"budget review\"")?, vec![s.sync]);
    assert!(search(&db, "\"review budget\"")?.is_empty());
    assert_eq!(search(&db, "quart*")?, vec![s.sync]);
    assert_eq!(search(&db, "cafe")?, vec![s.code], "diacritics folded");
    assert_eq!(search(&db, "CAFÉ CRÈME")?, vec![s.code]);
    assert_eq!(search(&db, "東京")?, vec![s.code]);
    // All words must be present.
    assert_eq!(search(&db, "budget alice")?, vec![s.sync]);
    assert!(search(&db, "budget lunch")?.is_empty());
    Ok(())
}

#[test]
fn hostile_text_is_searched_literally_and_never_errors() -> TestResult {
    let s = standard()?;
    let db = s.f.query()?;
    for text in [
        "NEAR(budget review)",
        "text:budget",
        "budget AND",
        "OR budget",
        "NOT",
        "-budget",
        "^budget",
        "\"",
        "\"budget",
        "budget\"",
        "(budget",
        "budget)",
        "*",
        "budget*)",
        "'; DROP TABLE events; --",
        "{text}:budget",
        "a\0b",
    ] {
        let result = search(&db, text);
        assert!(result.is_ok(), "{text:?}: {result:?}");
    }
    // Operator-looking input means the literal words.
    assert_eq!(search(&db, "alice AND bob")?, vec![s.sync]);
    assert_eq!(search(&db, "-budget")?.len(), 2, "minus does not negate");
    // The database is untouched.
    assert_eq!(db.recent(100, None)?.len(), 6);
    Ok(())
}

#[test]
fn application_filter_is_exact_case_insensitive_and_exe_optional() -> TestResult {
    let s = standard()?;
    let db = s.f.query()?;
    let by_app = |app: &str| -> Result<Vec<VisualStateId>, QueryError> {
        Ok(db
            .search(&SearchQuery {
                text: "budget".into(),
                application: Some(app.into()),
                ..SearchQuery::default()
            })?
            .into_iter()
            .map(|h| h.visual_state_id)
            .collect())
    };
    for app in ["Teams.exe", "teams", "TEAMS.EXE", " teams.Exe "] {
        assert_eq!(by_app(app)?, vec![s.sync], "{app}");
    }
    assert_eq!(by_app("firefox")?, vec![s.sheet]);
    assert!(by_app("team")?.is_empty(), "exact, not substring");
    assert!(by_app("eams.exe")?.is_empty());
    Ok(())
}

#[test]
fn title_filter_is_a_unicode_case_insensitive_substring() -> TestResult {
    let s = standard()?;
    let db = s.f.query()?;
    let by_title = |title: &str| -> Result<Vec<VisualStateId>, QueryError> {
        Ok(db
            .search(&SearchQuery {
                text: "budget".into(),
                title_contains: Some(title.into()),
                ..SearchQuery::default()
            })?
            .into_iter()
            .map(|h| h.visual_state_id)
            .collect())
    };
    assert_eq!(by_title("weekly SYNC")?, vec![s.sync]);
    assert_eq!(by_title("ÜNÏCODE")?, vec![s.sync], "non-ASCII case folding");
    assert_eq!(by_title("SPREAD")?, vec![s.sheet]);
    assert!(by_title("nothing like this")?.is_empty());
    // `%` and `_` are not wildcards.
    assert!(by_title("%")?.is_empty());
    assert!(by_title("_")?.is_empty());
    Ok(())
}

#[test]
fn time_filters_match_any_observation_that_overlaps_the_range() -> TestResult {
    let s = standard()?;
    let db = s.f.query()?;
    let between = |since: Option<i64>, until: Option<i64>| -> Result<Vec<_>, QueryError> {
        Ok(db
            .search(&SearchQuery {
                text: "budget".into(),
                since: since.map(Timestamp),
                until: until.map(Timestamp),
                ..SearchQuery::default()
            })?
            .into_iter()
            .map(|h| h.visual_state_id)
            .collect())
    };
    // `sync` was first seen at T0 but is on screen again at T0+4min, so it is part of that range;
    // its reported time is the earliest observation that matches the range.
    assert_eq!(between(Some(T0 + 30_000), None)?, vec![s.sheet, s.sync]);
    let hits = db.search(&SearchQuery {
        text: "budget".into(),
        since: Some(Timestamp(T0 + 30_000)),
        ..SearchQuery::default()
    })?;
    assert_eq!(hits[1].timestamp, Timestamp(T0 + 4 * MIN));
    assert_eq!(between(None, Some(T0 + 30_000))?, vec![s.sync]);
    assert_eq!(
        between(Some(T0), Some(T0))?,
        vec![s.sync],
        "inclusive bounds"
    );
    assert!(between(Some(T0 + 10 * MIN), None)?.is_empty());
    Ok(())
}

#[test]
fn empty_text_lists_by_filters_newest_first() -> TestResult {
    let s = standard()?;
    let db = s.f.query()?;
    let hits = db.search(&SearchQuery {
        text: "   ".into(),
        application: Some("teams".into()),
        ..SearchQuery::default()
    })?;
    let ids: Vec<_> = hits.iter().map(|h| h.visual_state_id).collect();
    assert_eq!(ids, vec![s.chat, s.sync]);
    assert_eq!(hits[0].snippet, "lunch plans");
    // Punctuation-only text is "no text", not "match nothing".
    let all = db.search(&SearchQuery {
        text: "- ( ) *".into(),
        ..SearchQuery::default()
    })?;
    assert_eq!(all.len(), 5);
    assert_eq!(all[0].visual_state_id, s.pending);
    assert_eq!(all[0].snippet, "");
    Ok(())
}

#[test]
fn limits_default_to_20_and_clamp_to_200() -> TestResult {
    let mut f = Fixture::new()?;
    let m1 = f.m1;
    for i in 0..205 {
        f.screen(m1, T0 + i * 10_000, "bulk.exe", "bulk", &["bulk row"])?;
    }
    let db = f.query()?;
    let count = |limit: u32| -> Result<usize, QueryError> {
        Ok(db
            .search(&SearchQuery {
                text: "bulk".into(),
                limit,
                ..SearchQuery::default()
            })?
            .len())
    };
    assert_eq!(count(0)?, DEFAULT_LIMIT as usize);
    assert_eq!(count(7)?, 7);
    assert_eq!(count(10_000)?, MAX_LIMIT as usize);
    assert_eq!(db.recent(0, None)?.len(), DEFAULT_LIMIT as usize);
    assert_eq!(db.recent(u32::MAX, None)?.len(), MAX_LIMIT as usize);
    Ok(())
}

#[test]
fn recent_is_newest_first_and_pages() -> TestResult {
    let s = standard()?;
    let db = s.f.query()?;
    let all = db.recent(50, None)?;
    let ids: Vec<_> = all.iter().map(|e| e.visual_state_id).collect();
    assert_eq!(
        ids,
        vec![s.pending, s.sync, s.chat, s.code, s.sheet, s.sync],
        "one entry per observation event"
    );
    assert!(all.windows(2).all(|w| w[0].started_at >= w[1].started_at));
    assert_eq!(all[0].ocr_status, "pending");
    assert_eq!(all[1].ocr_status, "done");
    assert_eq!(all[3].monitor.as_deref(), Some(r"\\.\DISPLAY2"));
    assert_eq!(all[3].application.as_deref(), Some("Code.exe"));
    assert!(Path::new(&all[0].media_path).is_file());

    let page = db.recent(2, Some(all[1].cursor()))?;
    let page_ids: Vec<_> = page.iter().map(|e| e.visual_state_id).collect();
    assert_eq!(page_ids, vec![s.chat, s.code]);
    Ok(())
}

#[test]
fn at_finds_covering_or_nearest_earlier_observation() -> TestResult {
    let mut f = Fixture::new()?;
    let (m1, m2) = (f.m1, f.m2);
    let long = f.screen(m1, T0, "a.exe", "long", &["long"])?;
    // Extend the first span to T0 + 4 s (within the 5 s gap).
    f.observe(m1, long, T0 + 4_000, "a.exe", "long")?;
    // A later, short event on another monitor that has ended before T0 + 3 s.
    let short = f.screen(m2, T0 + 2_000, "b.exe", "short", &["short"])?;
    let db = f.query()?;

    let covering = db.at(Timestamp(T0 + 3_000))?.ok_or("nothing at T0+3s")?;
    assert_eq!(covering.visual_state_id, long);
    assert_eq!(
        (covering.started_at, covering.ended_at),
        (Timestamp(T0), Timestamp(T0 + 4_000))
    );
    let exact = db.at(Timestamp(T0 + 2_000))?.ok_or("nothing at T0+2s")?;
    assert_eq!(
        exact.visual_state_id, short,
        "latest-started covering event"
    );
    let later = db.at(Timestamp(T0 + 60_000))?.ok_or("nothing after")?;
    assert_eq!(later.visual_state_id, short, "nearest earlier start");
    assert_eq!(db.at(Timestamp(T0 - 1))?, None);
    Ok(())
}

#[test]
fn visual_detail_has_text_blocks_and_context() -> TestResult {
    let s = standard()?;
    let db = s.f.query()?;
    let detail = db.visual_detail(s.sync)?.ok_or("missing detail")?;
    assert_eq!(detail.visual_state_id, s.sync);
    assert_eq!(detail.captured_at, Timestamp(T0));
    assert_eq!((detail.width, detail.height), (8, 4));
    assert_eq!(detail.application.as_deref(), Some("Teams.exe"));
    assert_eq!(
        detail.window_title.as_deref(),
        Some("Weekly Sync – Ünïcode Title")
    );
    assert_eq!(detail.monitor.as_deref(), Some(r"\\.\DISPLAY1"));
    assert_eq!(detail.ocr_status, "done");
    assert_eq!(detail.ocr_text, "quarterly budget review\nalice and bob");
    let lines: Vec<_> = detail.blocks.iter().map(|b| b.text.as_str()).collect();
    assert_eq!(lines, vec!["quarterly budget review", "alice and bob"]);
    assert_eq!(detail.blocks[1].line_index, 1);
    assert!(Path::new(&detail.media_path).is_file());

    let pending = db.visual_detail(s.pending)?.ok_or("missing pending")?;
    assert_eq!(
        (pending.ocr_status.as_str(), pending.ocr_text.as_str()),
        ("pending", "")
    );
    assert!(pending.blocks.is_empty());
    assert_eq!(db.visual_detail(VisualStateId(999_999))?, None);
    Ok(())
}

#[test]
fn deleted_history_disappears_from_queries() -> TestResult {
    let s = standard()?;
    s.f.store
        .delete_range(Timestamp(T0 + MIN), Timestamp(T0 + MIN + 1))?;
    let db = s.f.query()?;
    assert_eq!(search(&db, "budget")?, vec![s.sync]);
    assert!(search(&db, "spreadsheet")?.is_empty());
    assert_eq!(db.visual_detail(s.sheet)?, None);
    Ok(())
}

/// Observes `id` with a huge gap allowance so two calls make one long span.
fn span(
    f: &Fixture,
    m: MonitorId,
    id: VisualStateId,
    app: &str,
    title: &str,
    from: i64,
    to: i64,
) -> Fallible<()> {
    let application = f.store.upsert_application(
        &ApplicationContext {
            process_name: app.into(),
            exe_path: None,
        },
        Timestamp(from),
    )?;
    let window = f.store.upsert_window(
        application,
        &WindowContext {
            title: title.into(),
            class_name: None,
        },
        Timestamp(from),
    )?;
    for at in [from, to] {
        f.store.record_observation(&Observation {
            session: f.session,
            monitor: m,
            visual_state: id,
            application: Some(application),
            window: Some(window),
            at: Timestamp(at),
            max_gap_ms: i64::MAX / 4,
        })?;
    }
    Ok(())
}

#[test]
fn search_finds_a_state_that_was_on_screen_during_the_range() -> TestResult {
    let mut f = Fixture::new()?;
    let m1 = f.m1;
    let id = f.screen(m1, T0, "a.exe", "doc", &["steady words"])?;
    // One continuous observation 09:00-10:00 (an hour).
    span(&f, m1, id, "a.exe", "doc", T0, T0 + 60 * MIN)?;
    let db = f.query()?;
    let found = |since: i64, until: Option<i64>| -> Result<usize, QueryError> {
        Ok(db
            .search(&SearchQuery {
                text: "steady".into(),
                since: Some(Timestamp(since)),
                until: until.map(Timestamp),
                ..SearchQuery::default()
            })?
            .len())
    };
    assert_eq!(found(T0 + 30 * MIN, None)?, 1, "since inside the span");
    assert_eq!(
        found(T0 + 10 * MIN, Some(T0 + 20 * MIN))?,
        1,
        "range inside the span"
    );
    assert_eq!(found(T0 + 61 * MIN, None)?, 0, "after the span ended");
    Ok(())
}

#[test]
fn search_reports_context_from_the_matching_observation() -> TestResult {
    let mut f = Fixture::new()?;
    let m1 = f.m1;
    let id = f.screen(m1, T0, "mail.exe", "Inbox", &["shared screen text"])?;
    // The same pixels are later on screen under another application.
    span(
        &f,
        m1,
        id,
        "chat.exe",
        "Team chat",
        T0 + 10 * MIN,
        T0 + 20 * MIN,
    )?;
    let db = f.query()?;
    let hits = db.search(&SearchQuery {
        text: "shared".into(),
        application: Some("chat".into()),
        ..SearchQuery::default()
    })?;
    assert_eq!(
        hits.len(),
        1,
        "a later observation's application must match"
    );
    assert_eq!(hits[0].application.as_deref(), Some("chat.exe"));
    assert_eq!(hits[0].window_title.as_deref(), Some("Team chat"));
    assert_eq!(hits[0].timestamp, Timestamp(T0 + 10 * MIN));
    let title = db.search(&SearchQuery {
        text: "shared".into(),
        title_contains: Some("inbox".into()),
        ..SearchQuery::default()
    })?;
    assert_eq!(title[0].application.as_deref(), Some("mail.exe"));
    Ok(())
}

#[test]
fn at_finds_a_long_span_buried_under_many_short_events() -> TestResult {
    let mut f = Fixture::new()?;
    let (m1, m2) = (f.m1, f.m2);
    let long = f.screen(m1, T0, "a.exe", "long", &["long"])?;
    span(&f, m1, long, "a.exe", "long", T0, T0 + 10 * MIN)?;
    for i in 0..70i64 {
        f.screen(
            m2,
            T0 + 10_000 + i * 1_000,
            "b.exe",
            &format!("flicker {i}"),
            &[],
        )?;
    }
    let db = f.query()?;
    let hit = db
        .at(Timestamp(T0 + 5 * MIN))?
        .ok_or("nothing at T0+5min")?;
    assert_eq!(hit.visual_state_id, long);
    Ok(())
}

#[test]
fn paging_never_skips_observations_that_share_a_timestamp() -> TestResult {
    let mut f = Fixture::new()?;
    let (m1, m2) = (f.m1, f.m2);
    // Two monitors observed on the same tick, twice.
    let ids = [
        f.screen(m1, T0, "a.exe", "one", &[])?,
        f.screen(m2, T0, "b.exe", "two", &[])?,
        f.screen(m1, T0 + MIN, "a.exe", "three", &[])?,
        f.screen(m2, T0 + MIN, "b.exe", "four", &[])?,
    ];
    let db = f.query()?;
    let full = db.recent(50, None)?;
    assert_eq!(full.len(), 4);
    let mut paged = Vec::new();
    let mut cursor = None;
    loop {
        let page = db.recent(1, cursor)?;
        let Some(last) = page.last() else { break };
        cursor = Some(last.cursor());
        paged.extend(page);
    }
    assert_eq!(
        paged, full,
        "one-at-a-time paging must equal the full listing"
    );
    let seen: std::collections::BTreeSet<_> = paged.iter().map(|e| e.visual_state_id).collect();
    assert_eq!(seen, ids.iter().copied().collect());
    Ok(())
}
