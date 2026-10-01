//! Screen capture and desktop context for rsRewind (Windows only).
//!
//! - [`monitors`] / [`enable_dpi_awareness`]: what displays exist, in physical pixels.
//! - [`MonitorCapturer`]: Windows.Graphics.Capture per monitor, newest-frame-only, CPU copy.
//! - [`foreground`] / [`visible_windows`]: who has focus and what is on screen, for privacy rules
//!   and event metadata.
//! - [`idle_millis`]: time since last keyboard/mouse input.
//! - [`change`]: pure, platform-independent "did the screen meaningfully change?" logic.
//!
//! Window titles are captured content: this crate never logs them, and [`VisibleWindow`]'s
//! `Debug` impl redacts them.

pub mod change;

#[cfg(windows)]
mod error;
#[cfg(windows)]
mod idle;
#[cfg(windows)]
mod monitor;
#[cfg(windows)]
mod wgc;
#[cfg(windows)]
mod wide;
#[cfg(windows)]
mod window;

pub use change::{ChangeDetection, ChangeDetector, Fingerprint, FingerprintError};
#[cfg(windows)]
pub use error::{CaptureError, Result, is_access_denied, is_recoverable_hresult};
#[cfg(windows)]
pub use idle::idle_millis;
#[cfg(windows)]
pub use monitor::{DpiAwareness, MonitorHandle, enable_dpi_awareness, monitors};
#[cfg(windows)]
pub use wgc::{Adapter, MonitorCapturer};
#[cfg(windows)]
pub use window::{ScreenRect, UNKNOWN_PROCESS, VisibleWindow, foreground, visible_windows};
