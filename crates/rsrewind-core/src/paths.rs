//! The data directory layout.
//!
//! ```text
//! %LOCALAPPDATA%\rsRewind\
//!   config.toml
//!   recall.db (+ -wal, -shm)
//!   media\2026\09\30\20260930T201404123Z_m2.webp
//!   logs\
//!   backups\
//!   models\
//! ```
//!
//! Media paths stored in SQLite are *relative* to the data root, with forward slashes, so a user
//! can move or back up the whole folder and nothing breaks.

use crate::{CoreError, MonitorId, Result, Timestamp};
use std::path::{Path, PathBuf};

/// Overrides the data root. Used by tests and by anyone who keeps history on another drive.
pub const DATA_DIR_ENV: &str = "RSREWIND_DATA_DIR";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataDir {
    root: PathBuf,
}

impl DataDir {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// `%RSREWIND_DATA_DIR%` if set, else `<LocalAppData known folder>\rsRewind`.
    pub fn resolve() -> Result<Self> {
        if let Some(root) = std::env::var_os(DATA_DIR_ENV).filter(|v| !v.is_empty()) {
            return Ok(Self::new(root));
        }
        Ok(Self::new(local_app_data()?.join("rsRewind")))
    }

    /// Creates the directory tree. Idempotent.
    pub fn ensure(&self) -> Result<()> {
        for dir in [
            self.root.clone(),
            self.media_root(),
            self.logs(),
            self.backups(),
        ] {
            std::fs::create_dir_all(&dir).map_err(|e| CoreError::io(&dir, e))?;
        }
        Ok(())
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
    pub fn config_file(&self) -> PathBuf {
        self.root.join("config.toml")
    }
    pub fn database(&self) -> PathBuf {
        self.root.join("recall.db")
    }
    pub fn media_root(&self) -> PathBuf {
        self.root.join("media")
    }
    pub fn logs(&self) -> PathBuf {
        self.root.join("logs")
    }
    pub fn backups(&self) -> PathBuf {
        self.root.join("backups")
    }
    pub fn models(&self) -> PathBuf {
        self.root.join("models")
    }

    /// Resolves a stored relative media path. Rejects anything that could escape the data root
    /// (absolute paths, `..`, drive prefixes): the database is user-editable, the filesystem
    /// outside this folder is not ours to read or delete.
    pub fn resolve_media(&self, relative: &str) -> Option<PathBuf> {
        if relative.is_empty() || relative.contains(':') || relative.starts_with(['/', '\\']) {
            return None;
        }
        let mut path = self.root.clone();
        for part in relative.split(['/', '\\']) {
            match part {
                "" | "." | ".." => return None,
                part => path.push(part),
            }
        }
        Some(path)
    }
}

/// Relative path (forward slashes) for a new visual state image.
///
/// UTC date folders keep a day's files together regardless of DST; the millisecond timestamp plus
/// monitor id is unique per capture tick. Callers still create files exclusively, so a collision
/// is an error rather than an overwrite.
pub fn media_relative_path(captured_at: Timestamp, monitor: MonitorId) -> String {
    let utc = captured_at.to_utc();
    format!(
        "media/{}/{}_m{}.webp",
        utc.format("%Y/%m/%d"),
        utc.format("%Y%m%dT%H%M%S%3fZ"),
        monitor.0
    )
}

#[cfg(windows)]
fn local_app_data() -> Result<PathBuf> {
    use windows::Win32::System::Com::CoTaskMemFree;
    use windows::Win32::UI::Shell::{FOLDERID_LocalAppData, KF_FLAG_DEFAULT, SHGetKnownFolderPath};

    // SAFETY: SHGetKnownFolderPath returns a CoTaskMem-allocated, NUL-terminated wide string that
    // we own and must free exactly once, after copying it out.
    unsafe {
        let raw = SHGetKnownFolderPath(&FOLDERID_LocalAppData, KF_FLAG_DEFAULT, None)
            .map_err(|e| CoreError::DataDir(e.to_string()))?;
        let path = raw.to_string();
        CoTaskMemFree(Some(raw.0 as *const _));
        path.map(PathBuf::from)
            .map_err(|e| CoreError::DataDir(e.to_string()))
    }
}

#[cfg(not(windows))]
fn local_app_data() -> Result<PathBuf> {
    Err(CoreError::DataDir(format!(
        "rsRewind is Windows-only; set {DATA_DIR_ENV} to run its portable parts elsewhere"
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn media_paths_are_dated_and_unique_per_monitor() {
        // 2026-09-30T20:14:04.123Z
        let ts = Timestamp(1_790_799_244_123);
        assert_eq!(
            media_relative_path(ts, MonitorId(2)),
            format!(
                "media/{}_m2.webp",
                ts.to_utc().format("%Y/%m/%d/%Y%m%dT%H%M%S%3fZ")
            )
        );
        assert_ne!(
            media_relative_path(ts, MonitorId(1)),
            media_relative_path(ts, MonitorId(2))
        );
        assert!(media_relative_path(ts, MonitorId(1)).starts_with("media/20"));
    }

    #[test]
    fn resolve_media_stays_inside_root() {
        let data = DataDir::new(r"C:\data\rsRewind");
        assert_eq!(
            data.resolve_media("media/2026/09/30/a.webp"),
            Some(PathBuf::from(r"C:\data\rsRewind\media\2026\09\30\a.webp"))
        );
        for bad in [
            "",
            "../x.webp",
            "media/../../x",
            "/etc/passwd",
            "\\\\server\\share\\x",
            "C:/Windows/x",
            "media//x",
            "./media/x",
        ] {
            assert_eq!(data.resolve_media(bad), None, "{bad}");
        }
    }

    #[test]
    fn ensure_creates_layout() -> std::result::Result<(), Box<dyn std::error::Error>> {
        let tmp = tempfile::tempdir()?;
        let data = DataDir::new(tmp.path().join("rsRewind"));
        data.ensure()?;
        data.ensure()?;
        assert!(data.media_root().is_dir());
        assert!(data.logs().is_dir());
        assert!(data.backups().is_dir());
        Ok(())
    }
}
