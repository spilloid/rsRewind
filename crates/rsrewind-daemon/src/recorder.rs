//! The capture loop and recorder lifecycle.

use crate::counters::{self, Counters};
use crate::persist::{self, Job, PersistConfig, Subject};
use crate::plan::{self, MonitorAction, MonitorTick, SkipReason};
use crate::win::{self, InstanceGuard};
use anyhow::{Context, bail};
use rsrewind_capture::{
    ChangeDetection, ChangeDetector, Fingerprint, MonitorCapturer, MonitorHandle, VisibleWindow,
};
use rsrewind_core::{
    BgraFrame, CaptureState, Config, DataDir, EventKind, FocusContext, MonitorInfo, Timestamp,
};
use rsrewind_storage::{RecorderStatus, Store};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{TrySendError, sync_channel};
use std::time::{Duration, Instant};

pub struct RunOptions {
    pub data: DataDir,
    pub config: Config,
}

/// Re-enumerate displays this often even without errors, to notice hotplug.
const MONITOR_REFRESH: Duration = Duration::from_secs(30);
const HEARTBEAT: Duration = Duration::from_secs(5);
/// Persist queue depth. Each job may hold a full-resolution frame (~17 MB at 2.5K), so this also
/// bounds memory: about 4 frames in flight at worst.
const PERSIST_QUEUE: usize = 4;

/// Runs until `rsrewind stop`, Ctrl+C or a fatal storage error.
pub fn run(options: RunOptions) -> anyhow::Result<()> {
    let RunOptions { data, config } = options;
    if let Err(error) = rsrewind_capture::enable_dpi_awareness() {
        tracing::warn!(%error, "could not enable per-monitor DPI awareness; capture sizes may be scaled");
    }
    let Some(guard) = InstanceGuard::acquire().context("single-instance check")? else {
        bail!("rsRewind is already recording in this Windows session");
    };
    if let Err(error) = win::install_ctrl_handler() {
        tracing::debug!(%error, "no console control handler");
    }
    data.ensure()?;

    let control_store = Store::open(&data).context("open database")?;
    let persist_store = Store::open_existing(&data)?;
    let started_at = Timestamp::now();
    let hostname = std::env::var("COMPUTERNAME").unwrap_or_default();
    let session = persist_store.begin_session(started_at, &hostname, env!("CARGO_PKG_VERSION"))?;
    persist_store.record_marker(session, EventKind::RecorderStart, started_at, None)?;
    tracing::info!(%session, root = %data.root().display(), "recorder started");

    let counters = Arc::new(Counters::default());
    let stop = Arc::new(AtomicBool::new(false));
    let tick = Duration::from_secs_f32(1.0 / config.capture.fps_candidate);
    // A span may bridge up to three missed ticks (a slow encode, a dropped job) but never a pause.
    let max_gap_ms = (tick.as_millis() as i64).saturating_mul(3).max(1_500);

    let (ocr_wake_tx, ocr_thread) = if config.ocr.enabled {
        let (tx, rx) = sync_channel::<()>(1);
        let store = Store::open_existing(&data)?;
        let language = Some(config.ocr.language.clone()).filter(|l| !l.trim().is_empty());
        let (stop, counters) = (stop.clone(), counters.clone());
        let handle = std::thread::Builder::new()
            .name("rsrewind-ocr".into())
            .spawn(move || crate::ocr_worker::run(store, language, rx, stop, counters))?;
        (Some(tx), Some(handle))
    } else {
        (None, None)
    };

    let (jobs_tx, jobs_rx) = sync_channel::<Job>(PERSIST_QUEUE);
    let persist_thread = {
        let data = data.clone();
        let counters = counters.clone();
        let config = PersistConfig {
            session,
            image_quality: config.capture.image_quality,
            ocr_enabled: config.ocr.enabled,
            max_gap_ms,
            retention_days: config.storage.retention_days,
            max_bytes: (f64::from(config.storage.max_size_gb) * 1024.0 * 1024.0 * 1024.0) as u64,
        };
        std::thread::Builder::new()
            .name("rsrewind-persist".into())
            .spawn(move || {
                persist::run(persist_store, data, config, jobs_rx, ocr_wake_tx, counters)
            })?
    };

    let mut recorder = Recorder {
        config,
        counters: counters.clone(),
        jobs: jobs_tx,
        detector: ChangeDetector::default(),
        monitors: HashMap::new(),
        next_monitor_refresh: Instant::now(),
        last_reason: None,
        idle: false,
    };
    recorder.detector = ChangeDetector::new(recorder.config.capture.change_threshold);

    let mut next_heartbeat = Instant::now();
    while !guard.stop_requested() {
        let tick_started = Instant::now();
        let state = control_store.read_control().unwrap_or_else(|error| {
            tracing::warn!(%error, "could not read control state; treating as paused");
            CaptureState::Paused { until: None }
        });
        recorder.tick(state, Timestamp::now());

        if Instant::now() >= next_heartbeat {
            next_heartbeat = Instant::now() + HEARTBEAT;
            write_heartbeat(
                &control_store,
                started_at,
                state.effective_at(Timestamp::now()),
                &counters,
            );
        }
        let elapsed = tick_started.elapsed();
        sleep_unless_stopped(&guard, tick.saturating_sub(elapsed));
    }

    tracing::info!("recorder stopping");
    let stopped_at = Timestamp::now();
    let _ = recorder.jobs.send(Job::Marker {
        kind: EventKind::RecorderStop,
        at: stopped_at,
        metadata: None,
    });
    drop(recorder);
    stop.store(true, Ordering::Relaxed);
    if persist_thread.join().is_err() {
        tracing::error!("persist thread panicked");
    }
    if let Some(handle) = ocr_thread
        && handle.join().is_err()
    {
        tracing::error!("OCR thread panicked");
    }
    control_store.end_session(session, stopped_at)?;
    write_heartbeat(&control_store, started_at, CaptureState::Stopped, &counters);
    tracing::info!("recorder stopped");
    Ok(())
}

fn sleep_unless_stopped(guard: &InstanceGuard, mut remaining: Duration) {
    const SLICE: Duration = Duration::from_millis(100);
    while !remaining.is_zero() && !guard.stop_requested() {
        let step = remaining.min(SLICE);
        std::thread::sleep(step);
        remaining = remaining.saturating_sub(step);
    }
}

fn write_heartbeat(store: &Store, started_at: Timestamp, state: CaptureState, counters: &Counters) {
    let status = RecorderStatus {
        pid: std::process::id(),
        started_at,
        heartbeat_at: Timestamp::now(),
        state,
        counters: counters.snapshot(),
    };
    if let Err(error) = store.write_status(&status) {
        tracing::warn!(%error, "could not write heartbeat");
    }
}

struct MonitorSlot {
    handle: MonitorHandle,
    info: MonitorInfo,
    capturer: Option<MonitorCapturer>,
    /// Newest frame Windows gave us. WGC only delivers frames on change, so after a pause on a
    /// static screen this is the only copy of what is currently shown.
    last_frame: Option<BgraFrame>,
    /// Fingerprint of the last frame handed to the persist thread.
    persisted: Option<Fingerprint>,
}

struct Recorder {
    config: Config,
    counters: Arc<Counters>,
    jobs: std::sync::mpsc::SyncSender<Job>,
    detector: ChangeDetector,
    monitors: HashMap<String, MonitorSlot>,
    next_monitor_refresh: Instant,
    /// Last reason the whole recorder was not recording, for transition markers.
    last_reason: Option<SkipReason>,
    idle: bool,
}

impl Recorder {
    fn tick(&mut self, control: CaptureState, now: Timestamp) {
        counters::bump(&self.counters.ticks);
        self.refresh_monitors();

        let effective = control.effective_at(now);
        let idle_ms = rsrewind_capture::idle_millis();
        let idle = idle_ms > u64::from(self.config.capture.idle_after_secs) * 1_000;
        self.note_transitions(effective, idle, now);

        if !matches!(effective, CaptureState::Recording) {
            counters::bump(&self.counters.paused_ticks);
            // Drop everything that could leak into a span after resume.
            for slot in self.monitors.values_mut() {
                slot.persisted = None;
            }
            return;
        }

        let windows = rsrewind_capture::visible_windows();
        let focus = rsrewind_capture::foreground();
        let devices: Vec<String> = self.monitors.keys().cloned().collect();
        for device in devices {
            self.tick_monitor(&device, &windows, focus.as_ref(), idle, now);
        }
    }

    fn tick_monitor(
        &mut self,
        device: &str,
        windows: &[VisibleWindow],
        focus: Option<&FocusContext>,
        idle: bool,
        now: Timestamp,
    ) {
        let policy = &self.config.privacy;
        let Some(slot) = self.monitors.get_mut(device) else {
            return;
        };

        // Any visible window touching this monitor counts, not just the one it is mostly on: a
        // password manager straddling two screens must exclude both.
        let on_monitor: Vec<&VisibleWindow> = windows
            .iter()
            .filter(|w| w.rect.intersects(&slot.info))
            .collect();
        let contexts: Vec<_> = on_monitor
            .iter()
            .map(|w| (w.application_context(), w.window_context()))
            .collect();
        let mut privacy = plan::monitor_privacy(policy, contexts.iter().map(|(a, w)| (a, w)));
        // The foreground window is checked too, in case enumeration missed it (e.g. a UAC
        // prompt on the secure desktop, or a window that appeared between the two calls).
        if !privacy.is_excluded()
            && let Some(focus) = focus
        {
            let decision = policy.evaluate(Some(&focus.application), Some(&focus.window));
            if decision.is_excluded() && on_monitor.iter().any(|w| w.pid == focus.pid) {
                privacy = decision;
            }
        }

        // Pull the newest frame even when we will not store it, so the pool never stalls and
        // `last_frame` always reflects the screen.
        let mut has_new_frame = false;
        if slot.capturer.is_none() {
            match MonitorCapturer::new(slot.handle) {
                Ok(capturer) => {
                    slot.capturer = Some(capturer);
                    counters::bump(&self.counters.capturer_restarts);
                }
                Err(error) => {
                    counters::bump(&self.counters.capture_errors);
                    tracing::warn!(device, %error, "could not start capture");
                    return;
                }
            }
        }
        if let Some(capturer) = slot.capturer.as_mut() {
            match capturer.latest_frame() {
                Ok(Some(frame)) => {
                    counters::bump(&self.counters.candidate_frames);
                    slot.last_frame = Some(frame);
                    has_new_frame = true;
                }
                Ok(None) => {}
                Err(error) => {
                    counters::bump(&self.counters.capture_errors);
                    tracing::warn!(device, %error, recoverable = error.is_recoverable(), "capture failed; restarting capturer");
                    slot.capturer = None;
                    self.next_monitor_refresh = Instant::now();
                }
            }
        }

        // Privacy-excluded frames must not linger in memory as "the current screen" either.
        if privacy.is_excluded() {
            slot.last_frame = None;
        }

        let fingerprint = match (&slot.last_frame, has_new_frame || slot.persisted.is_none()) {
            (Some(frame), true) => Fingerprint::from_frame(frame).ok(),
            _ => None,
        };
        let changed = match (&fingerprint, &slot.persisted) {
            (Some(next), Some(prev)) => self.detector.is_meaningful(prev, next),
            (Some(_), None) => true,
            (None, _) => false,
        };
        let tick = MonitorTick {
            has_new_frame: fingerprint.is_some(),
            changed,
            has_current_state: slot.persisted.is_some(),
            privacy: privacy.clone(),
        };

        match plan::plan_monitor(CaptureState::Recording, now, idle, &tick) {
            MonitorAction::Persist => {
                let (Some(frame), Some(fingerprint)) = (slot.last_frame.clone(), fingerprint)
                else {
                    return;
                };
                let job = Job::NewState {
                    monitor: slot.info.clone(),
                    frame,
                    fingerprint: fingerprint.dhash(),
                    at: now,
                    subject: subject_for(&on_monitor, focus),
                };
                match self.jobs.try_send(job) {
                    Ok(()) => slot.persisted = Some(fingerprint),
                    Err(TrySendError::Full(_)) => {
                        // Not marking it persisted means the next tick tries again.
                        counters::bump(&self.counters.dropped_queue_full);
                    }
                    Err(TrySendError::Disconnected(_)) => {
                        counters::bump(&self.counters.persist_errors);
                    }
                }
            }
            MonitorAction::Extend => {
                if fingerprint.is_some() {
                    counters::bump(&self.counters.unchanged_frames);
                }
                let job = Job::Extend {
                    device_name: device.to_string(),
                    at: now,
                    subject: subject_for(&on_monitor, focus),
                };
                if self.jobs.try_send(job).is_err() {
                    counters::bump(&self.counters.dropped_queue_full);
                }
            }
            MonitorAction::Skip(reason) => {
                slot.persisted = None;
                match reason {
                    SkipReason::Privacy(rule) => {
                        counters::bump(&self.counters.privacy_skips);
                        tracing::debug!(device, rule, "privacy rule matched; nothing stored");
                    }
                    SkipReason::Idle => counters::bump(&self.counters.idle_ticks),
                    SkipReason::Paused | SkipReason::NoFrame => {}
                }
            }
        }
    }

    /// Records pause/resume/idle/privacy transitions once, not every tick.
    fn note_transitions(&mut self, effective: CaptureState, idle: bool, now: Timestamp) {
        let reason = match effective {
            CaptureState::Recording => None,
            _ => Some(SkipReason::Paused),
        };
        if reason != self.last_reason {
            let (kind, metadata) = match (&self.last_reason, &reason) {
                (None, Some(_)) => (
                    EventKind::Paused,
                    match effective {
                        CaptureState::Paused { until: Some(until) } => {
                            Some(serde_json::json!({ "until": until.as_millis() }))
                        }
                        _ => None,
                    },
                ),
                _ => (EventKind::Resumed, None),
            };
            tracing::info!(state = ?effective, "recording state changed");
            let _ = self.jobs.try_send(Job::Marker {
                kind,
                at: now,
                metadata,
            });
            self.last_reason = reason;
        }
        if idle != self.idle && matches!(effective, CaptureState::Recording) {
            let kind = if idle {
                EventKind::IdleStart
            } else {
                EventKind::IdleEnd
            };
            let _ = self.jobs.try_send(Job::Marker {
                kind,
                at: now,
                metadata: None,
            });
            self.idle = idle;
        }
    }

    fn refresh_monitors(&mut self) {
        if Instant::now() < self.next_monitor_refresh {
            return;
        }
        self.next_monitor_refresh = Instant::now() + MONITOR_REFRESH;
        let found = match rsrewind_capture::monitors() {
            Ok(found) => found,
            Err(error) => {
                counters::bump(&self.counters.capture_errors);
                tracing::warn!(%error, "could not enumerate monitors");
                return;
            }
        };
        let mut seen = Vec::with_capacity(found.len());
        for (handle, info) in found {
            seen.push(info.device_name.clone());
            match self.monitors.get_mut(&info.device_name) {
                // Same display, new geometry or handle (resolution change, sleep/wake): restart.
                Some(slot) if slot.info != info || slot.handle != handle => {
                    tracing::info!(device = %info.device_name, "display changed; restarting capture");
                    slot.info = info;
                    slot.handle = handle;
                    slot.capturer = None;
                    slot.last_frame = None;
                    slot.persisted = None;
                }
                Some(_) => {}
                None => {
                    tracing::info!(device = %info.device_name, width = info.width, height = info.height, "display found");
                    self.monitors.insert(
                        info.device_name.clone(),
                        MonitorSlot {
                            handle,
                            info,
                            capturer: None,
                            last_frame: None,
                            persisted: None,
                        },
                    );
                }
            }
        }
        self.monitors.retain(|device, _| {
            let keep = seen.contains(device);
            if !keep {
                tracing::info!(device, "display removed");
            }
            keep
        });
    }
}

/// The application to credit for a monitor's picture: the foreground window if it is on this
/// monitor, otherwise the topmost visible window on it (EnumWindows is in z-order).
fn subject_for(on_monitor: &[&VisibleWindow], focus: Option<&FocusContext>) -> Option<Subject> {
    if let Some(focus) = focus
        && on_monitor.iter().any(|w| w.pid == focus.pid)
    {
        return Some(Subject {
            application: focus.application.clone(),
            window: focus.window.clone(),
        });
    }
    on_monitor.first().map(|w| Subject {
        application: w.application_context(),
        window: w.window_context(),
    })
}
