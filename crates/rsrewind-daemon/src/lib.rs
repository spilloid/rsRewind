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

pub mod counters;
pub mod plan;

#[cfg(windows)]
mod ocr_worker;
#[cfg(windows)]
mod persist;
#[cfg(windows)]
mod recorder;
#[cfg(windows)]
pub mod win;

#[cfg(windows)]
pub use recorder::{RunOptions, run};
