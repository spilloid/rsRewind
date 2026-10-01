//! Query-facing types shared by the query layer, the CLI (`--json`) and the UI.
//!
//! These serialize to the CLI's JSON output, which is an integration surface for scripts and
//! agents: add fields freely, but do not rename or remove them without a version bump.

use crate::{EventId, OcrBlock, Timestamp, VisualStateId};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchQuery {
    /// Free text as the user typed it. The query layer turns it into a safe FTS5 expression; it is
    /// never spliced into SQL.
    pub text: String,
    pub since: Option<Timestamp>,
    pub until: Option<Timestamp>,
    /// Case-insensitive exact process name, e.g. `Teams.exe` (the `.exe` is optional).
    pub application: Option<String>,
    /// Case-insensitive substring of the window title.
    pub title_contains: Option<String>,
    pub limit: u32,
}

/// One search result: a visual state whose OCR text matched, plus where and when it was seen.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SearchHit {
    pub visual_state_id: VisualStateId,
    /// When this visual state was first observed.
    pub timestamp: Timestamp,
    /// RFC 3339 UTC rendering of `timestamp`, for consumers that do not want to do the math.
    pub timestamp_utc: String,
    pub application: Option<String>,
    pub window_title: Option<String>,
    pub monitor: Option<String>,
    /// Matching excerpt. Match boundaries are marked with `[` and `]`.
    pub snippet: String,
    /// Absolute path of the stored image.
    pub media_path: String,
    /// FTS rank (lower is better, as SQLite's bm25 reports it).
    pub rank: f64,
}

/// One row of the recent-history / timeline view.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TimelineEntry {
    /// The observation event this row is. With `started_at` it forms the paging cursor.
    pub event_id: EventId,
    pub visual_state_id: VisualStateId,
    pub started_at: Timestamp,
    pub ended_at: Timestamp,
    pub application: Option<String>,
    pub window_title: Option<String>,
    pub monitor: Option<String>,
    pub media_path: String,
    pub ocr_status: String,
}

impl TimelineEntry {
    /// The cursor that resumes a newest-first listing just after this entry.
    pub fn cursor(&self) -> TimelineCursor {
        TimelineCursor {
            started_at: self.started_at,
            event_id: self.event_id,
        }
    }
}

/// Position in the newest-first timeline: `(started_at, event_id)` sorts totally even when several
/// monitors share a tick timestamp, so paging by it never skips or repeats an observation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimelineCursor {
    pub started_at: Timestamp,
    pub event_id: EventId,
}

/// Everything known about one visual state, for the detail pane.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VisualDetail {
    pub visual_state_id: VisualStateId,
    pub captured_at: Timestamp,
    pub width: u32,
    pub height: u32,
    pub media_path: String,
    pub application: Option<String>,
    pub window_title: Option<String>,
    pub monitor: Option<String>,
    pub ocr_status: String,
    pub ocr_text: String,
    pub blocks: Vec<OcrBlock>,
}
