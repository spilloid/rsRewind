//! Query-facing types shared by the query layer, the CLI (`--json`) and the UI.
//!
//! These serialize to the CLI's JSON output, which is an integration surface for scripts and
//! agents: add fields freely, but do not rename or remove them without a version bump.

use crate::{EventId, OcrBlock, SourceId, Timestamp, VisualStateId};
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;

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
    /// Which installation recorded this: absent (`None`) for this machine's own history, the
    /// source id for history imported from another rsRewind. Omitted from JSON when absent, so
    /// output for a local store is unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<SourceId>,
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
    /// Which installation recorded this: absent (`None`) for this machine's own history, the
    /// source id for history imported from another rsRewind. Omitted from JSON when absent, so
    /// output for a local store is unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<SourceId>,
}

impl TimelineEntry {
    /// The cursor that resumes a newest-first listing just after this entry.
    pub fn cursor(&self) -> TimelineCursor {
        TimelineCursor {
            started_at: self.started_at,
            event_id: self.event_id,
            source: self.source,
        }
    }
}

/// Position in the newest-first timeline.
///
/// Within one store `(started_at, event_id)` sorts totally even when several monitors share a tick
/// timestamp, so paging by it never skips or repeats an observation. Event ids are only unique
/// per store, so across sources the order is `(started_at, source, event_id)` ([`Ord`] below,
/// oldest first; `None`, this machine, sorts before every remote source). A single-store query
/// ignores `source`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimelineCursor {
    pub started_at: Timestamp,
    pub event_id: EventId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<SourceId>,
}

impl TimelineCursor {
    /// A cursor that sorts after every observation that started at or before `at`, on every
    /// source: resuming a newest-first listing from it starts at `at`.
    pub fn at_or_before(at: Timestamp) -> Self {
        Self {
            started_at: at,
            event_id: EventId(i64::MAX),
            source: Some(SourceId::from_bytes([0xff; 16])),
        }
    }
}

impl Ord for TimelineCursor {
    fn cmp(&self, other: &Self) -> Ordering {
        self.started_at
            .cmp(&other.started_at)
            .then_with(|| self.source.cmp(&other.source))
            .then_with(|| self.event_id.cmp(&other.event_id))
    }
}

impl PartialOrd for TimelineCursor {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
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
    /// Which installation recorded this: absent (`None`) for this machine's own history, the
    /// source id for history imported from another rsRewind. Omitted from JSON when absent, so
    /// output for a local store is unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<SourceId>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(source: Option<SourceId>) -> TimelineEntry {
        TimelineEntry {
            event_id: EventId(7),
            visual_state_id: VisualStateId(3),
            started_at: Timestamp(1_000),
            ended_at: Timestamp(2_000),
            application: Some("app.exe".into()),
            window_title: None,
            monitor: None,
            media_path: "x".into(),
            ocr_status: "done".into(),
            source,
        }
    }

    #[test]
    fn local_results_serialize_exactly_as_before() -> Result<(), Box<dyn std::error::Error>> {
        let json = serde_json::to_value(entry(None))?;
        let keys: Vec<&str> = json
            .as_object()
            .ok_or("object")?
            .keys()
            .map(String::as_str)
            .collect();
        assert!(!keys.contains(&"source"), "{keys:?}");
        // Older JSON (no `source`) still deserializes.
        let back: TimelineEntry = serde_json::from_value(json)?;
        assert_eq!(back, entry(None));

        let id = SourceId::parse("0123456789abcdef0123456789abcdef").ok_or("id")?;
        let remote = serde_json::to_value(entry(Some(id)))?;
        assert_eq!(remote["source"], "0123456789abcdef0123456789abcdef");
        assert_eq!(remote["event_id"], 7);
        Ok(())
    }

    #[test]
    fn cursors_order_by_time_then_source_then_event() -> Result<(), Box<dyn std::error::Error>> {
        let a = SourceId::parse("0aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa").ok_or("a")?;
        let b = SourceId::parse("0bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb").ok_or("b")?;
        let c = |t: i64, s: Option<SourceId>, e: i64| TimelineCursor {
            started_at: Timestamp(t),
            event_id: EventId(e),
            source: s,
        };
        let mut cursors = vec![
            c(2, None, 1),
            c(1, Some(b), 1),
            c(1, Some(a), 9),
            c(1, None, 5),
            c(1, Some(a), 2),
        ];
        cursors.sort();
        assert_eq!(
            cursors,
            vec![
                c(1, None, 5),
                c(1, Some(a), 2),
                c(1, Some(a), 9),
                c(1, Some(b), 1),
                c(2, None, 1)
            ]
        );
        // `at_or_before(t)` is above everything that started at t and below anything later.
        let top = TimelineCursor::at_or_before(Timestamp(1));
        assert!(cursors[..4].iter().all(|x| *x < top));
        assert!(cursors[4] > top);
        Ok(())
    }
}
