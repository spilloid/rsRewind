//! The history facade over a central data folder: this machine's own store plus two replicas
//! imported from probes. Fixtures are built only through `rsrewind_storage` (recording, export,
//! import exactly as the real commands do) and read only through `History`.
//!
//! The probes deliberately share timestamps and, being separate databases, row ids: event 1 and
//! visual state 1 exist in every store. Any merge that orders or looks up by id alone, or a cursor
//! that is not total across sources, fails here.

use rsrewind_core::paths::media_relative_path;
use rsrewind_core::{
    ApplicationContext, BgraFrame, DataDir, MonitorInfo, OcrBlock, SearchQuery, SourceId,
    TimelineCursor, TimelineEntry, Timestamp, VisualStateId, WindowContext,
};
use rsrewind_query::{History, QueryError, SourceFilter, SourceKind};
use rsrewind_segment::Segment;
use rsrewind_storage::media::{encode_webp, write_webp_exclusive};
use rsrewind_storage::{
    ExportOptions, ImportOutcome, MIN_SETTLE_MS, NewVisualState, Observation, Store,
    import_into_root,
};
use std::collections::HashSet;
use std::path::Path;

type TestResult = Result<(), Box<dyn std::error::Error>>;
type Fallible<T> = Result<T, Box<dyn std::error::Error>>;

const T0: i64 = 1_790_000_000_000;
const SEC: i64 = 1_000;

/// One recorded moment: when, on which monitor, in which app/window, what text was on screen.
struct Moment {
    at: i64,
    monitor: usize,
    app: &'static str,
    title: &'static str,
    text: &'static str,
}

const fn m(
    at: i64,
    monitor: usize,
    app: &'static str,
    title: &'static str,
    text: &'static str,
) -> Moment {
    Moment {
        at,
        monitor,
        app,
        title,
        text,
    }
}

fn frame(seed: u8) -> BgraFrame {
    let (width, height) = (16u32, 8u32);
    let pixels = (0..width * height)
        .flat_map(|i| [seed, seed.wrapping_add(i as u8), 200, 255])
        .collect();
    BgraFrame {
        width,
        height,
        stride: width * 4,
        pixels,
    }
}

/// Records `moments` into a store at `data`, as the recorder would. Returns the store and the
/// encoded bytes of each moment's picture, in order.
fn record(
    data: &DataDir,
    host: &str,
    seed: u8,
    moments: &[Moment],
) -> Fallible<(Store, Vec<Vec<u8>>)> {
    let store = Store::open(data)?;
    let session = store.begin_session(Timestamp(T0 - SEC), host, "test")?;
    let monitors = [
        store.upsert_monitor(&monitor(r"\\.\DISPLAY1", 0), Timestamp(T0))?,
        store.upsert_monitor(&monitor(r"\\.\DISPLAY2", 1920), Timestamp(T0))?,
    ];
    let mut pictures = Vec::new();
    for (i, moment) in moments.iter().enumerate() {
        let monitor = monitors[moment.monitor];
        let bytes = encode_webp(&frame(seed.wrapping_add(i as u8 * 17)), 80)?;
        let relative = media_relative_path(Timestamp(moment.at), monitor);
        write_webp_exclusive(data, &relative, &bytes)?;
        let state = store.insert_visual_state(&NewVisualState {
            monitor,
            captured_at: Timestamp(moment.at),
            media_path: relative.clone(),
            width: 16,
            height: 8,
            byte_size: bytes.len() as u64,
            fingerprint: None,
            ocr_enabled: true,
        })?;
        store.save_ocr(
            state,
            &relative,
            &[OcrBlock {
                text: moment.text.into(),
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
        let application = store.upsert_application(
            &ApplicationContext {
                process_name: moment.app.into(),
                exe_path: None,
            },
            Timestamp(moment.at),
        )?;
        let window = store.upsert_window(
            application,
            &WindowContext {
                title: moment.title.into(),
                class_name: None,
            },
            Timestamp(moment.at),
        )?;
        let observe = |at: i64| {
            store.record_observation(&Observation {
                session,
                monitor,
                visual_state: state,
                application: Some(application),
                window: Some(window),
                at: Timestamp(at),
                max_gap_ms: 5 * SEC,
            })
        };
        observe(moment.at)?;
        // The screen holds for four seconds: one observation spanning [at, at + 4 s].
        observe(moment.at + 4 * SEC)?;
        pictures.push(bytes);
    }
    Ok((store, pictures))
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

/// Seals everything a probe recorded and imports it into `central`, returning the source id.
fn ship(store: &Store, label: &str, outbox: &Path, central: &DataDir) -> Fallible<SourceId> {
    let mut id = None;
    while let Some(report) = store.export_segment(
        outbox,
        &ExportOptions {
            label,
            platform: "test",
            app_version: "0.0.1",
            capabilities: &["ocr", "window_titles", "process_names"],
            now: Timestamp(T0 + 3_600 * SEC),
            settle_ms: MIN_SETTLE_MS,
            max_events: 3,
        },
    )? {
        let segment = Segment::open(&report.path)?;
        id = SourceId::parse(&segment.manifest().source.id);
        assert!(matches!(
            import_into_root(central, &segment)?,
            ImportOutcome::Imported(_)
        ));
    }
    Ok(id.ok_or("nothing was sealed")?)
}

struct Central {
    _tmp: tempfile::TempDir,
    root: DataDir,
    front: SourceId,
    kiosk: SourceId,
    front_pictures: Vec<Vec<u8>>,
}

/// This machine records three moments; FRONT-DESK-PC and kitchen-kiosk record moments at the
/// *same* instants (and get the same row ids), then are shipped here.
fn central() -> Fallible<Central> {
    let tmp = tempfile::tempdir()?;
    let root = DataDir::new(tmp.path().join("central"));
    record(
        &root,
        "central",
        1,
        &[
            m(T0, 0, "Code.exe", "main.rs", "local notes about konica"),
            m(T0 + 10 * SEC, 0, "Code.exe", "lib.rs", "nothing to see"),
            m(T0 + 20 * SEC, 1, "Terminal", "pwsh", "cargo test"),
        ],
    )?;
    let (front_store, front_pictures) = record(
        &DataDir::new(tmp.path().join("front")),
        "FRONT-DESK-PC",
        60,
        &[
            m(
                T0,
                0,
                "Chrome.exe",
                "Konica printer - Settings",
                "Konica bizhub C360",
            ),
            m(T0, 1, "Outlook.exe", "Inbox", "invoice 4471 overdue"),
            m(
                T0 + 10 * SEC,
                0,
                "Chrome.exe",
                "Supply order",
                "toner order",
            ),
            m(
                T0 + 30 * SEC,
                0,
                "Excel.exe",
                "Quarterly budget",
                "budget konica toner",
            ),
        ],
    )?;
    let (kiosk_store, _) = record(
        &DataDir::new(tmp.path().join("kiosk")),
        "kitchen-kiosk",
        120,
        &[
            m(T0, 0, "Kiosk.exe", "Menu", "lunch menu"),
            m(T0 + 10 * SEC, 0, "Kiosk.exe", "Orders", "order 8812 konica"),
            m(T0 + 40 * SEC, 0, "Kiosk.exe", "Menu", "dinner menu"),
        ],
    )?;
    let outbox = tmp.path().join("outbox");
    let front = ship(&front_store, "FRONT-DESK-PC", &outbox.join("front"), &root)?;
    let kiosk = ship(&kiosk_store, "kitchen-kiosk", &outbox.join("kiosk"), &root)?;
    Ok(Central {
        _tmp: tmp,
        root,
        front,
        kiosk,
        front_pictures,
    })
}

fn key(e: &TimelineEntry) -> TimelineCursor {
    e.cursor()
}

#[test]
fn sources_lists_this_machine_and_each_replica_with_its_label() -> TestResult {
    let c = central()?;
    let history = History::open(&c.root);
    assert!(history.problems().is_empty(), "{:?}", history.problems());
    let sources = history.sources()?;
    assert_eq!(sources.len(), 3);
    assert_eq!(sources[0].source, None);
    assert_eq!(sources[0].kind, SourceKind::Local);
    assert_eq!(sources[0].observations, 3);

    let front = sources
        .iter()
        .find(|s| s.source == Some(c.front))
        .ok_or("front")?;
    assert_eq!(front.kind, SourceKind::Replica);
    assert_eq!(front.label.as_deref(), Some("FRONT-DESK-PC"));
    assert_eq!(front.observations, 4);
    assert_eq!(front.first, Some(Timestamp(T0)));
    assert_eq!(front.last, Some(Timestamp(T0 + 34 * SEC)));
    let kiosk = sources
        .iter()
        .find(|s| s.source == Some(c.kiosk))
        .ok_or("kiosk")?;
    assert_eq!(kiosk.label.as_deref(), Some("kitchen-kiosk"));
    assert_eq!(kiosk.observations, 3);
    Ok(())
}

#[test]
fn recent_merges_every_source_newest_first_and_pages_without_gaps_or_repeats() -> TestResult {
    let c = central()?;
    let history = History::open(&c.root);
    let everything = history.recent(SourceFilter::All, 200, None)?;
    assert_eq!(everything.len(), 10);
    assert!(
        everything.windows(2).all(|w| key(&w[0]) > key(&w[1])),
        "strictly newest first in the cross-source order"
    );
    assert_eq!(everything[0].source, Some(c.kiosk));
    assert_eq!(everything[0].started_at, Timestamp(T0 + 40 * SEC));

    // Page sizes that split the four observations sharing T0 at every possible boundary.
    for page in 1..=4 {
        let mut seen = Vec::new();
        let mut after = None;
        loop {
            let batch = history.recent(SourceFilter::All, page, after)?;
            if batch.is_empty() {
                break;
            }
            assert!(batch.len() <= page as usize);
            after = batch.last().map(TimelineEntry::cursor);
            seen.extend(batch);
            // A cursor that does not advance would page forever; fail instead.
            assert!(
                seen.len() <= everything.len(),
                "page size {page} repeats entries"
            );
        }
        assert_eq!(seen, everything, "page size {page}");
    }
    Ok(())
}

#[test]
fn a_cursor_at_a_time_starts_there_on_every_source() -> TestResult {
    let c = central()?;
    let history = History::open(&c.root);
    let from = history.recent(
        SourceFilter::All,
        200,
        Some(TimelineCursor::at_or_before(Timestamp(T0 + 10 * SEC))),
    )?;
    assert!(
        from.iter()
            .all(|e| e.started_at <= Timestamp(T0 + 10 * SEC))
    );
    // Three at T0+10 s (one per source) and four at T0.
    assert_eq!(from.len(), 7);
    assert_eq!(
        from.iter()
            .filter(|e| e.started_at == Timestamp(T0 + 10 * SEC))
            .count(),
        3
    );
    Ok(())
}

#[test]
fn filters_select_one_source() -> TestResult {
    let c = central()?;
    let history = History::open(&c.root);
    let front = history.recent(SourceFilter::Remote(c.front), 200, None)?;
    assert_eq!(front.len(), 4);
    assert!(front.iter().all(|e| e.source == Some(c.front)));
    let local = history.recent(SourceFilter::Local, 200, None)?;
    assert_eq!(local.len(), 3);
    assert!(local.iter().all(|e| e.source.is_none()));
    assert!(
        local
            .iter()
            .all(|e| e.application.as_deref() != Some("Chrome.exe"))
    );

    let unknown = SourceId::parse("ffffffffffffffffffffffffffffffff").ok_or("id")?;
    assert!(
        history
            .recent(SourceFilter::Remote(unknown), 10, None)?
            .is_empty()
    );
    Ok(())
}

#[test]
fn at_finds_the_covering_moment_across_sources() -> TestResult {
    let c = central()?;
    let history = History::open(&c.root);
    // T0+32 s: only FRONT-DESK-PC's budget sheet (T0+30..34 s) is on screen anywhere.
    let entry = history
        .at(Timestamp(T0 + 32 * SEC), SourceFilter::All)?
        .ok_or("at")?;
    assert_eq!(entry.source, Some(c.front));
    assert_eq!(entry.window_title.as_deref(), Some("Quarterly budget"));
    // T0+36 s: nothing covers it; the nearest earlier start wins, whichever source it is on.
    let entry = history
        .at(Timestamp(T0 + 36 * SEC), SourceFilter::All)?
        .ok_or("at")?;
    assert_eq!(entry.started_at, Timestamp(T0 + 30 * SEC));
    // A filter is honoured even when another source has a better match.
    let entry = history
        .at(Timestamp(T0 + 32 * SEC), SourceFilter::Remote(c.kiosk))?
        .ok_or("at")?;
    assert_eq!(entry.source, Some(c.kiosk));
    assert_eq!(entry.window_title.as_deref(), Some("Orders"));
    assert_eq!(
        history.at(Timestamp(T0 - 60 * SEC), SourceFilter::All)?,
        None
    );
    Ok(())
}

#[test]
fn search_merges_hits_and_attributes_each_to_its_source() -> TestResult {
    let c = central()?;
    let history = History::open(&c.root);
    let query = SearchQuery {
        text: "konica".into(),
        limit: 50,
        ..SearchQuery::default()
    };
    let hits = history.search(&query, SourceFilter::All)?;
    let mut by_source: Vec<(Option<SourceId>, String)> = hits
        .iter()
        .map(|h| (h.source, h.window_title.clone().unwrap_or_default()))
        .collect();
    by_source.sort();
    let mut expected = vec![
        (None, "main.rs".to_owned()),
        (Some(c.front), "Konica printer - Settings".to_owned()),
        (Some(c.front), "Quarterly budget".to_owned()),
        (Some(c.kiosk), "Orders".to_owned()),
    ];
    expected.sort();
    assert_eq!(by_source, expected);
    assert!(hits.windows(2).all(|w| w[0].rank <= w[1].rank));

    // Visual state ids collide across stores; detail must come from the hit's own source.
    for hit in &hits {
        let detail = history
            .visual_detail(hit.source, hit.visual_state_id)?
            .ok_or("detail")?;
        assert_eq!(detail.source, hit.source);
        assert!(
            detail.ocr_text.to_lowercase().contains("konica"),
            "{detail:?}"
        );
        assert_eq!(detail.window_title, hit.window_title);
    }

    let only_kiosk = history.search(&query, SourceFilter::Remote(c.kiosk))?;
    assert_eq!(only_kiosk.len(), 1);
    assert_eq!(only_kiosk[0].snippet, "order 8812 [konica]");

    // Empty text: a time browser across sources, newest first, limit honoured after the merge.
    let browse = history.search(
        &SearchQuery {
            limit: 3,
            ..SearchQuery::default()
        },
        SourceFilter::All,
    )?;
    assert_eq!(browse.len(), 3);
    assert_eq!(browse[0].source, Some(c.kiosk));
    assert!(browse.windows(2).all(|w| w[0].timestamp >= w[1].timestamp));
    Ok(())
}

#[test]
fn frames_are_read_only_from_their_own_sources_media() -> TestResult {
    let c = central()?;
    let history = History::open(&c.root);
    let media = history.media();
    let hits = history.search(
        &SearchQuery {
            text: "bizhub".into(),
            limit: 5,
            ..SearchQuery::default()
        },
        SourceFilter::All,
    )?;
    let hit = hits.first().ok_or("hit")?;
    assert_eq!(hit.source, Some(c.front));
    assert_eq!(
        media.frame_bytes(hit.source, &hit.media_path)?,
        c.front_pictures[0]
    );
    let decoded = media.frame(hit.source, &hit.media_path)?;
    assert_eq!((decoded.width, decoded.height), (16, 8));
    assert_eq!(decoded.pixels.len(), 16 * 8 * 4);

    // The same path claimed by another source, or by this machine, is refused.
    assert!(media.frame_bytes(Some(c.kiosk), &hit.media_path).is_err());
    assert!(media.frame_bytes(None, &hit.media_path).is_err());
    // Escapes and non-media files are refused even inside the right source.
    let replica = c.root.source_dir(&c.front.to_string()).ok_or("dir")?;
    for bad in [
        replica.database(),
        replica.root().join("media").join("..").join("recall.db"),
        c.root.database(),
        replica.root().join("media"),
    ] {
        let err = media.frame_bytes(Some(c.front), &bad.to_string_lossy());
        assert!(
            matches!(err, Err(QueryError::MediaPath(_))),
            "{bad:?}: {err:?}"
        );
    }
    let unknown = SourceId::parse("ffffffffffffffffffffffffffffffff").ok_or("id")?;
    assert!(matches!(
        media.frame_bytes(Some(unknown), &hit.media_path),
        Err(QueryError::UnknownSource(_))
    ));
    assert!(matches!(
        history.visual_detail(Some(unknown), VisualStateId(1)),
        Err(QueryError::UnknownSource(_))
    ));
    Ok(())
}

#[test]
fn a_folder_that_lies_about_its_source_is_skipped_and_reported() -> TestResult {
    let c = central()?;
    // Someone copies FRONT-DESK-PC's replica into a folder named for a different id. Using it
    // would attribute FRONT-DESK-PC's screens to a machine that never showed them.
    let impostor = "0123456789abcdef0123456789abcdef";
    let from = c.root.source_dir(&c.front.to_string()).ok_or("dir")?;
    let to = c.root.source_dir(impostor).ok_or("dir")?;
    copy_dir(from.root(), to.root())?;
    // Not a source id at all: ignored, not interpreted.
    std::fs::create_dir_all(c.root.sources_root().join("notes"))?;
    // A source folder with no database yet.
    let empty = "fedcba9876543210fedcba9876543210";
    std::fs::create_dir_all(c.root.sources_root().join(empty))?;

    let history = History::open(&c.root);
    let ids: HashSet<Option<SourceId>> = history.sources()?.iter().map(|s| s.source).collect();
    assert_eq!(ids, HashSet::from([None, Some(c.front), Some(c.kiosk)]));
    let reported: Vec<String> = history
        .problems()
        .iter()
        .map(|p| p.data_dir.to_string_lossy().into_owned())
        .collect();
    assert_eq!(reported.len(), 2, "{:?}", history.problems());
    assert!(reported.iter().any(|d| d.ends_with(impostor)));
    assert!(reported.iter().any(|d| d.ends_with(empty)));
    let everything = history.recent(SourceFilter::All, 200, None)?;
    assert_eq!(everything.len(), 10, "the impostor's rows must not appear");
    Ok(())
}

#[test]
fn a_replica_opened_as_the_root_is_still_attributed_to_its_source() -> TestResult {
    let c = central()?;
    let replica = c.root.source_dir(&c.kiosk.to_string()).ok_or("dir")?;
    let history = History::open(&replica);
    let sources = history.sources()?;
    assert_eq!(sources.len(), 1);
    assert_eq!(sources[0].source, Some(c.kiosk));
    assert_eq!(sources[0].kind, SourceKind::Replica);
    assert!(
        history
            .recent(SourceFilter::All, 10, None)?
            .iter()
            .all(|e| e.source == Some(c.kiosk))
    );
    Ok(())
}

#[test]
fn an_empty_data_folder_is_an_empty_history() -> TestResult {
    let tmp = tempfile::tempdir()?;
    let history = History::open(&DataDir::new(tmp.path().join("nothing-here")));
    assert!(history.sources()?.is_empty());
    assert!(history.problems().is_empty());
    assert!(history.recent(SourceFilter::All, 10, None)?.is_empty());
    assert!(history.at(Timestamp(T0), SourceFilter::All)?.is_none());
    assert!(
        history
            .search(&SearchQuery::default(), SourceFilter::All)?
            .is_empty()
    );
    Ok(())
}

fn copy_dir(from: &Path, to: &Path) -> Fallible<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}
