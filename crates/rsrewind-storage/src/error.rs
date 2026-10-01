use rsrewind_core::{CoreError, VisualStateId};
use std::path::PathBuf;

pub type Result<T, E = StorageError> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error("SQLite error: {0}")]
    Sqlite(#[from] rusqlite::Error),

    #[error("I/O error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error(transparent)]
    Core(#[from] CoreError),

    #[error("no rsRewind database at {0} (has the recorder ever run?)")]
    DatabaseMissing(PathBuf),

    #[error(
        "database schema version {found} is newer than this build supports ({supported}); \
         upgrade rsRewind"
    )]
    SchemaTooNew { found: u32, supported: u32 },

    #[error("could not back up the database before migrating, so it was left untouched: {0}")]
    BackupFailed(String),

    #[error("migration {version} ({name}) failed: {source}")]
    Migration {
        version: u32,
        name: &'static str,
        #[source]
        source: rusqlite::Error,
    },

    #[error("media path {0:?} is not a safe path inside the data directory")]
    InvalidMediaPath(String),

    #[error("refusing to overwrite existing media file {0}")]
    MediaExists(PathBuf),

    #[error("image error: {0}")]
    Image(String),

    #[error("visual state {0} does not exist")]
    VisualStateMissing(VisualStateId),

    /// The write falls inside an interval the user deleted (`forget`). The caller must discard
    /// what it wrote (media file), drop any cached ids for the frame, and may store the current
    /// picture again later: only the *captured_at* inside the fence is refused.
    #[error("refused: time {at} is inside a deleted interval [{since}, {until})")]
    Fenced { at: i64, since: i64, until: i64 },

    /// An application/window id the writer cached no longer exists (deleted with its history).
    /// Drop the writer's caches and upsert again.
    #[error("the application or window context no longer exists; re-resolve it")]
    ContextMissing,

    #[error("invalid argument: {0}")]
    InvalidArgument(String),

    #[error("unexpected value in the database: {0}")]
    Corrupt(String),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
}

impl StorageError {
    pub(crate) fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Self::Io {
            path: path.into(),
            source,
        }
    }
}
