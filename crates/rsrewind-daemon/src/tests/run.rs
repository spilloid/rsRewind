//! The whole recorder, end to end, on fake backends: `run_with` with its real persist and OCR
//! threads writing a real SQLite store in a temp folder. Time is the fake clock; the world changes
//! in clock hooks that run between ticks on the recorder's own thread, so every step happens at an
//! exact tick. The only wait is a condition variable on the OCR thread's progress.

use super::fakes::*;
use crate::{RunOptions, run_with};
use rsrewind_core::{
    Capabilities, CaptureState, Config, DataDir, PrivacyPolicy, SearchQuery, Timestamp,
};
use rsrewind_segment::{Manifest, Segment};
use rsrewind_storage::{ExportOptions, Store};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Duration;

type TestResult = Result<(), Box<dyn std::error::Error>>;

const FULL_TAGS: &[&str] = &["ocr", "window_titles", "process_names", "multi_monitor"];

fn secs(s: f64) -> Duration {
    Duration::from_secs_f64(s)
}

fn ms(s: f64) -> i64 {
    FakeClock::wall_at(secs(s)).0
}

fn config(unenforced_ok: bool) -> Config {
    let mut config = Config::default();
    config.privacy = PrivacyPolicy {
        unenforced_ok,
        ..PrivacyPolicy::suggested_defaults()
    };
    config.capture.idle_after_secs = 60;
    config
}

fn one_monitor_desktop() -> SharedDesktop {
    Arc::new(Mutex::new(Desktop {
        monitors: vec![monitor(0)],
        windows: Some(vec![window_on("notepad.exe", "notes.txt", 10, 0)]),
        focus: Some(focus("notepad.exe", "notes.txt", 10)),
        idle_ms: Some(0),
        ..Desktop::default()
    }))
}

/// Seals everything the store recorded into one segment and returns its manifest: the most
/// complete read-back of events (markers included), states and OCR the crate graph offers.
fn sealed(data: &DataDir, tags: &[&str]) -> Result<Manifest, Box<dyn std::error::Error>> {
    let store = Store::open_existing(data)?;
    let out = data.root().join("test-outbox");
    let report = store
        .export_segment(
            &out,
            &ExportOptions {
                label: "test",
                platform: "test",
                app_version: "0",
                capabilities: tags,
                now: Timestamp(ms(3_600.0)),
                settle_ms: rsrewind_storage::MIN_SETTLE_MS,
                max_events: 10_000,
            },
        )?
        .ok_or("nothing to seal")?;
    Ok(Segment::open(&report.path)?.manifest().clone())
}

// ----- fail closed at start ------------------------------------------------------------------

#[test]
fn refuses_to_start_where_privacy_cannot_be_enforced_unless_opted_in() -> TestResult {
    let tmp = tempfile::tempdir()?;
    let data = DataDir::new(tmp.path().join("data"));
    let clock = FakeClock::new();
    let desktop = one_monitor_desktop();

    // No window list: the any-visible-window rule cannot hold.
    let no_list = Capabilities {
        list_windows: false,
        ..Capabilities::WINDOWS
    };
    let error = run_with(
        RunOptions {
            data: data.clone(),
            config: config(false),
        },
        platform(&desktop, no_list, &clock, secs(3.0), None),
    )
    .err()
    .ok_or("recorder started without enforceable privacy rules")?;
    let message = error.to_string();
    assert!(message.contains("window list"), "{message}");
    assert!(message.contains("unenforced_ok = true"), "{message}");
    // Refused before touching anything: no data folder, no session, no frame looked at.
    assert!(!data.root().exists());
    assert_eq!(lock(&desktop).polls(display(0)), 0);

    // Titles unavailable while title rules exist: same refusal, naming the gap.
    let no_titles = Capabilities {
        window_titles: false,
        ..Capabilities::WINDOWS
    };
    let error = run_with(
        RunOptions {
            data: data.clone(),
            config: config(false),
        },
        platform(&desktop, no_titles, &clock, secs(3.0), None),
    )
    .err()
    .ok_or("recorder started without window titles while title rules exist")?;
    assert!(error.to_string().contains("window titles"));
    assert!(!data.root().exists());
    Ok(())
}

#[test]
fn an_opted_in_unenforced_run_records_it_and_never_claims_window_titles() -> TestResult {
    let tmp = tempfile::tempdir()?;
    let data = DataDir::new(tmp.path().join("data"));
    let clock = FakeClock::new();
    let desktop = one_monitor_desktop();
    lock(&desktop).show(display(0), frame(1));
    let no_list = Capabilities {
        list_windows: false,
        ..Capabilities::WINDOWS
    };
    run_with(
        RunOptions {
            data: data.clone(),
            config: config(true),
        },
        platform(&desktop, no_list, &clock, secs(3.0), None),
    )?;

    let store = Store::open_existing(&data)?;
    let (_, recorded) = store
        .latest_session_capabilities()?
        .ok_or("session capabilities not recorded")?;
    assert!(!recorded.privacy_enforced);
    assert_eq!(recorded.privacy_gaps, ["window list"]);
    assert!(!recorded.capabilities.ocr, "no OCR backend was supplied");
    assert_eq!(lock(&desktop).window_queries, 0);

    // The export claims only what the session had, whatever the build would otherwise claim.
    let manifest = sealed(&data, FULL_TAGS)?;
    assert_eq!(
        manifest.source.capabilities,
        ["process_names", "multi_monitor"]
    );
    assert_eq!(manifest.states.len(), 1, "it did record");
    Ok(())
}

// ----- end to end ----------------------------------------------------------------------------

#[test]
fn records_searchable_history_with_privacy_and_pause_holes_and_stops_in_order() -> TestResult {
    let tmp = tempfile::tempdir()?;
    let data = DataDir::new(tmp.path().join("data"));
    let clock = FakeClock::new();
    let desktop = one_monitor_desktop();
    let probe = Arc::new(OcrProbe::default());

    // Ticks run at t = 0, 1, 2, ... s. Each hook fires between two ticks.
    lock(&desktop).show(display(0), frame(1)); // t=0: stored
    {
        let d = desktop.clone();
        clock.at(secs(1.5), move || lock(&d).show(display(0), frame(2))); // t=2: stored
    }
    {
        // t=3,4: a vault is on screen, and so is its picture. Nothing may be stored.
        let d = desktop.clone();
        clock.at(secs(2.5), move || {
            let mut d = lock(&d);
            d.windows = Some(vec![
                window_on("KeePassXC.exe", "Database", 20, 0),
                window_on("notepad.exe", "notes.txt", 10, 0),
            ]);
            d.show(display(0), frame(3));
        });
    }
    {
        let d = desktop.clone();
        clock.at(secs(4.5), move || {
            let mut d = lock(&d);
            d.windows = Some(vec![window_on("notepad.exe", "notes.txt", 10, 0)]);
            d.show(display(0), frame(4)); // t=5: stored
        });
    }
    {
        // `rsrewind pause` from another process between t=5 and t=6, then the screen changes.
        let (d, data) = (desktop.clone(), data.clone());
        clock.at(secs(5.5), move || {
            if let Ok(store) = Store::open(&data) {
                let _ = store.set_control(CaptureState::Paused { until: None });
            }
            lock(&d).show(display(0), frame(5));
        });
    }
    {
        let data = data.clone();
        clock.at(secs(7.5), move || {
            if let Ok(store) = Store::open(&data) {
                let _ = store.set_control(CaptureState::Recording); // t=8: resumed, #5 stored
            }
        });
    }
    {
        // Barrier: let OCR catch up with the four stored states before the stop at t=10.
        let probe = probe.clone();
        clock.at(secs(8.5), move || {
            let _ = probe.wait_for(4, Duration::from_secs(60));
        });
    }

    run_with(
        RunOptions {
            data: data.clone(),
            config: config(false),
        },
        platform(
            &desktop,
            Capabilities::WINDOWS,
            &clock,
            secs(10.0),
            Some(fake_ocr(probe.clone())),
        ),
    )?;

    // Shutdown order: by the time run_with returns, the OCR thread has finished (its backend is
    // dropped), the session is closed at the stop instant, and the last heartbeat says stopped.
    assert!(probe.dropped.load(Ordering::SeqCst), "OCR thread not joined");
    assert_eq!(probe.created.load(Ordering::SeqCst), 1);
    let store = Store::open_existing(&data)?;
    let status = store.read_status()?.ok_or("no heartbeat")?;
    assert_eq!(status.state, CaptureState::Stopped);
    assert_eq!(status.counters.get("persisted_states"), Some(&4));
    assert_eq!(status.counters.get("privacy_skips"), Some(&2));
    assert_eq!(status.counters.get("paused_ticks"), Some(&2));
    let stats = store.stats()?;
    assert_eq!((stats.visual_states, stats.pending_ocr), (4, 0));

    // Search finds every stored screen and nothing from the excluded one.
    let query = rsrewind_query::QueryDb::open(&data)?;
    let found = |text: &str| -> Result<usize, Box<dyn std::error::Error>> {
        Ok(query
            .search(&SearchQuery {
                text: text.into(),
                since: None,
                until: None,
                application: None,
                title_contains: None,
                limit: 50,
            })?
            .len())
    };
    assert_eq!(found("screen")?, 4);
    assert_eq!(found("\"screen 3\"")?, 0);

    // Event order and holes, read back from a sealed segment.
    let manifest = sealed(&data, FULL_TAGS)?;
    assert_eq!(manifest.source.capabilities, FULL_TAGS);
    let session = manifest.sessions.first().ok_or("no session")?;
    assert_eq!(session.ended_at, Some(ms(10.0)));
    let mut events = manifest.events.clone();
    events.sort_by_key(|e| (e.started_at, e.kind != "recorder_stop"));
    let kinds: Vec<(&str, i64)> = events
        .iter()
        .filter(|e| e.kind != "observation")
        .map(|e| (e.kind.as_str(), e.started_at))
        .collect();
    assert_eq!(
        kinds,
        [
            ("recorder_start", ms(0.0)),
            ("paused", ms(6.0)),
            ("resumed", ms(8.0)),
            ("recorder_stop", ms(10.0)),
        ]
    );
    let captured: Vec<i64> = manifest.states.iter().map(|s| s.captured_at).collect();
    assert_eq!(captured, [ms(0.0), ms(2.0), ms(5.0), ms(8.0)]);
    for e in events.iter().filter(|e| e.kind == "observation") {
        // Nothing observed while the vault was visible (3..5) or while paused (6..8).
        for (from, to) in [(ms(3.0), ms(5.0)), (ms(6.0), ms(8.0))] {
            assert!(
                e.ended_at < from || e.started_at >= to,
                "observation {}..{} overlaps the hole {from}..{to}",
                e.started_at,
                e.ended_at
            );
        }
    }
    let media_files = walk(&data.root().join("media"))?;
    assert_eq!(media_files, 4, "a file was written for an unstored frame");
    Ok(())
}

fn walk(dir: &std::path::Path) -> Result<usize, Box<dyn std::error::Error>> {
    let mut files = 0;
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            files += walk(&entry.path())?;
        } else {
            files += 1;
        }
    }
    Ok(files)
}
