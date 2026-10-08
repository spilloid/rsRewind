//! The Windows side of the platform seam: thin adapters from [`crate::platform`] to the
//! Windows implementations in `rsrewind-capture` (WGC, EnumWindows, `GetLastInputInfo`),
//! `rsrewind-ocr` (Windows.Media.Ocr) and [`crate::win`] (named mutex, stop event, console
//! handler). No logic lives here; behaviour is whatever those modules already did.

use crate::platform::{
    CaptureBackend, DisplayHandle, FrameSource, IdleClock, Lifecycle, OcrBackend, OcrFactory,
    Platform, PlatformFault, Recognition, ScreenContext, SingleInstance, SystemClock,
};
use crate::recorder::{RunOptions, run_with};
use crate::win::{self, InstanceGuard};
use anyhow::{Context, bail};
use rsrewind_capture::{CaptureError, MonitorCapturer, MonitorHandle, VisibleWindow};
use rsrewind_core::{BgraFrame, Capabilities, FocusContext, MonitorInfo};
use rsrewind_ocr::OcrEngine;
use std::sync::Arc;

/// Runs until `rsrewind stop`, Ctrl+C or a fatal storage error.
pub fn run(options: RunOptions) -> anyhow::Result<()> {
    if let Err(error) = rsrewind_capture::enable_dpi_awareness() {
        tracing::warn!(%error, "could not enable per-monitor DPI awareness; capture sizes may be scaled");
    }
    let Some(guard) = WindowsInstance.acquire()? else {
        bail!("rsRewind is already recording in this Windows session");
    };
    if let Err(error) = win::install_ctrl_handler() {
        tracing::debug!(%error, "no console control handler");
    }
    let language = Some(options.config.ocr.language.clone()).filter(|l| !l.trim().is_empty());
    let platform = Platform {
        context: Box::new(WindowsContext),
        capture: Box::new(WgcCapture),
        idle: Box::new(WindowsIdle),
        clock: Arc::new(SystemClock::new()),
        lifecycle: Box::new(guard),
        ocr: Some(windows_ocr(language)),
        hostname: std::env::var("COMPUTERNAME").unwrap_or_default(),
    };
    run_with(options, platform)
}

fn fault(error: &CaptureError) -> PlatformFault {
    PlatformFault::new(error.to_string(), error.is_recoverable())
}

/// The `Local\rsRewind.Recorder` mutex plus the `rsrewind stop` event.
pub struct WindowsInstance;

impl SingleInstance for WindowsInstance {
    type Guard = InstanceGuard;

    fn acquire(&self) -> anyhow::Result<Option<InstanceGuard>> {
        InstanceGuard::acquire().context("single-instance check")
    }
}

impl Lifecycle for InstanceGuard {
    fn stop_requested(&self) -> bool {
        InstanceGuard::stop_requested(self)
    }
}

/// EnumDisplayMonitors / EnumWindows / GetForegroundWindow.
struct WindowsContext;

impl ScreenContext for WindowsContext {
    fn capabilities(&self) -> Capabilities {
        Capabilities::WINDOWS
    }

    fn monitors(&mut self) -> Result<Vec<(DisplayHandle, MonitorInfo)>, PlatformFault> {
        rsrewind_capture::monitors()
            .map(|found| {
                found
                    .into_iter()
                    .map(|(handle, info)| (DisplayHandle(handle.to_raw() as u64), info))
                    .collect()
            })
            .map_err(|error| fault(&error))
    }

    fn foreground(&mut self) -> Option<FocusContext> {
        rsrewind_capture::foreground()
    }

    fn visible_windows(&mut self) -> Result<Vec<VisibleWindow>, PlatformFault> {
        // Enumeration degrades to "fewer windows" rather than failing (remediation F4, open).
        Ok(rsrewind_capture::visible_windows())
    }
}

/// Windows.Graphics.Capture, one capturer per monitor.
struct WgcCapture;

impl CaptureBackend for WgcCapture {
    fn open(&mut self, display: DisplayHandle) -> Result<Box<dyn FrameSource>, PlatformFault> {
        let handle = MonitorHandle::from_raw(display.0 as usize);
        MonitorCapturer::new(handle)
            .map(|capturer| Box::new(WgcFrames(capturer)) as Box<dyn FrameSource>)
            .map_err(|error| fault(&error))
    }
}

struct WgcFrames(MonitorCapturer);

impl FrameSource for WgcFrames {
    fn latest_frame(&mut self) -> Result<Option<BgraFrame>, PlatformFault> {
        self.0.latest_frame().map_err(|error| fault(&error))
    }
}

/// `GetLastInputInfo`. It reports "active" when Windows will not say (remediation F4, open).
struct WindowsIdle;

impl IdleClock for WindowsIdle {
    fn idle_millis(&mut self) -> Option<u64> {
        Some(rsrewind_capture::idle_millis())
    }
}

struct WindowsOcr(OcrEngine);

impl OcrBackend for WindowsOcr {
    fn engine_name(&self) -> &'static str {
        "windows.media.ocr"
    }

    fn recognize(&mut self, frame: &BgraFrame) -> Result<Recognition, String> {
        self.0
            .recognize(frame)
            .map(|output| Recognition {
                blocks: output.blocks,
                elapsed_ms: output.elapsed_ms,
            })
            .map_err(|error| error.to_string())
    }
}

fn windows_ocr(language: Option<String>) -> OcrFactory {
    Box::new(move || {
        win::lower_current_thread_priority();
        OcrEngine::new(language.as_deref())
            .map(|engine| Box::new(WindowsOcr(engine)) as Box<dyn OcrBackend>)
            .map_err(|error| error.to_string())
    })
}
