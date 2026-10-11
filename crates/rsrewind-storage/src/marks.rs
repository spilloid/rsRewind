//! Marks: the user's own guide marks on the timeline (`docs/design/marks-keyframes.md`).
//!
//! A mark is a moment in time with an optional short label. This module owns the writes; reading
//! marks is `rsrewind-query`'s job (`History::marks`). Labels are the user's own words about their
//! history, so they are never logged.

use crate::store::{Store, refuse_if_fenced};
use crate::{Result, StorageError};
use rsrewind_core::Timestamp;
use rusqlite::params;

/// Longest label, in characters. Matches the `CHECK` in `0002_marks.sql`.
pub const MAX_MARK_LABEL_CHARS: usize = 120;

/// Row id of a mark. Never reused (`AUTOINCREMENT`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MarkId(pub i64);

impl std::fmt::Display for MarkId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

/// A label as stored: trimmed, single line, no control characters, at most
/// [`MAX_MARK_LABEL_CHARS`] characters. An empty label is allowed (an unnamed mark).
pub fn normalize_mark_label(label: &str) -> Result<String> {
    let label = label.trim();
    if label.chars().any(char::is_control) {
        return Err(StorageError::InvalidArgument(
            "a mark label must be one line without control characters".into(),
        ));
    }
    let chars = label.chars().count();
    if chars > MAX_MARK_LABEL_CHARS {
        return Err(StorageError::InvalidArgument(format!(
            "a mark label is at most {MAX_MARK_LABEL_CHARS} characters (this one has {chars})"
        )));
    }
    Ok(label.to_owned())
}

impl Store {
    /// Marks the moment `at` with `label` (may be empty). Refused inside a deletion fence: like
    /// every writer, a mark may not put anything back into history the user erased.
    pub fn add_mark(&self, at: Timestamp, label: &str, now: Timestamp) -> Result<MarkId> {
        let label = normalize_mark_label(label)?;
        let tx = self.immediate()?;
        refuse_if_fenced(&tx, at.0)?;
        tx.execute(
            "INSERT INTO marks (at, label, created_at, updated_at) VALUES (?1, ?2, ?3, ?3)",
            params![at.0, label, now.0],
        )?;
        let id = MarkId(tx.last_insert_rowid());
        tx.commit()?;
        tracing::debug!(mark = id.0, "mark added");
        Ok(id)
    }

    /// Replaces a mark's label. `false` if there is no such mark.
    pub fn rename_mark(&self, id: MarkId, label: &str, now: Timestamp) -> Result<bool> {
        let label = normalize_mark_label(label)?;
        let changed = self.conn.execute(
            "UPDATE marks SET label = ?2, updated_at = ?3 WHERE id = ?1",
            params![id.0, label, now.0],
        )?;
        Ok(changed > 0)
    }

    /// Deletes a mark. `false` if there was no such mark.
    pub fn remove_mark(&self, id: MarkId) -> Result<bool> {
        let changed = self
            .conn
            .execute("DELETE FROM marks WHERE id = ?1", [id.0])?;
        Ok(changed > 0)
    }
}
