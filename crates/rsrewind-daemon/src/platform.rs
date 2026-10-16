//! The platform seam: everything the recorder needs from the operating system, as traits.
//!
//! The recorder loop ([`crate::run_with`]), the persist thread and the OCR thread are portable and
//! talk to the desktop only through these traits. Each platform supplies a [`Platform`] value:
//! Windows wires WGC, EnumWindows, `GetLastInputInfo`, Windows.Media.Ocr and the named
//! mutex/console handler in `windows_platform.rs`; the tests wire deterministic fakes
//! (`src/tests/fakes.rs`). A platform that cannot do something says so in its [`Capabilities`]
//! instead of returning plausible-looking empty answers: an empty window list from a compositor
//! that cannot list windows would otherwise read as "nothing excluded is visible".
//!
//! Failure semantics are part of the contract ("unknown means do not record"):
//! - [`ScreenContext::visible_windows`] returning `Err` skips every monitor for that tick.
//! - [`IdleClock::idle_millis`] returning `None` is treated as idle (nothing stored).
//! - a [`Capabilities`] value whose [`Capabilities::privacy_gaps`] is non-empty stops the recorder
//!   from starting unless `privacy.unenforced_ok` is set.

use rsrewind_capture::VisibleWindow;
use rsrewind_core::{BgraFrame, Capabilities, FocusContext, MonitorInfo, OcrBlock, Timestamp};
use std::fmt;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// An opaque per-platform display key. It changes when the display is re-created (sleep/wake,
/// mode change); the recorder restarts capture when it does. Persisted identity is
/// `MonitorInfo::device_name`, never this.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DisplayHandle(pub u64);

/// Why a platform call failed. `message` names the failure (an API and an error code), never
/// captured content, so it is safe to log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlatformFault {
    pub message: String,
    /// Dropping the capturer and creating a new one is expected to help.
    pub recoverable: bool,
}

impl PlatformFault {
    pub fn new(message: impl Into<String>, recoverable: bool) -> Self {
        Self {
            message: message.into(),
            recoverable,
        }
    }
}

impl fmt::Display for PlatformFault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for PlatformFault {}

/// Frames of one display. Newest-frame-only: `Ok(None)` means nothing new since the last call
/// (Windows.Graphics.Capture only delivers on change).
pub trait FrameSource {
    fn latest_frame(&mut self) -> Result<Option<BgraFrame>, PlatformFault>;
}

/// Opens a [`FrameSource`] per display.
pub trait CaptureBackend {
    fn open(&mut self, display: DisplayHandle) -> Result<Box<dyn FrameSource>, PlatformFault>;
}

/// Displays, focus and visible windows: what privacy decisions and attribution are made over.
pub trait ScreenContext {
    /// What this provider can see. Constant for the life of the provider.
    fn capabilities(&self) -> Capabilities;
    /// Every attached display, in physical pixels.
    fn monitors(&mut self) -> Result<Vec<(DisplayHandle, MonitorInfo)>, PlatformFault>;
    /// Who has focus, or `None` if nobody does / it cannot be told.
    fn foreground(&mut self) -> Option<FocusContext>;
    /// Every visible top-level window with its rectangle, topmost first. Called only when
    /// [`Capabilities::list_windows`] is true. `Err` means the list is unknown, and the recorder
    /// then stores nothing on any monitor for that tick.
    fn visible_windows(&mut self) -> Result<Vec<VisibleWindow>, PlatformFault>;
}

/// Time since the last user input.
pub trait IdleClock {
    /// `None` when the platform cannot tell; the recorder treats that as idle.
    fn idle_millis(&mut self) -> Option<u64>;
}

/// Wall clock, monotonic clock and sleeping, so tests can run the recorder loop without waiting.
pub trait Clock: Send + Sync {
    fn now(&self) -> Timestamp;
    /// Monotonic time since an arbitrary, fixed origin.
    fn monotonic(&self) -> Duration;
    fn sleep(&self, duration: Duration);
}

/// The real clocks.
#[derive(Debug)]
pub struct SystemClock {
    origin: Instant,
}

impl SystemClock {
    pub fn new() -> Self {
        Self {
            origin: Instant::now(),
        }
    }
}

impl Default for SystemClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for SystemClock {
    fn now(&self) -> Timestamp {
        Timestamp::now()
    }

    fn monotonic(&self) -> Duration {
        self.origin.elapsed()
    }

    fn sleep(&self, duration: Duration) {
        std::thread::sleep(duration);
    }
}

/// The running recorder's view of its process lifecycle.
pub trait Lifecycle {
    /// True once a stop was requested (`rsrewind stop`, Ctrl+C, console close, a signal).
    fn stop_requested(&self) -> bool;
}

/// One recorder per user session. Windows: the `Local\rsRewind.Recorder` named mutex. Another
/// platform needs an equivalent (a lock file held for the process lifetime); none exists yet.
pub trait SingleInstance {
    type Guard: Lifecycle;
    /// `Ok(None)` when another recorder already owns this session.
    fn acquire(&self) -> anyhow::Result<Option<Self::Guard>>;
}

/// Text recognized in one frame.
#[derive(Debug, Clone, PartialEq)]
pub struct Recognition {
    pub blocks: Vec<OcrBlock>,
    pub elapsed_ms: u64,
}

/// A text recognizer. Runs on the OCR thread only.
pub trait OcrBackend {
    /// Stored with each result (`visual_states.ocr_engine`).
    fn engine_name(&self) -> &'static str;
    /// `Err` carries a failure description (I/O, decode, API code), never recognized text.
    fn recognize(&mut self, frame: &BgraFrame) -> Result<Recognition, String>;
}

/// Creates the OCR backend *on the OCR thread* (thread priority and COM apartment are per
/// thread). `Err` leaves captured states pending; `doctor` explains how to fix it.
pub type OcrFactory = Box<dyn FnOnce() -> Result<Box<dyn OcrBackend>, String> + Send>;

/// Everything one platform supplies to [`crate::run_with`].
pub struct Platform {
    pub context: Box<dyn ScreenContext>,
    pub capture: Box<dyn CaptureBackend>,
    pub idle: Box<dyn IdleClock>,
    pub clock: Arc<dyn Clock>,
    pub lifecycle: Box<dyn Lifecycle>,
    /// `None` when this platform has no text recognizer.
    pub ocr: Option<OcrFactory>,
    /// Recorded with the session. Display only.
    pub hostname: String,
}

impl Platform {
    /// What this recorder can see, OCR included.
    pub fn capabilities(&self) -> Capabilities {
        Capabilities {
            ocr: self.ocr.is_some(),
            ..self.context.capabilities()
        }
    }
}
