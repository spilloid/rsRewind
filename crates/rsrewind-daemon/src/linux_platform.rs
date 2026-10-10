//! The Linux side of the platform seam, for KDE Plasma on Wayland: thin adapters from
//! [`crate::platform`] to `rsrewind_capture::kwin` (screens, windows, focus, screenshots) and
//! `rsrewind_capture::wayland_idle` (idle time). No OCR engine yet: captured states stay
//! `pending` and become searchable once one exists (or on a central instance that has one).
//!
//! Stop is SIGTERM/SIGINT/SIGHUP (`rsrewind stop` sends SIGTERM to the pid in the recorder's
//! heartbeat); one recorder per data folder is an advisory lock on `<data>/recorder.lock`.

use crate::platform::{
    CaptureBackend, DisplayHandle, FrameSource, IdleClock, Lifecycle, OcrBackend, OcrFactory,
    Platform, PlatformFault, Recognition, ScreenContext, SingleInstance, SystemClock,
};
use crate::recorder::{RunOptions, run_with};
use anyhow::{Context, bail};
use rsrewind_capture::VisibleWindow;
use rsrewind_capture::kwin::{Kwin, KwinError, Snapshot, process_name};
use rsrewind_capture::wayland_idle::WaylandIdle;
use rsrewind_core::{BgraFrame, Capabilities, DataDir, FocusContext, MonitorInfo};
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// What the Plasma recorder sees: KWin lists every window with its geometry, title and pid.
pub const PLASMA: Capabilities = Capabilities {
    list_windows: true,
    window_titles: true,
    process_names: true,
    multi_monitor: true,
    ocr: false,
};

/// Runs until SIGTERM/SIGINT or a fatal storage error.
pub fn run(options: RunOptions) -> anyhow::Result<()> {
    let Some(guard) = LockFile(options.data.clone()).acquire()? else {
        bail!(
            "rsRewind is already recording into {}",
            options.data.root().display()
        );
    };
    let kwin = Kwin::connect(&runtime_dir()).map_err(|e| anyhow::anyhow!(e.message))?;
    if !kwin.available() {
        bail!("KWin is not on the session bus; the Linux recorder needs KDE Plasma on Wayland");
    }
    let idle = match WaylandIdle::start() {
        Ok(idle) => Some(idle),
        Err(error) => {
            // Unknown idle time means nothing is stored; say so loudly rather than quietly.
            tracing::warn!(%error, "no idle time from the compositor; nothing will be stored");
            None
        }
    };
    let shared = Rc::new(RefCell::new(PlasmaDesktop {
        kwin,
        snapshot: None,
        handles: HashMap::new(),
    }));
    let platform = Platform {
        context: Box::new(PlasmaContext(shared.clone())),
        capture: Box::new(PlasmaCapture(shared.clone())),
        idle: Box::new(PlasmaIdle {
            idle,
            desktop: shared,
        }),
        clock: Arc::new(SystemClock::new()),
        lifecycle: Box::new(guard),
        ocr: Some(ocrs_factory(options.data.models())),
        hostname: hostname(),
    };
    run_with(options, platform)
}

/// Text recognition with `ocrs`, loaded on the OCR thread from `<data>/models`. If the models are
/// not there, the factory fails, moments stay `pending`, and `doctor` says where to put them.
fn ocrs_factory(models: PathBuf) -> OcrFactory {
    Box::new(move || {
        rsrewind_ocr::OcrsEngine::load(&models)
            .map(|engine| Box::new(Ocrs(engine)) as Box<dyn OcrBackend>)
    })
}

struct Ocrs(rsrewind_ocr::OcrsEngine);

impl OcrBackend for Ocrs {
    fn engine_name(&self) -> &'static str {
        rsrewind_ocr::ENGINE_NAME
    }

    fn recognize(&mut self, frame: &BgraFrame) -> Result<Recognition, String> {
        self.0.recognize(frame).map(|out| Recognition {
            blocks: out.blocks,
            elapsed_ms: out.elapsed_ms,
        })
    }
}

/// Where the KWin script file lives: `$XDG_RUNTIME_DIR/rsrewind`, else the system temp folder.
fn runtime_dir() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(std::env::temp_dir)
        .join("rsrewind")
}

fn hostname() -> String {
    std::fs::read_to_string("/proc/sys/kernel/hostname")
        .map(|h| h.trim().to_string())
        .unwrap_or_default()
}

fn fault(error: KwinError) -> PlatformFault {
    PlatformFault::new(error.message, error.recoverable)
}

/// The KWin connection and the latest snapshot, shared by the context, capture and idle adapters
/// (all on the recorder thread).
struct PlasmaDesktop {
    kwin: Kwin,
    /// Taken by `visible_windows` each tick and reused by `foreground` in the same tick.
    snapshot: Option<Snapshot>,
    /// Stable handle per screen name.
    handles: HashMap<String, DisplayHandle>,
}

impl PlasmaDesktop {
    fn handle(&mut self, name: &str) -> DisplayHandle {
        let next = DisplayHandle(self.handles.len() as u64 + 1);
        *self.handles.entry(name.to_string()).or_insert(next)
    }

    fn name_of(&self, handle: DisplayHandle) -> Option<String> {
        self.handles
            .iter()
            .find(|(_, h)| **h == handle)
            .map(|(name, _)| name.clone())
    }
}

struct PlasmaContext(Rc<RefCell<PlasmaDesktop>>);

impl ScreenContext for PlasmaContext {
    fn capabilities(&self) -> Capabilities {
        PLASMA
    }

    fn monitors(&mut self) -> Result<Vec<(DisplayHandle, MonitorInfo)>, PlatformFault> {
        let mut desktop = self.0.borrow_mut();
        let snapshot = desktop.kwin.snapshot().map_err(fault)?;
        let monitors = snapshot.monitors();
        Ok(monitors
            .into_iter()
            .map(|info| (desktop.handle(&info.device_name), info))
            .collect())
    }

    fn foreground(&mut self) -> Option<FocusContext> {
        let desktop = self.0.borrow();
        // The recorder asks for windows first in the same tick; with no fresh snapshot (it failed)
        // there is no focus either, and the window error already excluded every monitor.
        desktop
            .snapshot
            .as_ref()
            .and_then(|s| s.foreground(&mut process_name))
    }

    fn visible_windows(&mut self) -> Result<Vec<VisibleWindow>, PlatformFault> {
        let mut desktop = self.0.borrow_mut();
        desktop.snapshot = None;
        let snapshot = desktop.kwin.snapshot().map_err(fault)?;
        let windows = snapshot.visible_windows(&mut process_name);
        desktop.snapshot = Some(snapshot);
        Ok(windows)
    }
}

struct PlasmaCapture(Rc<RefCell<PlasmaDesktop>>);

impl CaptureBackend for PlasmaCapture {
    fn open(&mut self, display: DisplayHandle) -> Result<Box<dyn FrameSource>, PlatformFault> {
        let name = self
            .0
            .borrow()
            .name_of(display)
            .ok_or_else(|| PlatformFault::new("unknown display", true))?;
        Ok(Box::new(PlasmaFrames {
            desktop: self.0.clone(),
            name,
        }))
    }
}

/// KWin captures on request, so every call returns a fresh frame; change detection decides what
/// is stored.
struct PlasmaFrames {
    desktop: Rc<RefCell<PlasmaDesktop>>,
    name: String,
}

impl FrameSource for PlasmaFrames {
    fn latest_frame(&mut self) -> Result<Option<BgraFrame>, PlatformFault> {
        let desktop = self.desktop.borrow();
        // A screen that left the latest snapshot is not captured (it may be gone).
        if let Some(snapshot) = &desktop.snapshot
            && !snapshot.screens.iter().any(|s| s.name == self.name)
        {
            return Err(PlatformFault::new("display left the desktop", true));
        }
        desktop
            .kwin
            .capture_screen(&self.name)
            .map(Some)
            .map_err(fault)
    }
}

/// Idle time from the compositor; a locked screen counts as idle (the lock screen is not history).
struct PlasmaIdle {
    idle: Option<WaylandIdle>,
    desktop: Rc<RefCell<PlasmaDesktop>>,
}

impl IdleClock for PlasmaIdle {
    fn idle_millis(&mut self) -> Option<u64> {
        match self.desktop.borrow().kwin.screen_locked() {
            Some(false) => {}
            Some(true) => return Some(u64::MAX),
            None => return None,
        }
        self.idle.as_ref()?.idle_millis()
    }
}

/// `<data>/recorder.lock`, held (advisory `flock`) for the life of the recorder.
pub struct LockFile(pub DataDir);

pub struct LockGuard {
    _file: std::fs::File,
    stop: Arc<AtomicBool>,
}

impl SingleInstance for LockFile {
    type Guard = LockGuard;

    fn acquire(&self) -> anyhow::Result<Option<LockGuard>> {
        let path = lock_path(self.0.root());
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)
            .with_context(|| format!("open {}", path.display()))?;
        match file.try_lock() {
            Ok(()) => {}
            Err(std::fs::TryLockError::WouldBlock) => return Ok(None),
            Err(std::fs::TryLockError::Error(error)) => {
                return Err(error).with_context(|| format!("lock {}", path.display()));
            }
        }
        let stop = Arc::new(AtomicBool::new(false));
        for signal in [
            signal_hook::consts::SIGTERM,
            signal_hook::consts::SIGINT,
            signal_hook::consts::SIGHUP,
        ] {
            signal_hook::flag::register(signal, stop.clone())
                .with_context(|| format!("handle signal {signal}"))?;
        }
        Ok(Some(LockGuard { _file: file, stop }))
    }
}

fn lock_path(root: &Path) -> PathBuf {
    root.join("recorder.lock")
}

impl Lifecycle for LockGuard {
    fn stop_requested(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod lock_tests {
    use super::*;

    #[test]
    fn one_recorder_per_data_folder() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let data = DataDir::new(dir.path());
        let first = LockFile(data.clone()).acquire()?;
        assert!(first.is_some());
        assert!(LockFile(data.clone()).acquire()?.is_none());
        drop(first);
        assert!(LockFile(data).acquire()?.is_some());
        Ok(())
    }
}
