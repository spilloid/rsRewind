//! The capture loop, tick by tick, against fake backends. The persist queue is a real bounded
//! channel whose receiving end the test holds, so "what would be persisted, in what order" is
//! observed exactly, with no thread and no sleep in between.

use super::fakes::*;
use crate::counters::Counters;
use crate::health::Health;
use crate::persist::Job;
use crate::platform::Clock;
use crate::recorder::{Backends, PERSIST_QUEUE, RULE_WINDOWS_UNKNOWN, Recorder};
use rsrewind_core::{Capabilities, CaptureState, Config, PrivacyPolicy};
use std::sync::atomic::Ordering;
use std::sync::mpsc::{Receiver, sync_channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

type TestResult = Result<(), Box<dyn std::error::Error>>;

const REC: CaptureState = CaptureState::Recording;
const PAUSED: CaptureState = CaptureState::Paused { until: None };

struct Harness {
    recorder: Recorder,
    jobs: Receiver<Job>,
    desktop: SharedDesktop,
    clock: Arc<FakeClock>,
    counters: Arc<Counters>,
}

#[allow(clippy::field_reassign_with_default)]
fn config() -> Config {
    let mut config = Config::default();
    config.privacy = PrivacyPolicy {
        excluded_processes: vec!["KeePassXC.exe".into()],
        excluded_title_patterns: vec!["*Incognito*".into()],
        unenforced_ok: false,
    };
    config.capture.idle_after_secs = 60;
    config
}

fn harness(monitors: u64, capabilities: Capabilities) -> Harness {
    let desktop: SharedDesktop = Arc::new(Mutex::new(Desktop {
        monitors: (0..monitors).map(monitor).collect(),
        windows: Some(Vec::new()),
        idle_ms: Some(0),
        ..Desktop::default()
    }));
    let clock = FakeClock::new();
    let counters = Arc::new(Counters::default());
    let (tx, rx) = sync_channel(PERSIST_QUEUE);
    let recorder = Recorder::new(
        config(),
        capabilities,
        Backends {
            context: Box::new(FakeContext {
                desktop: desktop.clone(),
                capabilities,
            }),
            capture: Box::new(FakeCapture {
                desktop: desktop.clone(),
            }),
            idle: Box::new(FakeIdle {
                desktop: desktop.clone(),
            }),
            clock: clock.clone(),
        },
        tx,
        counters.clone(),
    );
    Harness {
        recorder,
        jobs: rx,
        desktop,
        clock,
        counters,
    }
}

impl Harness {
    /// One capture tick at the clock's current time, then one second passes.
    fn tick(&mut self, control: CaptureState) {
        let now = self.clock.now();
        self.recorder.tick(control, now);
        self.clock.advance(Duration::from_secs(1));
    }

    fn desktop(&self) -> std::sync::MutexGuard<'_, Desktop> {
        lock(&self.desktop)
    }

    /// Everything queued for the persist thread since the last call, summarised.
    fn drain(&self) -> Vec<String> {
        self.jobs.try_iter().map(|job| summary(&job)).collect()
    }
}

fn summary(job: &Job) -> String {
    match job {
        Job::NewState { monitor, frame, .. } => format!(
            "state {} #{}",
            monitor.device_name.trim_start_matches(r"\\.\"),
            seed_of(frame).map_or("?".to_string(), |s| s.to_string())
        ),
        Job::Extend { device_name, .. } => {
            format!("extend {}", device_name.trim_start_matches(r"\\.\"))
        }
        Job::Marker { kind, .. } => format!("marker {}", kind.as_str()),
    }
}

// ----- privacy -------------------------------------------------------------------------------

#[test]
fn an_excluded_window_skips_its_monitor_and_its_frame_is_never_stored_later() -> TestResult {
    let mut h = harness(2, Capabilities::WINDOWS);
    {
        let mut d = h.desktop();
        // The vault is visible on DISPLAY2 but not focused; the user types in DISPLAY1.
        d.windows = Some(vec![
            window_on("Code.exe", "main.rs", 10, 0),
            window_on("keepassxc.exe", "Passwords", 20, 1),
        ]);
        d.focus = Some(focus("Code.exe", "main.rs", 10));
        d.show(display(0), frame(1));
        d.show(display(1), frame(2));
    }
    h.tick(REC);
    assert_eq!(h.drain(), ["state DISPLAY1 #1"]);
    assert_eq!(h.counters.privacy_skips.load(Ordering::Relaxed), 1);
    // The excluded display was still drained, so the platform's pool never stalls.
    assert_eq!(h.desktop().polls(display(1)), 1);

    // The vault closes. No new frame arrives on DISPLAY2: the picture taken while it was visible
    // must not be stored now that the monitor is allowed again.
    h.desktop().windows = Some(vec![window_on("Code.exe", "main.rs", 10, 0)]);
    h.tick(REC);
    assert_eq!(h.drain(), ["extend DISPLAY1"]);

    // A fresh frame after the window is gone is stored as usual.
    h.desktop().show(display(1), frame(3));
    h.tick(REC);
    let mut jobs = h.drain();
    jobs.sort(); // monitors are visited in hash order
    assert_eq!(jobs, ["extend DISPLAY1", "state DISPLAY2 #3"]);
    Ok(())
}

#[test]
fn a_window_straddling_two_monitors_excludes_both() -> TestResult {
    let mut h = harness(3, Capabilities::WINDOWS);
    {
        let mut d = h.desktop();
        // Mostly on DISPLAY1, a sliver on DISPLAY2; DISPLAY3 is clear. Title rule, not process.
        d.windows = Some(vec![window(
            "chrome.exe",
            "Bank - Incognito",
            30,
            0,
            W as i32 + 5,
        )]);
        d.focus = Some(focus("explorer.exe", "Desktop", 1));
        for i in 0..3 {
            d.show(display(i), frame(i as u8 + 1));
        }
    }
    h.tick(REC);
    assert_eq!(h.drain(), ["state DISPLAY3 #3"]);
    assert_eq!(h.counters.privacy_skips.load(Ordering::Relaxed), 2);
    Ok(())
}

#[test]
fn an_excluded_foreground_window_missing_from_the_list_still_excludes_its_monitor() -> TestResult {
    let mut h = harness(2, Capabilities::WINDOWS);
    {
        let mut d = h.desktop();
        // Enumeration shows the vault's process as an ordinary window on DISPLAY2 (e.g. a
        // helper window), but focus names the excluded process with that pid.
        d.windows = Some(vec![
            window_on("Code.exe", "main.rs", 10, 0),
            window_on("helper.exe", "", 20, 1),
        ]);
        d.focus = Some(focus("KeePassXC.exe", "Unlock", 20));
        d.show(display(0), frame(1));
        d.show(display(1), frame(2));
    }
    h.tick(REC);
    assert_eq!(h.drain(), ["state DISPLAY1 #1"]);
    Ok(())
}

#[test]
fn a_failed_window_enumeration_stores_nothing_anywhere() -> TestResult {
    let mut h = harness(2, Capabilities::WINDOWS);
    {
        let mut d = h.desktop();
        d.windows = None;
        d.show(display(0), frame(1));
        d.show(display(1), frame(2));
    }
    h.tick(REC);
    assert_eq!(h.drain(), Vec::<String>::new());
    assert_eq!(h.counters.privacy_skips.load(Ordering::Relaxed), 2);
    assert!(RULE_WINDOWS_UNKNOWN.starts_with("unknown:"));
    Ok(())
}

#[test]
fn without_a_window_list_the_list_is_never_asked_for_and_an_excluded_focus_skips_every_monitor()
-> TestResult {
    let caps = Capabilities {
        list_windows: false,
        ..Capabilities::WINDOWS
    };
    let mut h = harness(2, caps);
    {
        let mut d = h.desktop();
        // Even if the provider had an answer, it must not be trusted.
        d.windows = Some(vec![window_on("Code.exe", "main.rs", 10, 0)]);
        d.focus = Some(focus("KeePassXC.exe", "Unlock", 20));
        d.show(display(0), frame(1));
        d.show(display(1), frame(2));
    }
    h.tick(REC);
    assert_eq!(h.drain(), Vec::<String>::new());
    assert_eq!(h.desktop().window_queries, 0);

    h.desktop().focus = Some(focus("Code.exe", "main.rs", 10));
    h.desktop().show(display(0), frame(3));
    h.desktop().show(display(1), frame(4));
    h.tick(REC);
    let mut jobs = h.drain();
    jobs.sort();
    assert_eq!(jobs, ["state DISPLAY1 #3", "state DISPLAY2 #4"]);
    assert_eq!(h.desktop().window_queries, 0);
    Ok(())
}

// ----- saving health (issue #33) -------------------------------------------------------------

fn gauge(h: &Harness) -> u64 {
    h.counters.health.load(Ordering::Relaxed)
}

#[test]
fn an_unknown_window_list_stores_nothing_says_why_and_recovers() -> TestResult {
    let mut h = harness(2, Capabilities::WINDOWS);
    h.desktop().windows = None;
    for seed in 1..=4u8 {
        h.desktop().show(display(0), frame(seed));
        h.desktop().show(display(1), frame(seed + 8));
        h.tick(REC);
        // Fail closed on every tick, from the first one on.
        assert_eq!(h.drain(), Vec::<String>::new(), "tick {seed}");
    }
    // Visible: the heartbeat's health gauge names the condition (after the debounce), and the
    // per-tick counter says how often.
    assert_eq!(h.recorder.health(), Health::WindowListUnknown);
    assert_eq!(gauge(&h), Health::WindowListUnknown.code());
    assert_eq!(
        h.counters.health_since_ms.load(Ordering::Relaxed),
        FakeClock::wall_at(Duration::ZERO).0 as u64,
        "since the first unknown tick"
    );
    assert_eq!(
        h.counters.window_list_unknown_ticks.load(Ordering::Relaxed),
        4
    );
    assert_eq!(h.counters.privacy_skips.load(Ordering::Relaxed), 8);

    // The list comes back: the next fresh frames are stored, the picture taken while the list was
    // unknown is not, and health says saving at once.
    h.desktop().windows = Some(Vec::new());
    h.tick(REC);
    assert_eq!(
        h.drain(),
        Vec::<String>::new(),
        "no stale frame after recovery"
    );
    h.desktop().show(display(0), frame(5));
    h.desktop().show(display(1), frame(6));
    h.tick(REC);
    let mut jobs = h.drain();
    jobs.sort();
    assert_eq!(jobs, ["state DISPLAY1 #5", "state DISPLAY2 #6"]);
    assert_eq!(h.recorder.health(), Health::Saving);
    assert_eq!(gauge(&h), 0);
    Ok(())
}

#[test]
fn one_bad_tick_does_not_change_the_reported_health() -> TestResult {
    let mut h = harness(1, Capabilities::WINDOWS);
    h.desktop().show(display(0), frame(1));
    h.tick(REC);
    h.desktop().windows = None;
    h.tick(REC);
    h.desktop().windows = Some(Vec::new());
    h.tick(REC);
    assert_eq!(h.recorder.health(), Health::Saving);
    assert_eq!(gauge(&h), 0);
    assert_eq!(
        h.counters.window_list_unknown_ticks.load(Ordering::Relaxed),
        1
    );
    Ok(())
}

/// A `tracing` writer into a shared buffer.
#[derive(Clone, Default)]
struct LogBuffer(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for LogBuffer {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        lock(&self.0).extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Runs `body` with every info-and-above log line captured, and returns them.
fn logs_at_info(body: impl FnOnce()) -> String {
    let buffer = LogBuffer::default();
    let writer = buffer.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(move || writer.clone())
        .with_max_level(tracing::Level::INFO)
        .with_ansi(false)
        .finish();
    tracing::subscriber::with_default(subscriber, body);
    String::from_utf8_lossy(&lock(&buffer.0)).into_owned()
}

#[test]
fn a_window_fault_message_never_reaches_a_log_line() -> TestResult {
    // A platform fault built from a parse error can echo input (serde_json quotes string
    // values), and the input is window titles. Only the reason name and counts may be logged.
    let mut h = harness(1, Capabilities::WINDOWS);
    {
        let mut d = h.desktop();
        d.windows = None;
        d.window_fault =
            Some(r#"KWin snapshot: invalid type: string "Bank - Account 12345""#.into());
    }
    let logs = logs_at_info(|| {
        for seed in 1..=4 {
            h.desktop().show(display(0), frame(seed));
            h.tick(REC);
        }
        h.desktop().windows = Some(Vec::new());
        h.tick(REC);
    });
    assert!(logs.contains("window_context_unavailable"), "{logs}");
    assert!(logs.contains("saving again"), "{logs}");
    assert!(!logs.contains("12345"), "{logs}");
    assert!(!logs.contains("Bank"), "{logs}");
    Ok(())
}

#[test]
fn unknown_idle_and_failing_capture_are_reported_as_such() -> TestResult {
    let mut h = harness(1, Capabilities::WINDOWS);
    h.desktop().idle_ms = None;
    for seed in 1..=3 {
        h.desktop().show(display(0), frame(seed));
        h.tick(REC);
    }
    assert_eq!(h.drain(), ["marker idle_start"]);
    assert_eq!(h.recorder.health(), Health::IdleUnknown);
    assert_eq!(gauge(&h), Health::IdleUnknown.code());
    assert_eq!(h.counters.idle_unknown_ticks.load(Ordering::Relaxed), 3);

    let mut h = harness(1, Capabilities::WINDOWS);
    h.desktop().capture_fails = true;
    for seed in 1..=3 {
        h.desktop().show(display(0), frame(seed));
        h.tick(REC);
    }
    assert_eq!(h.drain(), Vec::<String>::new());
    assert_eq!(h.recorder.health(), Health::CaptureFailing);
    assert_eq!(h.counters.capture_errors.load(Ordering::Relaxed), 3);

    h.desktop().capture_fails = false;
    h.desktop().show(display(0), frame(4));
    h.tick(REC);
    assert_eq!(h.drain(), ["state DISPLAY1 #4"]);
    assert_eq!(h.recorder.health(), Health::Saving);
    Ok(())
}

#[test]
fn expected_pauses_in_saving_are_not_faults() -> TestResult {
    let mut h = harness(1, Capabilities::WINDOWS);
    h.desktop().idle_ms = Some(61_000);
    for _ in 0..3 {
        h.tick(REC);
    }
    assert_eq!(h.recorder.health(), Health::Idle);
    assert!(!h.recorder.health().is_fault());

    h.desktop().idle_ms = Some(0);
    h.desktop().windows = Some(vec![window_on("KeePassXC.exe", "Vault", 20, 0)]);
    for seed in 1..=3 {
        h.desktop().show(display(0), frame(seed));
        h.tick(REC);
    }
    assert_eq!(h.recorder.health(), Health::PrivacyExcluded);
    assert!(!h.recorder.health().is_fault());
    Ok(())
}

// ----- pause ---------------------------------------------------------------------------------

#[test]
fn pause_is_a_barrier_ordered_before_anything_captured_after_it() -> TestResult {
    let mut h = harness(1, Capabilities::WINDOWS);
    h.desktop().show(display(0), frame(1));
    h.tick(REC);
    assert_eq!(h.drain(), ["state DISPLAY1 #1"]);
    let polls_before_pause = h.desktop().polls(display(0));

    // Paused: the screen changes, but the tick neither looks at it nor queues anything but the
    // pause marker.
    h.desktop().show(display(0), frame(2));
    h.tick(PAUSED);
    h.tick(PAUSED);
    assert_eq!(h.drain(), ["marker paused"]);
    assert_eq!(h.desktop().polls(display(0)), polls_before_pause);
    assert_eq!(h.counters.paused_ticks.load(Ordering::Relaxed), 2);

    // Resume: the marker is queued before the first post-pause state.
    h.tick(REC);
    assert_eq!(h.drain(), ["marker resumed", "state DISPLAY1 #2"]);

    // A span never bridges a pause: the same picture after a pause is a new observation, not an
    // extension of the one before it.
    h.tick(PAUSED);
    h.tick(REC);
    assert_eq!(
        h.drain(),
        ["marker paused", "marker resumed", "state DISPLAY1 #2"]
    );
    Ok(())
}

#[test]
fn a_timed_pause_ends_by_itself() -> TestResult {
    let mut h = harness(1, Capabilities::WINDOWS);
    let until = FakeClock::wall_at(Duration::from_secs(2));
    h.desktop().show(display(0), frame(1));
    h.tick(CaptureState::Paused { until: Some(until) }); // t=0
    h.tick(CaptureState::Paused { until: Some(until) }); // t=1
    assert_eq!(h.drain(), ["marker paused"]);
    h.tick(CaptureState::Paused { until: Some(until) }); // t=2: expired
    assert_eq!(h.drain(), ["marker resumed", "state DISPLAY1 #1"]);
    Ok(())
}

// ----- idle ----------------------------------------------------------------------------------

#[test]
fn idle_stores_nothing_and_marks_both_edges() -> TestResult {
    let mut h = harness(1, Capabilities::WINDOWS);
    h.desktop().show(display(0), frame(1));
    h.tick(REC);
    assert_eq!(h.drain(), ["state DISPLAY1 #1"]);

    // Idle threshold is 60 s; the screen keeps changing (a clock, a video) while away.
    h.desktop().idle_ms = Some(61_000);
    h.desktop().show(display(0), frame(2));
    h.tick(REC);
    h.desktop().show(display(0), frame(3));
    h.tick(REC);
    assert_eq!(h.drain(), ["marker idle_start"]);
    assert_eq!(h.counters.idle_ticks.load(Ordering::Relaxed), 2);

    h.desktop().idle_ms = Some(0);
    h.tick(REC);
    assert_eq!(h.drain(), ["marker idle_end", "state DISPLAY1 #3"]);
    Ok(())
}

#[test]
fn unknown_idle_time_counts_as_idle() -> TestResult {
    let mut h = harness(1, Capabilities::WINDOWS);
    h.desktop().idle_ms = None;
    h.desktop().show(display(0), frame(1));
    h.tick(REC);
    assert_eq!(h.drain(), ["marker idle_start"]);
    Ok(())
}

// ----- backpressure --------------------------------------------------------------------------

#[test]
fn a_full_persist_queue_drops_candidates_and_never_blocks_capture() -> TestResult {
    // The recorder's backends are not `Send`, so the whole harness lives on a worker thread; only
    // the queue's receiving end and the counters come back. Nobody drains the queue, and every
    // tick shows a new picture. If a tick blocked on the full queue, `done` would never arrive.
    let (handoff_tx, handoff_rx) = sync_channel(1);
    let (done_tx, done_rx) = sync_channel::<()>(1);
    let worker = std::thread::spawn(move || {
        let Harness {
            mut recorder,
            jobs,
            desktop,
            clock,
            counters,
        } = harness(1, Capabilities::WINDOWS);
        let _ = handoff_tx.send((jobs, counters));
        for seed in 1..=(PERSIST_QUEUE as u8 + 3) {
            lock(&desktop).show(display(0), frame(seed));
            recorder.tick(REC, clock.now());
            clock.advance(Duration::from_secs(1));
        }
        let _ = done_tx.send(());
    });
    let (jobs, counters) = handoff_rx
        .recv_timeout(Duration::from_secs(30))
        .map_err(|_| "worker did not start")?;
    // A safety net for a broken build, not synchronisation: a correct loop finishes in
    // microseconds.
    done_rx
        .recv_timeout(Duration::from_secs(30))
        .map_err(|_| "capture blocked on a full persist queue")?;
    worker.join().map_err(|_| "tick thread panicked")?;

    assert_eq!(counters.dropped_queue_full.load(Ordering::Relaxed), 3);
    assert_eq!(counters.ticks.load(Ordering::Relaxed), 7);
    let queued: Vec<String> = jobs.try_iter().map(|j| summary(&j)).collect();
    assert_eq!(
        queued,
        [
            "state DISPLAY1 #1",
            "state DISPLAY1 #2",
            "state DISPLAY1 #3",
            "state DISPLAY1 #4"
        ]
    );
    Ok(())
}

/// Remediation F9, still open: a changed frame dropped on a full queue is not retried. The next
/// tick without a new frame extends the *previous* state, so history claims the old picture was
/// on screen while the new one was. Kept as a failing test so the fix has a target.
#[test]
#[ignore = "F9 open: dropped candidates are not kept dirty"]
fn f9_a_dropped_change_is_retried_not_papered_over_by_an_extension() -> TestResult {
    let mut h = harness(1, Capabilities::WINDOWS);
    for seed in 1..=PERSIST_QUEUE as u8 {
        h.desktop().show(display(0), frame(seed));
        h.tick(REC);
    }
    h.desktop().show(display(0), frame(9)); // dropped: queue full
    h.tick(REC);
    let _ = h.drain(); // the persist thread catches up
    h.tick(REC); // no new frame from the platform
    assert_eq!(h.drain(), ["state DISPLAY1 #9"]);
    Ok(())
}

// ----- markers -------------------------------------------------------------------------------

#[test]
fn markers_are_recorded_once_per_transition() -> TestResult {
    let mut h = harness(1, Capabilities::WINDOWS);
    for _ in 0..3 {
        h.tick(PAUSED);
    }
    for _ in 0..3 {
        h.tick(REC);
    }
    let kinds: Vec<String> = h.drain();
    assert_eq!(kinds, ["marker paused", "marker resumed"]);
    Ok(())
}
