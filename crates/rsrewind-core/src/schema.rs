//! The database schema version, shared by the writer (`rsrewind-storage`) and the readers
//! (`rsrewind-query`).
//!
//! It lives here rather than in storage so a reader can check it without depending on the crate
//! that owns the writes (and, through it, everything the recorder needs). Storage asserts at compile
//! time that its last migration has exactly this version, so the two cannot drift apart.

/// The schema version this build writes and expects.
pub const SCHEMA_VERSION: u32 = 1;
