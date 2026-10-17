//! The persist thread: the only writer of visual states and observations.

use crate::counters::{self, Counters};
use rsrewind_core::{
    ApplicationContext, ApplicationId, BgraFrame, DataDir, EventId, EventKind, MonitorId,
    MonitorInfo, SessionId, Timestamp, VisualStateId, WindowContext, WindowId,
    paths::media_relative_path,
};
use rsrewind_storage::{NewVisualState, Observation, StorageError, Store, media};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{Receiver, SyncSender, TrySendError};
use std::time::{Duration, Instant};

/// Who/what was in front of a monitor at the time of an observation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Subject {
    pub application: ApplicationContext,
    pub window: WindowContext,
}

#[derive(Debug)]
pub enum Job {
    NewState {
        monitor: MonitorInfo,
        frame: BgraFrame,
        fingerprint: u64,
        at: Timestamp,
        subject: Option<Subject>,
    },
    Extend {
        device_name: String,
        at: Timestamp,
        subject: Option<Subject>,
    },
    Marker {
        kind: EventKind,
        at: Timestamp,
        metadata: Option<serde_json::Value>,
    },
}

pub struct PersistConfig {
    pub session: SessionId,
    pub image_quality: u8,
    pub ocr_enabled: bool,
    pub max_gap_ms: i64,
    pub retention_days: u32,
    pub max_bytes: u64,
}

pub fn run(
    store: Store,
    data: DataDir,
    config: PersistConfig,
    jobs: Receiver<Job>,
    ocr_wake: Option<SyncSender<()>>,
    counters: Arc<Counters>,
) {
    let mut state = PersistState {
        store,
        data,
        config,
        counters,
        monitors: HashMap::new(),
        current: HashMap::new(),
        applications: HashMap::new(),
        windows: HashMap::new(),
    };
    let mut next_retention = Instant::now() + Duration::from_secs(60);
    // Failed jobs since anything was last written, published as the `persist_failing` gauge; the
    // capture loop turns it into "nothing saved: saving to disk is failing" (a full disk, an
    // unwritable folder). Jobs that had nothing to write (an Extend with no current picture) leave
    // it as it is: they are no evidence that writing works.
    let mut streak: u64 = 0;

    // `recv` returns Err only when the capture loop dropped the sender: that is shutdown.
    while let Ok(job) = jobs.recv() {
        let woke_ocr = matches!(job, Job::NewState { .. });
        let wrote = match state.handle(job) {
            Ok(wrote) => wrote,
            // Expected after `forget`: a frame queued before its fence committed. Not a fault.
            Err(StorageError::Fenced { .. }) => {
                tracing::debug!("a queued moment fell inside forgotten time; dropped");
                false
            }
            Err(error) => {
                counters::bump(&state.counters.persist_errors);
                // Once per streak at warn: a full disk fails every job, every second.
                if streak == 0 {
                    tracing::warn!(%error, "persist failed");
                } else {
                    tracing::debug!(%error, streak, "persist failed (still)");
                }
                streak += 1;
                false
            }
        };
        if wrote && streak > 0 {
            tracing::info!(failed_jobs = streak, "persisting works again");
            streak = 0;
        }
        state
            .counters
            .persist_failing
            .store(streak, Ordering::Relaxed);
        if wrote
            && woke_ocr
            && let Some(wake) = &ocr_wake
        {
            // Lossy on purpose: the OCR worker reads its backlog from SQLite.
            if let Err(TrySendError::Disconnected(())) = wake.try_send(()) {
                tracing::debug!("OCR worker is gone");
            }
        }
        if Instant::now() >= next_retention {
            next_retention = Instant::now() + Duration::from_secs(3_600);
            state.retention();
        }
    }
}

struct PersistState {
    store: Store,
    data: DataDir,
    config: PersistConfig,
    counters: Arc<Counters>,
    monitors: HashMap<String, MonitorId>,
    /// device name -> the visual state now on that monitor (as far as storage knows).
    current: HashMap<String, VisualStateId>,
    applications: HashMap<String, ApplicationId>,
    windows: HashMap<(ApplicationId, String, Option<String>), WindowId>,
}

impl PersistState {
    /// Applies one job. `Ok(true)` if something was written, `Ok(false)` if there was nothing to
    /// write (an Extend of a picture that is no longer current).
    fn handle(&mut self, job: Job) -> Result<bool, StorageError> {
        match job {
            Job::NewState {
                monitor,
                frame,
                fingerprint,
                at,
                subject,
            } => {
                // Forget the old state first: if anything below fails, later Extend jobs must
                // not stretch the previous picture over a moment it no longer showed.
                self.current.remove(&monitor.device_name);
                let monitor_id = self.monitor_id(&monitor, at)?;
                let relative = media_relative_path(at, monitor_id);
                let bytes = media::encode_webp(&frame, self.config.image_quality)?;
                let written = media::write_webp_exclusive(&self.data, &relative, &bytes)?;
                let visual_state = match self.store.insert_visual_state(&NewVisualState {
                    monitor: monitor_id,
                    captured_at: at,
                    media_path: relative,
                    width: frame.width,
                    height: frame.height,
                    byte_size: bytes.len() as u64,
                    fingerprint: Some(fingerprint),
                    ocr_enabled: self.config.ocr_enabled,
                }) {
                    Ok(id) => id,
                    Err(error) => {
                        // No row will ever point at the file (e.g. `Fenced`: the moment was
                        // forgotten while this frame was queued). Do not leave it behind.
                        let _ = std::fs::remove_file(&written);
                        return Err(error);
                    }
                };
                self.observe(monitor_id, visual_state, subject.as_ref(), at)?;
                self.current.insert(monitor.device_name, visual_state);
                counters::bump(&self.counters.persisted_states);
                counters::add(&self.counters.storage_bytes_written, bytes.len() as u64);
                Ok(true)
            }
            Job::Extend {
                device_name,
                at,
                subject,
            } => {
                let (Some(&visual_state), Some(&monitor)) = (
                    self.current.get(&device_name),
                    self.monitors.get(&device_name),
                ) else {
                    return Ok(false);
                };
                match self.observe(monitor, visual_state, subject.as_ref(), at) {
                    Ok(_) => {
                        counters::bump(&self.counters.extended_observations);
                        Ok(true)
                    }
                    // Deleted under us (delete-recent / retention): stop extending it. The
                    // capture loop will persist a fresh state on the next change.
                    Err(StorageError::VisualStateMissing(_)) => {
                        self.current.remove(&device_name);
                        Ok(false)
                    }
                    Err(error) => Err(error),
                }
            }
            Job::Marker { kind, at, metadata } => {
                self.store
                    .record_marker(self.config.session, kind, at, metadata)?;
                if matches!(
                    kind,
                    EventKind::Paused | EventKind::PrivacySkip | EventKind::IdleStart
                ) {
                    // A gap in recording ends every open span.
                    self.current.clear();
                }
                Ok(true)
            }
        }
    }

    /// Records that `monitor` showed `visual_state` at `at`. `forget` and retention delete the
    /// application and window rows nothing references any more, which after "forget the last 10
    /// minutes" are usually the ones cached here for the app still in front; storage then answers
    /// `ContextMissing`, and the caches are dropped and the rows looked up (re-created) once more.
    /// Without this, every later moment of that app failed until the recorder restarted
    /// (road-to-1.0 audit, 2026-10-16).
    fn observe(
        &mut self,
        monitor: MonitorId,
        visual_state: VisualStateId,
        subject: Option<&Subject>,
        at: Timestamp,
    ) -> Result<EventId, StorageError> {
        let mut retried = false;
        loop {
            let (application, window) = self.subject_ids(subject, at)?;
            match self.store.record_observation(&Observation {
                session: self.config.session,
                monitor,
                visual_state,
                application,
                window,
                at,
                max_gap_ms: self.config.max_gap_ms,
            }) {
                Err(StorageError::ContextMissing) if !retried => {
                    tracing::debug!(
                        "a cached application or window was deleted; looking it up again"
                    );
                    self.applications.clear();
                    self.windows.clear();
                    retried = true;
                }
                other => return other,
            }
        }
    }

    fn monitor_id(
        &mut self,
        monitor: &MonitorInfo,
        at: Timestamp,
    ) -> Result<MonitorId, StorageError> {
        if let Some(&id) = self.monitors.get(&monitor.device_name) {
            return Ok(id);
        }
        let id = self.store.upsert_monitor(monitor, at)?;
        self.monitors.insert(monitor.device_name.clone(), id);
        Ok(id)
    }

    fn subject_ids(
        &mut self,
        subject: Option<&Subject>,
        at: Timestamp,
    ) -> Result<(Option<ApplicationId>, Option<WindowId>), StorageError> {
        let Some(subject) = subject else {
            return Ok((None, None));
        };
        let app_key = subject.application.process_name.to_lowercase();
        let application = match self.applications.get(&app_key) {
            Some(&id) => id,
            None => {
                let id = self.store.upsert_application(&subject.application, at)?;
                self.applications.insert(app_key, id);
                id
            }
        };
        let window_key = (
            application,
            subject.window.title.clone(),
            subject.window.class_name.clone(),
        );
        let window = match self.windows.get(&window_key) {
            Some(&id) => id,
            None => {
                let id = self.store.upsert_window(application, &subject.window, at)?;
                // Titles churn (every browser tab, every document); keep the cache bounded.
                if self.windows.len() > 4_096 {
                    self.windows.clear();
                }
                self.windows.insert(window_key, id);
                id
            }
        };
        Ok((Some(application), Some(window)))
    }

    fn retention(&mut self) {
        counters::bump(&self.counters.retention_runs);
        match self.store.apply_retention(
            self.config.retention_days,
            self.config.max_bytes,
            Timestamp::now(),
        ) {
            Ok(report) => {
                if report.visual_states_deleted > 0 || report.events_deleted > 0 {
                    tracing::info!(
                        events = report.events_deleted,
                        states = report.visual_states_deleted,
                        bytes = report.bytes_freed,
                        file_errors = report.file_errors.len(),
                        "retention pruned history"
                    );
                    // States may have been removed from under the open spans.
                    self.current.clear();
                }
            }
            Err(error) => tracing::warn!(%error, "retention failed"),
        }
    }
}
