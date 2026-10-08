//! Deterministic fake backends for the platform seam.
//!
//! Nothing here sleeps or reads the real clock. Time moves only when the recorder sleeps on the
//! [`FakeClock`], and that sleep runs scheduled hooks: a hook is how a test changes the world
//! (pause, open a window, wait for OCR) at an exact point between two ticks.

use crate::platform::{
    CaptureBackend, Clock, DisplayHandle, FrameSource, IdleClock, Lifecycle, OcrBackend,
    OcrFactory, Platform, PlatformFault, Recognition, ScreenContext,
};
use rsrewind_capture::{ScreenRect, VisibleWindow};
use rsrewind_core::{
    ApplicationContext, BgraFrame, Capabilities, FocusContext, MonitorInfo, OcrBlock, Timestamp,
    WindowContext,
};
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::Duration;

/// Wall-clock origin of every fake run: 2026-09-21T13:46:40Z.
pub const WALL0: i64 = 1_790_000_000_000;

type Hook = Box<dyn FnOnce() + Send>;

pub struct FakeClock {
    mono: Mutex<Duration>,
    hooks: Mutex<Vec<(Duration, Hook)>>,
}

impl FakeClock {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            mono: Mutex::new(Duration::ZERO),
            hooks: Mutex::new(Vec::new()),
        })
    }

    /// Runs `hook` on the recorder's thread the first time its sleep reaches `at`.
    pub fn at(&self, at: Duration, hook: impl FnOnce() + Send + 'static) {
        lock(&self.hooks).push((at, Box::new(hook)));
    }

    pub fn advance(&self, by: Duration) {
        let now = {
            let mut mono = lock(&self.mono);
            *mono += by;
            *mono
        };
        loop {
            let due = {
                let mut hooks = lock(&self.hooks);
                let next = hooks
                    .iter()
                    .enumerate()
                    .filter(|(_, (at, _))| *at <= now)
                    .min_by_key(|(_, (at, _))| *at)
                    .map(|(i, _)| i);
                next.map(|i| hooks.remove(i))
            };
            match due {
                Some((_, hook)) => hook(),
                None => break,
            }
        }
    }

    pub fn wall_at(at: Duration) -> Timestamp {
        Timestamp(WALL0 + at.as_millis() as i64)
    }
}

impl Clock for FakeClock {
    fn now(&self) -> Timestamp {
        Self::wall_at(*lock(&self.mono))
    }

    fn monotonic(&self) -> Duration {
        *lock(&self.mono)
    }

    fn sleep(&self, duration: Duration) {
        self.advance(duration);
    }
}

/// Stops the recorder once the fake clock reaches `at`.
pub struct StopAt {
    pub clock: Arc<FakeClock>,
    pub at: Duration,
}

impl Lifecycle for StopAt {
    fn stop_requested(&self) -> bool {
        self.clock.monotonic() >= self.at
    }
}

/// The scripted desktop every fake reads. Tests change it between ticks.
#[derive(Default)]
pub struct Desktop {
    pub monitors: Vec<(DisplayHandle, MonitorInfo)>,
    /// `None` makes `visible_windows` fail.
    pub windows: Option<Vec<VisibleWindow>>,
    pub focus: Option<FocusContext>,
    /// `None` makes `idle_millis` report "unknown".
    pub idle_ms: Option<u64>,
    /// Frames the platform produced and the recorder has not yet taken, per display.
    pub pending: HashMap<DisplayHandle, VecDeque<BgraFrame>>,
    /// How often each display was asked for a frame.
    pub polls: HashMap<DisplayHandle, usize>,
    pub window_queries: usize,
}

pub type SharedDesktop = Arc<Mutex<Desktop>>;

impl Desktop {
    /// The platform produced `frame` on display `handle` (it is delivered on the next poll).
    pub fn show(&mut self, handle: DisplayHandle, frame: BgraFrame) {
        self.pending.entry(handle).or_default().push_back(frame);
    }

    pub fn polls(&self, handle: DisplayHandle) -> usize {
        self.polls.get(&handle).copied().unwrap_or(0)
    }
}

pub fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    // A panicking test thread poisons the lock; the data is still what the test wrote.
    mutex.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

pub struct FakeContext {
    pub desktop: SharedDesktop,
    pub capabilities: Capabilities,
}

impl ScreenContext for FakeContext {
    fn capabilities(&self) -> Capabilities {
        self.capabilities
    }

    fn monitors(&mut self) -> Result<Vec<(DisplayHandle, MonitorInfo)>, PlatformFault> {
        Ok(lock(&self.desktop).monitors.clone())
    }

    fn foreground(&mut self) -> Option<FocusContext> {
        lock(&self.desktop).focus.clone()
    }

    fn visible_windows(&mut self) -> Result<Vec<VisibleWindow>, PlatformFault> {
        let mut desktop = lock(&self.desktop);
        desktop.window_queries += 1;
        desktop
            .windows
            .clone()
            .ok_or_else(|| PlatformFault::new("scripted enumeration failure", false))
    }
}

pub struct FakeCapture {
    pub desktop: SharedDesktop,
}

impl CaptureBackend for FakeCapture {
    fn open(&mut self, display: DisplayHandle) -> Result<Box<dyn FrameSource>, PlatformFault> {
        Ok(Box::new(FakeFrames {
            desktop: self.desktop.clone(),
            display,
        }))
    }
}

/// Newest-frame-only, like Windows.Graphics.Capture: a poll takes everything pending and returns
/// the last one.
pub struct FakeFrames {
    desktop: SharedDesktop,
    display: DisplayHandle,
}

impl FrameSource for FakeFrames {
    fn latest_frame(&mut self) -> Result<Option<BgraFrame>, PlatformFault> {
        let mut desktop = lock(&self.desktop);
        *desktop.polls.entry(self.display).or_default() += 1;
        Ok(desktop
            .pending
            .get_mut(&self.display)
            .and_then(|queue| queue.drain(..).last()))
    }
}

pub struct FakeIdle {
    pub desktop: SharedDesktop,
}

impl IdleClock for FakeIdle {
    fn idle_millis(&mut self) -> Option<u64> {
        lock(&self.desktop).idle_ms
    }
}

/// In-memory OCR: reads the seed back out of a [`frame`] and "recognizes" `screen <seed>`.
#[derive(Default)]
pub struct OcrProbe {
    pub recognized: Mutex<usize>,
    pub changed: Condvar,
    /// Set when the backend is dropped, i.e. when the OCR thread has finished.
    pub dropped: AtomicBool,
    pub created: AtomicUsize,
}

impl OcrProbe {
    /// Blocks until `count` frames were recognized, or `timeout` passes (a safety net for a broken
    /// build, never the synchronisation itself). Returns whether the count was reached.
    pub fn wait_for(&self, count: usize, timeout: Duration) -> bool {
        let guard = lock(&self.recognized);
        let (guard, _) = self
            .changed
            .wait_timeout_while(guard, timeout, |n| *n < count)
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *guard >= count
    }
}

struct FakeOcr(Arc<OcrProbe>);

impl OcrBackend for FakeOcr {
    fn engine_name(&self) -> &'static str {
        "fake.ocr"
    }

    fn recognize(&mut self, frame: &BgraFrame) -> Result<Recognition, String> {
        let seed = seed_of(frame).ok_or("not a fake frame")?;
        let result = Recognition {
            blocks: vec![OcrBlock {
                text: format!("screen {seed}"),
                x: 0.0,
                y: 0.0,
                width: 10.0,
                height: 10.0,
                confidence: None,
                line_index: 0,
            }],
            elapsed_ms: 1,
        };
        *lock(&self.0.recognized) += 1;
        self.0.changed.notify_all();
        Ok(result)
    }
}

impl Drop for FakeOcr {
    fn drop(&mut self) {
        self.0.dropped.store(true, Ordering::SeqCst);
    }
}

pub fn fake_ocr(probe: Arc<OcrProbe>) -> OcrFactory {
    Box::new(move || {
        probe.created.fetch_add(1, Ordering::SeqCst);
        Ok(Box::new(FakeOcr(probe)) as Box<dyn OcrBackend>)
    })
}

/// A whole fake platform over `desktop`, stopping at `stop_at` on the fake clock.
pub fn platform(
    desktop: &SharedDesktop,
    capabilities: Capabilities,
    clock: &Arc<FakeClock>,
    stop_at: Duration,
    ocr: Option<OcrFactory>,
) -> Platform {
    Platform {
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
        lifecycle: Box::new(StopAt {
            clock: clock.clone(),
            at: stop_at,
        }),
        ocr,
        hostname: "fakehost".into(),
    }
}

// ----- scripted content ----------------------------------------------------------------------

pub const W: u32 = 128;
pub const H: u32 = 72;

/// A uniform frame whose grey level encodes `seed`; any two seeds differ in every fingerprint
/// cell, so the change detector always calls them different.
pub fn frame(seed: u8) -> BgraFrame {
    let level = seed_level(seed);
    BgraFrame {
        width: W,
        height: H,
        stride: W * 4,
        pixels: [level, level, level, 255].repeat((W * H) as usize),
    }
}

fn seed_level(seed: u8) -> u8 {
    8u8.saturating_add(seed.saturating_mul(16))
}

/// Reads the seed back from a [`frame`] (also after a WebP round trip, which is lossy).
pub fn seed_of(frame: &BgraFrame) -> Option<u8> {
    let level = *frame.pixels.first()?;
    (0..16u8).find(|&seed| seed_level(seed).abs_diff(level) <= 4)
}

pub fn display(index: u64) -> DisplayHandle {
    DisplayHandle(100 + index)
}

/// Monitor `index` (0-based), side by side, W x H each.
pub fn monitor(index: u64) -> (DisplayHandle, MonitorInfo) {
    (
        display(index),
        MonitorInfo {
            device_name: format!(r"\\.\DISPLAY{}", index + 1),
            left: (index as i32) * W as i32,
            top: 0,
            width: W,
            height: H,
            dpi: 96,
            primary: index == 0,
        },
    )
}

pub fn device(index: u64) -> String {
    format!(r"\\.\DISPLAY{}", index + 1)
}

/// A window covering `[left, right)` horizontally, full height.
pub fn window(process: &str, title: &str, pid: u32, left: i32, right: i32) -> VisibleWindow {
    VisibleWindow {
        process_name: process.into(),
        title: title.into(),
        rect: ScreenRect {
            left,
            top: 0,
            right,
            bottom: H as i32,
        },
        monitor: String::new(),
        pid,
    }
}

/// A window filling monitor `index`.
pub fn window_on(process: &str, title: &str, pid: u32, index: i32) -> VisibleWindow {
    window(process, title, pid, index * W as i32, (index + 1) * W as i32)
}

pub fn focus(process: &str, title: &str, pid: u32) -> FocusContext {
    FocusContext {
        application: ApplicationContext {
            process_name: process.into(),
            exe_path: None,
        },
        window: WindowContext {
            title: title.into(),
            class_name: None,
        },
        pid,
    }
}
