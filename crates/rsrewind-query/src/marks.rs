//! Reading marks (schema v2), the user's own guide marks on the timeline. Written only by
//! `rsrewind-storage` (`rsrewind mark ...`); see `docs/design/marks-keyframes.md`.

use crate::{QueryDb, Result};
use rsrewind_core::Timestamp;

/// Most marks returned at once. Marks are made by hand; this only bounds a tampered database.
pub const MAX_MARKS: u32 = 10_000;

/// Longest label handed out, in characters (the writer allows 120).
const MAX_LABEL_CHARS: usize = 120;

/// A moment the user marked, with an optional label.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mark {
    /// Row id in this data folder's own store; what `rsrewind mark rename/remove` take.
    pub id: i64,
    pub at: Timestamp,
    /// Display text: control characters removed and length bounded, whatever the row holds.
    pub label: String,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

impl QueryDb {
    /// Every mark, oldest moment first (at most [`MAX_MARKS`]). A v1 store has none.
    ///
    /// Whether the table exists is checked on every call, not taken from the version seen at open:
    /// the first `rsrewind mark` after an upgrade migrates a store this connection already holds.
    pub fn marks(&self) -> Result<Vec<Mark>> {
        let has_table: bool = self.conn.query_row(
            "SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'marks')",
            [],
            |row| row.get(0),
        )?;
        if !has_table {
            return Ok(Vec::new());
        }
        let mut stmt = self.conn.prepare(
            "SELECT id, at, label, created_at, updated_at FROM marks ORDER BY at, id LIMIT ?1",
        )?;
        let rows = stmt.query_map([MAX_MARKS], |row| {
            Ok(Mark {
                id: row.get(0)?,
                at: Timestamp(row.get(1)?),
                label: clean_label(&row.get::<_, String>(2)?),
                created_at: Timestamp(row.get(3)?),
                updated_at: Timestamp(row.get(4)?),
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }
}

/// The database is user-editable: a label is display text only after control characters (terminal
/// escapes, newlines) are dropped and its length bounded.
pub(crate) fn clean_label(label: &str) -> String {
    label
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_LABEL_CHARS)
        .collect::<String>()
        .trim()
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_are_cleaned_for_display() {
        assert_eq!(clean_label("  call\u{1b}[2J back\n"), "call[2J back");
        assert_eq!(clean_label(&"x".repeat(500)).len(), MAX_LABEL_CHARS);
        assert_eq!(clean_label("\u{7}"), "");
    }
}
