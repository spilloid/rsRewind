//! The persist thread: the only writer of visual states and observations.

use crate::counters::{self, Counters};
use rsrewind_core::{
    ApplicationContext, ApplicationId, BgraFrame, DataDir, EventId, EventKind, MonitorId,
    MonitorInfo, SessionId, Timestamp, VisualStateId, WindowContext, WindowId,
    paths::media_relative_path,
};
use rsrewind_storage::{NewVisualState, Observation, StorageError, Store, media};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
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

/// What the capture loop and the persist thread share besides the job queue.
#[derive(Debug, Default)]
pub(crate) struct Handoff {
    /// Monitors whose screen the persist thread was handed but does not hold as a saved, current
    /// picture: a new picture that failed to save (or fell inside forgotten time), or an extension
    /// of a picture that is gone. The capture loop takes them and offers those screens again. Only
    /// the capture loop's [`Handoff::take_unsaved`] or a later saved picture *of the same monitor*
    /// clears an entry: a write for another monitor cannot cancel it (astra review, 2026-10-19).
    unsaved: Mutex<HashSet<String>>,
    /// Jobs the capture loop queued and jobs the persist thread finished, so a test can wait until
    /// everything queued so far has been handled.
    queued: AtomicU64,
    handled: AtomicU64,
}

impl Handoff {
    fn unsaved(&self) -> std::sync::MutexGuard<'_, HashSet<String>> {
        // A panic while holding this lock leaves a plain set behind, still valid.
        self.unsaved.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn mark_unsaved(&self, device: &str) {
        self.unsaved().insert(device.to_owned());
    }

    fn mark_saved(&self, device: &str) {
        self.unsaved().remove(device);
    }

    /// The monitors to offer again, removed from the set.
    pub(crate) fn take_unsaved(&self) -> HashSet<String> {
        std::mem::take(&mut *self.unsaved())
    }

    /// Called by the capture loop for each job it queued.
    pub(crate) fn queued(&self) {
        self.queued.fetch_add(1, Ordering::SeqCst);
    }

    /// True when every job queued so far has been handled and its effects are visible.
    #[cfg(test)]
    pub(crate) fn settled(&self) -> bool {
        self.handled.load(Ordering::SeqCst) == self.queued.load(Ordering::SeqCst)
    }
}

pub fn run(
    store: Store,
    data: DataDir,
    config: PersistConfig,
    jobs: Receiver<Job>,
    ocr_wake: Option<SyncSender<()>>,
    counters: Arc<Counters>,
    handoff: Arc<Handoff>,
) {
    let mut persister = Persister::new(store, data, config, ocr_wake, counters, handoff);
    // `recv` returns Err only when the capture loop dropped the sender: that is shutdown.
    while let Ok(job) = jobs.recv() {
        persister.step(job);
    }
    persister.finish();
}

/// The persist thread's loop body, one job at a time (tests drive it directly).
pub(crate) struct Persister {
    state: PersistState,
    ocr_wake: Option<SyncSender<()>>,
    next_retention: Instant,
    /// Failed jobs since anything was last written, published as the `persist_failing` gauge; the
    /// capture loop turns it into "nothing saved: saving to disk is failing" (a full disk, an
    /// unwritable folder). Jobs that had nothing to write (an Extend with no current picture) leave
    /// it as it is: they are no evidence that writing works. It only drives health; which screens
    /// to offer again is [`Handoff`]'s per-monitor set.
    streak: u64,
}

impl Persister {
    pub(crate) fn new(
        store: Store,
        data: DataDir,
        config: PersistConfig,
        ocr_wake: Option<SyncSender<()>>,
        counters: Arc<Counters>,
        handoff: Arc<Handoff>,
    ) -> Self {
        Self {
            state: PersistState {
                store,
                data,
                config,
                counters,
                handoff,
                monitors: HashMap::new(),
                current: HashMap::new(),
                applications: HashMap::new(),
                windows: HashMap::new(),
                leftovers: Vec::new(),
                remove_file: |path| std::fs::remove_file(path),
            },
            ocr_wake,
            next_retention: Instant::now() + Duration::from_secs(60),
            streak: 0,
        }
    }

    /// Replaces how files are deleted, so a test can make deletion fail like a file another
    /// program holds open on Windows.
    #[cfg(test)]
    pub(crate) fn set_remove_file(&mut self, remove: fn(&Path) -> std::io::Result<()>) {
        self.state.remove_file = remove;
    }

    /// Handles one job and publishes its effects.
    pub(crate) fn step(&mut self, job: Job) {
        let Self {
            state,
            ocr_wake,
            next_retention,
            streak,
        } = self;
        // Files that could not be deleted after earlier jobs: try again first.
        state.retry_leftovers();
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
                if *streak == 0 {
                    tracing::warn!(%error, "persist failed");
                } else {
                    tracing::debug!(%error, streak = *streak, "persist failed (still)");
                }
                *streak += 1;
                false
            }
        };
        if wrote && *streak > 0 {
            tracing::info!(failed_jobs = *streak, "persisting works again");
            *streak = 0;
        }
        state
            .counters
            .persist_failing
            .store(*streak, Ordering::Relaxed);
        if wrote
            && woke_ocr
            && let Some(wake) = ocr_wake.as_ref()
        {
            // Lossy on purpose: the OCR worker reads its backlog from SQLite.
            if let Err(TrySendError::Disconnected(())) = wake.try_send(()) {
                tracing::debug!("OCR worker is gone");
            }
        }
        if Instant::now() >= *next_retention {
            *next_retention = Instant::now() + Duration::from_secs(3_600);
            state.retention();
        }
        // Last: everything this job changed is visible before it counts as handled.
        state.handoff.handled.fetch_add(1, Ordering::SeqCst);
    }

    /// At shutdown: one last attempt at the files that could not be deleted.
    pub(crate) fn finish(&mut self) {
        self.state.retry_leftovers();
        for path in &self.state.leftovers {
            tracing::warn!(
                path = %path.display(),
                "an unsaved screenshot is still on disk; `rsrewind doctor` lists it, and it is \
                 deleted when retention reaches its time or another forget covers it"
            );
        }
    }
}

struct PersistState {
    store: Store,
    data: DataDir,
    config: PersistConfig,
    counters: Arc<Counters>,
    handoff: Arc<Handoff>,
    monitors: HashMap<String, MonitorId>,
    /// device name -> the visual state now on that monitor (as far as storage knows).
    current: HashMap<String, VisualStateId>,
    applications: HashMap<String, ApplicationId>,
    windows: HashMap<(ApplicationId, String, Option<String>), WindowId>,
    /// Screenshot files no row will ever point at that could not be deleted yet (another program
    /// had them open). Retried after every job.
    leftovers: Vec<PathBuf>,
    remove_file: fn(&Path) -> std::io::Result<()>,
}

impl PersistState {
    /// Applies one job. `Ok(true)` if something was written, `Ok(false)` if there was nothing to
    /// write (an Extend of a picture that is no longer current).
    ///
    /// Whenever a monitor ends up with no saved current picture while the capture loop believes
    /// one was saved (a new picture failed or was fenced, an extension found its picture gone),
    /// the monitor is marked in [`Handoff`] so its screen is offered again on the next tick.
    fn handle(&mut self, job: Job) -> Result<bool, StorageError> {
        match job {
            Job::NewState {
                monitor,
                frame,
                fingerprint,
                at,
                subject,
            } => {
                let device = monitor.device_name.clone();
                let result = self.new_state(monitor, &frame, fingerprint, at, subject.as_ref());
                match &result {
                    Ok(_) => self.handoff.mark_saved(&device),
                    Err(_) => self.handoff.mark_unsaved(&device),
                }
                result
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
                    // The picture the capture loop is extending was never saved, or is gone.
                    self.handoff.mark_unsaved(&device_name);
                    return Ok(false);
                };
                match self.observe(monitor, visual_state, subject.as_ref(), at) {
                    Ok(_) => {
                        counters::bump(&self.counters.extended_observations);
                        Ok(true)
                    }
                    // Deleted under us (forget / retention): stop extending it;
                    // the screen still on display is offered again as a new picture.
                    Err(StorageError::VisualStateMissing(_)) => {
                        self.current.remove(&device_name);
                        self.handoff.mark_unsaved(&device_name);
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

    fn new_state(
        &mut self,
        monitor: MonitorInfo,
        frame: &BgraFrame,
        fingerprint: u64,
        at: Timestamp,
        subject: Option<&Subject>,
    ) -> Result<bool, StorageError> {
        // Forget the old state first: if anything below fails, later Extend jobs must not
        // stretch the previous picture over a moment it no longer showed.
        self.current.remove(&monitor.device_name);
        let monitor_id = self.monitor_id(&monitor, at)?;
        let relative = media_relative_path(at, monitor_id);
        let bytes = media::encode_webp(frame, self.config.image_quality)?;
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
                // No row will ever point at the file (e.g. `Fenced`: the moment was forgotten
                // while this frame was queued). Do not leave it behind.
                self.discard(written);
                return Err(error);
            }
        };
        self.observe(monitor_id, visual_state, subject, at)?;
        self.current.insert(monitor.device_name, visual_state);
        counters::bump(&self.counters.persisted_states);
        counters::add(&self.counters.storage_bytes_written, bytes.len() as u64);
        Ok(true)
    }

    /// Deletes a screenshot no row will point at. A failed delete (on Windows: another program
    /// has the file open) is not silent: it counts as a persist error, is logged, and is retried
    /// after every later job until it succeeds.
    fn discard(&mut self, path: PathBuf) {
        match (self.remove_file)(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                counters::bump(&self.counters.persist_errors);
                tracing::warn!(
                    path = %path.display(),
                    %error,
                    "could not delete an unsaved screenshot; will retry"
                );
                self.leftovers.push(path);
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

    /// Tries again to delete files that could not be deleted before.
    fn retry_leftovers(&mut self) {
        if self.leftovers.is_empty() {
            return;
        }
        let remove = self.remove_file;
        self.leftovers.retain(|path| match remove(path) {
            Ok(()) => {
                tracing::info!(path = %path.display(), "removed an unsaved screenshot");
                false
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
            Err(_) => true,
        });
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
