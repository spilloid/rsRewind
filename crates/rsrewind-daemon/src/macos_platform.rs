//! Native macOS adapters for the portable recorder, storage and OCR pipeline.
use crate::platform::{
    CaptureBackend, DisplayHandle, FrameSource, IdleClock, Lifecycle, OcrBackend, Platform,
    PlatformFault, Recognition, ScreenContext, SingleInstance, SystemClock,
};
use crate::recorder::{RunOptions, run_with};
use anyhow::{Context, bail};
use rsrewind_capture::{VisibleWindow, macos};
use rsrewind_core::{BgraFrame, Capabilities, DataDir, FocusContext, MonitorInfo};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

pub const MACOS: Capabilities = Capabilities {
    list_windows: true,
    window_titles: true,
    process_names: true,
    multi_monitor: true,
    ocr: true,
};

pub fn run(options: RunOptions) -> anyhow::Result<()> {
    let Some(guard) = LockFile(options.data.clone()).acquire()? else {
        bail!(
            "rsRewind is already recording into {}",
            options.data.root().display()
        );
    };
    if !macos::screen_recording_permission(false) {
        bail!(
            "Screen Recording permission required: open System Settings > Privacy & Security > Screen Recording, authorize rsRewind, then restart it"
        );
    }
    let language = options.config.ocr.language.clone();
    run_with(
        options,
        Platform {
            context: Box::new(MacContext),
            capture: Box::new(MacCapture),
            idle: Box::new(MacIdle),
            clock: Arc::new(SystemClock::new()),
            lifecycle: Box::new(guard),
            ocr: Some(Box::new(move || {
                rsrewind_ocr::VisionEngine::new(Some(&language))
                    .map(|e| Box::new(Vision(e)) as Box<dyn OcrBackend>)
            })),
            hostname: std::env::var("HOSTNAME").unwrap_or_default(),
        },
    )
}
struct MacContext;
fn fault(message: String) -> PlatformFault {
    PlatformFault::new(message, true)
}
impl ScreenContext for MacContext {
    fn capabilities(&self) -> Capabilities {
        MACOS
    }
    fn monitors(&mut self) -> Result<Vec<(DisplayHandle, MonitorInfo)>, PlatformFault> {
        macos::monitors()
            .map(|ds| {
                ds.into_iter()
                    .map(|d| (DisplayHandle(u64::from(d.id)), d.info))
                    .collect()
            })
            .map_err(fault)
    }
    // CoreGraphics ordering does not prove the focused window. Do not misattribute foreground.
    fn foreground(&mut self) -> Option<FocusContext> {
        None
    }
    fn visible_windows(&mut self) -> Result<Vec<VisibleWindow>, PlatformFault> {
        macos::visible_windows().map_err(fault)
    }
}
struct MacCapture;
impl CaptureBackend for MacCapture {
    fn open(&mut self, display: DisplayHandle) -> Result<Box<dyn FrameSource>, PlatformFault> {
        let id = u32::try_from(display.0)
            .map_err(|_| PlatformFault::new("invalid macOS display", false))?;
        Ok(Box::new(MacFrames(id)))
    }
}
struct MacFrames(u32);
impl FrameSource for MacFrames {
    fn latest_frame(&mut self) -> Result<Option<BgraFrame>, PlatformFault> {
        macos::capture(self.0).map(Some).map_err(fault)
    }
}
struct MacIdle;
impl IdleClock for MacIdle {
    fn idle_millis(&mut self) -> Option<u64> {
        macos::idle_millis()
    }
}
struct Vision(rsrewind_ocr::VisionEngine);
impl OcrBackend for Vision {
    fn engine_name(&self) -> &'static str {
        rsrewind_ocr::ENGINE_NAME
    }
    fn recognize(&mut self, frame: &BgraFrame) -> Result<Recognition, String> {
        self.0.recognize(frame).map(|o| Recognition {
            blocks: o.blocks,
            elapsed_ms: o.elapsed_ms,
        })
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
