//! The whole recorder, end to end, on fake backends: `run_with` with its real persist and OCR
//! threads writing a real SQLite store in a temp folder. Time is the fake clock; the world changes
//! in clock hooks that run between ticks on the recorder's own thread, so every step happens at an
//! exact tick. The only wait is a condition variable on the OCR thread's progress.

use super::fakes::*;
use crate::persist::Handoff;
use crate::recorder::run_with_queue;
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

#[allow(clippy::field_reassign_with_default)]
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

/// Issue #33: a window list that is unknown on every tick stores nothing, and the heartbeat that
/// `rsrewind status` / `doctor` / the window read says so while it lasts; once the list is back,
/// recording resumes and the heartbeat says saving.
#[test]
fn an_unknown_window_list_is_visible_in_the_heartbeat_and_recording_resumes() -> TestResult {
    let tmp = tempfile::tempdir()?;
    let data = DataDir::new(tmp.path().join("data"));
    let clock = FakeClock::new();
    let desktop = one_monitor_desktop();
    {
        let mut d = lock(&desktop);
        d.windows = None;
        d.show(display(0), frame(1)); // never stored: the list is unknown
    }
    // The heartbeat is written at t=0 and t=5 (every 5 s); read it between t=5 and t=6.
    let seen: Arc<Mutex<Option<std::collections::BTreeMap<String, u64>>>> =
        Arc::new(Mutex::new(None));
    {
        let (data, seen) = (data.clone(), seen.clone());
        clock.at(secs(5.5), move || {
            if let Ok(Some(status)) = Store::open_existing(&data).and_then(|s| s.read_status()) {
                *lock(&seen) = Some(status.counters);
            }
        });
    }
    {
        let d = desktop.clone();
        clock.at(secs(6.5), move || {
            let mut d = lock(&d);
            d.windows = Some(vec![window_on("notepad.exe", "notes.txt", 10, 0)]);
            d.show(display(0), frame(2)); // t=7: stored
        });
    }
    run_with_queue(
        RunOptions {
            data: data.clone(),
            config: config(false),
        },
        platform(&desktop, Capabilities::WINDOWS, &clock, secs(9.0), None),
        64,
        Arc::default(),
    )?;

    let mid = lock(&seen).clone().ok_or("no heartbeat read mid-run")?;
    assert_eq!(
        crate::health::from_counters(&mid),
        Some((
            crate::health::Health::WindowListUnknown,
            Some(FakeClock::wall_at(secs(0.0)))
        ))
    );
    assert_eq!(mid.get("persisted_states"), Some(&0));
    assert_eq!(mid.get("window_list_unknown_ticks"), Some(&6));

    let store = Store::open_existing(&data)?;
    let status = store.read_status()?.ok_or("no heartbeat")?;
    assert_eq!(status.counters.get("persisted_states"), Some(&1));
    assert_eq!(
        crate::health::from_counters(&status.counters).map(|(h, _)| h),
        Some(crate::health::Health::Saving)
    );
    let manifest = sealed(&data, FULL_TAGS)?;
    let captured: Vec<i64> = manifest.states.iter().map(|s| s.captured_at).collect();
    assert_eq!(captured, [ms(7.0)], "only the frame taken after recovery");
    Ok(())
}

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

    // A queue deep enough that the real persist thread never has to keep pace with the fake
    // clock: every job of the run is accepted, so what is stored depends only on the script.
    run_with_queue(
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
        64,
        Arc::default(),
    )?;

    // Shutdown order: by the time run_with returns, the OCR thread has finished (its backend is
    // dropped), the session is closed at the stop instant, and the last heartbeat says stopped.
    assert!(
        probe.dropped.load(Ordering::SeqCst),
        "OCR thread not joined"
    );
    assert_eq!(probe.created.load(Ordering::SeqCst), 1);
    let store = Store::open_existing(&data)?;
    let status = store.read_status()?.ok_or("no heartbeat")?;
    assert_eq!(status.state, CaptureState::Stopped);
    // The run's stop token is published, so `rsrewind stop` can name exactly this run.
    assert_eq!(
        status.counters.get(crate::counters::STOP_TOKEN_KEY),
        Some(&FAKE_STOP_TOKEN)
    );
    assert_eq!(status.counters.get("persisted_states"), Some(&4));
    assert_eq!(status.counters.get("privacy_skips"), Some(&2));
    assert_eq!(status.counters.get("paused_ticks"), Some(&2));
    assert_eq!(status.counters.get("dropped_queue_full"), Some(&0));
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

/// Waits (real time, bounded) until the persist thread has handled every job queued so far, with
/// all its effects visible: rows, observations, the failure gauge, the monitors it marked to be
/// offered again. Hooks run on the recorder thread between ticks, so nothing is queued while this
/// waits, and the next tick sees exactly what those jobs left behind.
fn settle(handoff: &Handoff) -> bool {
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    while std::time::Instant::now() < deadline {
        if handoff.settled() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    false
}

/// `forget` (or retention) deletes the application and window rows nothing references any more,
/// which after "forget the last 10 minutes" is usually the app still in front. The persist thread
/// must look them up again instead of failing every later moment of that app with
/// `ContextMissing` until the recorder restarts (found 2026-10-16, road-to-1.0 audit).
#[test]
fn moments_after_a_forget_of_the_app_in_front_are_still_recorded() -> TestResult {
    let tmp = tempfile::tempdir()?;
    let data = DataDir::new(tmp.path().join("data"));
    let clock = FakeClock::new();
    let desktop = one_monitor_desktop();
    let handoff = Arc::new(Handoff::default());
    lock(&desktop).show(display(0), frame(1)); // t=0: stored, notepad / notes.txt
    let forgot = Arc::new(Mutex::new(None));
    {
        let (d, data, forgot, handoff) = (
            desktop.clone(),
            data.clone(),
            forgot.clone(),
            handoff.clone(),
        );
        clock.at(secs(1.5), move || {
            // `rsrewind forget` from another process: everything so far, notepad's rows with it.
            // Only once the picture, its observation and notepad's rows are all written, or the
            // forget could miss the rows the persist thread has cached.
            if settle(&handoff)
                && let Ok(store) = Store::open_existing(&data)
                && let Ok(before) = store.stats()
            {
                *lock(&forgot) = store
                    .delete_range(Timestamp(ms(0.0) - 1), Timestamp(ms(1.5)))
                    .ok()
                    .map(|r| (r.visual_states_deleted, before.observations));
            }
            lock(&d).show(display(0), frame(2)); // t=2: same app and window, new picture
        });
    }
    {
        let d = desktop.clone();
        clock.at(secs(3.5), move || lock(&d).show(display(0), frame(3))); // t=4
    }
    run_with_queue(
        RunOptions {
            data: data.clone(),
            config: config(false),
        },
        platform(&desktop, Capabilities::WINDOWS, &clock, secs(6.0), None),
        64,
        handoff,
    )?;

    assert_eq!(
        *lock(&forgot),
        Some((1, 1)),
        "the forget did not run as scripted (one picture, after its observation was written)"
    );
    let store = Store::open_existing(&data)?;
    let status = store.read_status()?.ok_or("no heartbeat")?;
    assert_eq!(status.counters.get("persist_errors"), Some(&0));
    let manifest = sealed(&data, FULL_TAGS)?;
    let captured: Vec<i64> = manifest.states.iter().map(|s| s.captured_at).collect();
    assert_eq!(captured, [ms(2.0), ms(4.0)]);
    // Each stored picture is on the timeline (an observation shows it), credited to notepad.
    for (index, state) in manifest.states.iter().enumerate() {
        let shown = manifest.events.iter().any(|e| {
            e.kind == "observation"
                && e.state == u32::try_from(index).ok()
                && e.application.is_some()
        });
        assert!(
            shown,
            "state at {} has no credited observation",
            state.captured_at
        );
    }
    Ok(())
}

/// A full disk (here: `media/` replaced by a file, so no picture can be written) must not read as
/// "Recording" while nothing is saved: the heartbeat says `storage_failing`, and once writing works
/// again the screen on display is saved without waiting for it to change (road-to-1.0 audit,
/// 2026-10-16; before, the capture loop counted a queued frame as saved and never offered a
/// static screen again). Every hook first waits for the persist thread to finish what was queued,
/// so each tick sees the outcome of the one before and the run is exact (astra review, 2026-10-19).
#[test]
fn a_failing_disk_is_reported_and_recording_recovers_when_it_has_room() -> TestResult {
    let tmp = tempfile::tempdir()?;
    let data = DataDir::new(tmp.path().join("data"));
    let clock = FakeClock::new();
    let desktop = one_monitor_desktop();
    let handoff = Arc::new(Handoff::default());
    lock(&desktop).show(display(0), frame(1)); // t=0: stored
    let parked: Arc<Mutex<Option<std::path::PathBuf>>> = Arc::new(Mutex::new(None));
    {
        let (d, data, parked, handoff) = (
            desktop.clone(),
            data.clone(),
            parked.clone(),
            handoff.clone(),
        );
        clock.at(secs(1.5), move || {
            if settle(&handoff) {
                *lock(&parked) = break_media(&data).ok();
            }
            lock(&d).show(display(0), frame(2)); // t=2..11: never stored
        });
    }
    for t in 2..=12 {
        let handoff = handoff.clone();
        clock.at(secs(f64::from(t) + 0.5), move || {
            settle(&handoff);
        });
    }
    let seen: Arc<Mutex<Option<std::collections::BTreeMap<String, u64>>>> =
        Arc::new(Mutex::new(None));
    {
        let (data, seen) = (data.clone(), seen.clone());
        clock.at(secs(10.6), move || {
            if let Ok(Some(status)) = Store::open_existing(&data).and_then(|s| s.read_status()) {
                *lock(&seen) = Some(status.counters);
            }
        });
    }
    {
        let (data, parked) = (data.clone(), parked.clone());
        clock.at(secs(11.7), move || {
            // Room again (after the t=11 picture failed). The screen still shows picture 2 and
            // does not change.
            if let Some(parked) = lock(&parked).take() {
                let _ = restore_media(&data, &parked);
            }
        });
    }
    run_with_queue(
        RunOptions {
            data: data.clone(),
            config: config(false),
        },
        platform(&desktop, Capabilities::WINDOWS, &clock, secs(16.0), None),
        64,
        handoff,
    )?;

    let mid = lock(&seen).clone().ok_or("no heartbeat read mid-run")?;
    assert_eq!(
        crate::health::from_counters(&mid).map(|(h, _)| h),
        Some(crate::health::Health::StorageFailing),
        "{mid:?}"
    );
    assert!(
        mid.get("persist_errors").is_some_and(|&n| n >= 3),
        "{mid:?}"
    );

    let store = Store::open_existing(&data)?;
    let status = store.read_status()?.ok_or("no heartbeat")?;
    assert_eq!(
        crate::health::from_counters(&status.counters).map(|(h, _)| h),
        Some(crate::health::Health::Saving),
        "recovered"
    );
    assert_eq!(status.counters.get("persist_failing"), Some(&0));
    // Picture 2 was offered on every tick from 2 to 11 and failed each time.
    assert_eq!(status.counters.get("persist_errors"), Some(&10));
    let manifest = sealed(&data, FULL_TAGS)?;
    let captured: Vec<i64> = manifest.states.iter().map(|s| s.captured_at).collect();
    // The unchanged screen is saved once there is room, exactly once.
    assert_eq!(captured, [ms(0.0), ms(12.0)]);
    Ok(())
}
