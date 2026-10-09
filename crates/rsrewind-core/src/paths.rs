//! The data directory layout.
//!
//! ```text
//! %LOCALAPPDATA%\rsRewind\        (Linux: $XDG_DATA_HOME/rsRewind, else ~/.local/share/rsRewind;
//!                                 macOS: ~/Library/Application Support/rsRewind)
//!   config.toml
//!   recall.db (+ -wal, -shm)
//!   media\2026\09\30\20260930T201404123Z_m2.webp
//!   logs\
//!   backups\
//!   models\
//!   outbox\         (sealed segments awaiting transfer; probes only)
//!   sources\<id>\  (one full store per remote source; central instances only)
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

    /// `%RSREWIND_DATA_DIR%` if set, else `<LocalAppData known folder>\rsRewind` (see the module
    /// docs for the other platforms).
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
    /// Sealed segments waiting to be carried to another rsRewind (see `rsrewind export`).
    pub fn outbox(&self) -> PathBuf {
        self.root.join("outbox")
    }
    /// Where a central instance keeps one complete, independent store per remote source.
    pub fn sources_root(&self) -> PathBuf {
        self.root.join("sources")
    }
    /// The store for one remote source, or `None` unless `source_id` is exactly 32 lowercase hex
    /// characters: it becomes a directory name, so anything else is refused here.
    pub fn source_dir(&self, source_id: &str) -> Option<DataDir> {
        let valid = source_id.len() == 32
            && source_id
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
        valid.then(|| DataDir::new(self.sources_root().join(source_id)))
    }

    /// Resolves a stored relative media path. Rejects anything that could escape the media
    /// subtree (absolute paths, `..`, drive prefixes) and anything that is not *under* `media/`:
    /// the database is user-editable, and a tampered row naming `config.toml` or `recall.db` must
    /// never be readable, writable or deletable through this path.
    ///
    /// This is a lexical check. [`DataDir::resolve_media_checked`] adds the stored-path rules
    /// (forward slashes only) and rejects reparse points (symlinks, junctions) inside `media/`.
    pub fn resolve_media(&self, relative: &str) -> Option<PathBuf> {
        if relative.is_empty() || relative.contains(':') || relative.starts_with(['/', '\\']) {
            return None;
        }
        let mut path = self.root.clone();
        let mut depth = 0usize;
        for part in relative.split(['/', '\\']) {
            match part {
                "" | "." | ".." => return None,
                part => {
                    // The first component must be exactly `media` (case-insensitive: NTFS).
                    if depth == 0 && !part.eq_ignore_ascii_case("media") {
                        return None;
                    }
                    path.push(part);
                    depth += 1;
                }
            }
        }
        // `media` alone is the directory, not a file.
        (depth >= 2).then_some(path)
    }

    /// Maps a media path *as stored in the database* to an absolute path, rejecting anything
    /// unsafe: everything [`DataDir::resolve_media`] rejects, any backslash (stored paths use
    /// forward slashes only, so a backslash means someone else wrote the value), and any existing
    /// component below the data root that is a symlink or junction.
    ///
    /// This is the one place both the writer (`rsrewind-storage`) and the readers
    /// (`rsrewind-query`) turn a stored path into a file system path.
    pub fn resolve_media_checked(
        &self,
        relative: &str,
    ) -> std::result::Result<PathBuf, MediaPathError> {
        if relative.contains('\\') {
            return Err(MediaPathError::Unsafe(relative.to_owned()));
        }
        let path = self
            .resolve_media(relative)
            .ok_or_else(|| MediaPathError::Unsafe(relative.to_owned()))?;
        self.reject_reparse_points(&path, relative)?;
        Ok(path)
    }

    /// Fails if any existing component of `path` below the data root is a symlink or junction.
    ///
    /// A lexically clean `media/link/x.webp` is still outside the data folder when `link` points
    /// elsewhere. The root itself may legitimately be a link (history on another drive), so only
    /// the components under it are checked. This is check-then-use: it stops tampered rows and
    /// stray links, not an attacker racing the recorder, who already runs as this user.
    fn reject_reparse_points(
        &self,
        path: &Path,
        relative: &str,
    ) -> std::result::Result<(), MediaPathError> {
        let Ok(below_root) = path.strip_prefix(&self.root) else {
            return Err(MediaPathError::Unsafe(relative.to_owned()));
        };
        let mut current = self.root.clone();
        for component in below_root.components() {
            current.push(component);
            match std::fs::symlink_metadata(&current) {
                Ok(metadata) if is_reparse_point(&metadata) => {
                    return Err(MediaPathError::Unsafe(relative.to_owned()));
                }
                Ok(_) => {}
                // Nothing exists from here down yet; there is nothing to follow.
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
                Err(source) => {
                    return Err(MediaPathError::Io {
                        path: current,
                        source,
                    });
                }
            }
        }
        Ok(())
    }
}

/// Why a stored media path was refused by [`DataDir::resolve_media_checked`].
#[derive(Debug, thiserror::Error)]
pub enum MediaPathError {
    /// The path is not a safe path inside the data directory's `media/` tree.
    #[error("media path {0:?} is not a safe path inside the data directory")]
    Unsafe(String),
    /// A component could not be inspected.
    #[error("I/O error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

/// A symlink, or (on Windows) anything carrying a reparse tag, junctions included.
pub fn is_reparse_point(metadata: &std::fs::Metadata) -> bool {
    metadata.file_type().is_symlink() || has_reparse_attribute(metadata)
}

/// FILE_ATTRIBUTE_REPARSE_POINT: junctions and every other reparse tag.
#[cfg(windows)]
fn has_reparse_attribute(metadata: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    const REPARSE_POINT: u32 = 0x400;
    metadata.file_attributes() & REPARSE_POINT != 0
}

#[cfg(not(windows))]
fn has_reparse_attribute(_: &std::fs::Metadata) -> bool {
    false
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

/// The per-user data folder outside Windows: `~/Library/Application Support` on macOS, the XDG
/// data home elsewhere.
#[cfg(not(windows))]
fn local_app_data() -> Result<PathBuf> {
    unix_data_home(
        std::env::var_os("HOME"),
        std::env::var_os("XDG_DATA_HOME"),
        cfg!(target_os = "macos"),
    )
}

/// A relative or empty `XDG_DATA_HOME` is ignored, as the XDG spec requires.
#[cfg_attr(windows, allow(dead_code))]
fn unix_data_home(
    home: Option<std::ffi::OsString>,
    xdg_data_home: Option<std::ffi::OsString>,
    macos: bool,
) -> Result<PathBuf> {
    let home = home
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| CoreError::DataDir(format!("HOME is not set; set {DATA_DIR_ENV} instead")));
    if macos {
        return Ok(home?.join("Library").join("Application Support"));
    }
    match xdg_data_home.map(PathBuf::from) {
        Some(dir) if dir.is_absolute() => Ok(dir),
        _ => Ok(home?.join(".local").join("share")),
    }
}

#[cfg(test)]
mod tests {

    // Unix path semantics: on Windows `/data` is not absolute (no drive), and the function is unused.
    #[cfg(not(windows))]
    #[test]
    fn unix_data_home_follows_xdg_and_ignores_relative_values() {
        let home = Some("/home/u".into());
        let at =
            |xdg: Option<&str>, macos| unix_data_home(home.clone(), xdg.map(Into::into), macos);
        assert_eq!(
            at(None, false).ok(),
            Some(PathBuf::from("/home/u/.local/share"))
        );
        assert_eq!(at(Some("/data"), false).ok(), Some(PathBuf::from("/data")));
        assert_eq!(
            at(Some("rel/dir"), false).ok(),
            Some(PathBuf::from("/home/u/.local/share"))
        );
        assert_eq!(
            at(Some(""), false).ok(),
            Some(PathBuf::from("/home/u/.local/share"))
        );
        assert_eq!(
            at(Some("/data"), true).ok(),
            Some(PathBuf::from("/home/u/Library/Application Support"))
        );
        assert!(unix_data_home(None, Some("/data".into()), false).is_ok());
        assert!(unix_data_home(None, None, false).is_err());
        assert!(unix_data_home(Some("".into()), None, false).is_err());
    }

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
            Some(
                ["media", "2026", "09", "30", "a.webp"]
                    .iter()
                    .fold(data.root().to_path_buf(), |p, part| p.join(part))
            )
        );
        for bad in [
            "config.toml",
            "recall.db",
            "logs/x.log",
            "media",
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
    fn checked_resolution_refuses_backslashes_and_links()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let tmp = tempfile::tempdir()?;
        let data = DataDir::new(tmp.path());
        // Lexically fine but stored paths never contain a backslash.
        assert!(data.resolve_media("media\\a\\b.webp").is_some());
        assert!(matches!(
            data.resolve_media_checked("media\\a\\b.webp"),
            Err(MediaPathError::Unsafe(_))
        ));
        assert!(matches!(
            data.resolve_media_checked("recall.db"),
            Err(MediaPathError::Unsafe(_))
        ));
        // Not existing yet is fine: the writer resolves before creating.
        assert_eq!(
            data.resolve_media_checked("media/2026/x.webp")?,
            tmp.path().join("media").join("2026").join("x.webp")
        );
        #[cfg(unix)]
        {
            let outside = tempfile::tempdir()?;
            std::fs::create_dir_all(tmp.path().join("media"))?;
            std::os::unix::fs::symlink(outside.path(), tmp.path().join("media").join("link"))?;
            assert!(matches!(
                data.resolve_media_checked("media/link/x.webp"),
                Err(MediaPathError::Unsafe(_))
            ));
        }
        Ok(())
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
