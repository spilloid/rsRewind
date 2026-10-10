//! The recorder: capture -> change detection -> storage -> OCR.
//!
//! Threading model (no async runtime):
//!
//! ```text
//! main thread: capture loop (tick = 1 / fps_candidate)
//!     |  try_send (bounded; a full queue drops the candidate, never blocks capture)
//!     v
//! persist thread: owns write connection #1; encodes WebP, writes the file, inserts rows,
//!                 extends observation spans, runs retention hourly
//!     |  try_send wake (bounded, lossy: the backlog lives in SQLite, not in memory)
//!     v
//! OCR thread (below-normal priority): owns write connection #2; pulls `ocr_status = pending`
//! ```
//!
//! The capture thread keeps a third connection only to read pause/resume control and write the
//! heartbeat. Control and status flow through SQLite; the recorder opens no sockets or pipes.
//!
//! Everything above is portable and talks to the desktop only through [`platform`]: the Windows
//! adapters live in `windows_platform.rs` (`run` on Windows), and the tests drive the same loop
//! with deterministic fakes. Any other platform has no backends yet.

pub mod counters;
#[cfg(target_os = "linux")]
pub mod linux_platform;
mod ocr_worker;
mod persist;
pub mod plan;
pub mod platform;
mod recorder;
#[cfg(windows)]
pub mod win;
#[cfg(windows)]
mod windows_platform;

#[cfg(test)]
mod tests;

#[cfg(target_os = "linux")]
pub use linux_platform::run;
pub use recorder::{RunOptions, run_with};
#[cfg(windows)]
pub use windows_platform::{WindowsInstance, run};

#[cfg(target_os = "macos")]
pub mod macos_platform;
#[cfg(target_os = "macos")]
pub use macos_platform::run;
