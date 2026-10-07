//! rsRewind's write side: the SQLite schema and migrations, the recorder's inserts, OCR results,
//! control/status rows, deletion and retention, and the WebP media files.
//!
//! One [`Store`] is one connection. Every write connection runs in WAL mode with
//! `synchronous=NORMAL`, `foreign_keys=ON` and a 5 s busy timeout, so the daemon's persist and
//! OCR threads (and a short-lived CLI) can each hold their own.

mod error;
pub mod media;
pub mod migrations;
mod segment;
mod store;

pub use error::{Result, StorageError};
pub use migrations::{MIGRATIONS, Migration, SCHEMA_VERSION};
pub use segment::{
    DEFAULT_MAX_EVENTS, DEFAULT_SETTLE_MS, ExportOptions, ExportReport, ExportStatus,
    ImportOutcome, ImportReport, MIN_SETTLE_MS, OutboxScan, OutboxSegment, SourceIdentity,
    SourceRole, import_into_root, scan_outbox,
};
pub use store::{
    DeleteReport, NewVisualState, Observation, OcrStatus, OrphanReport, PendingOcr, RecorderStatus,
    StorageStats, Store,
};

#[cfg(test)]
mod tests;
