//! The multi-frame selection ("picks"), as plain rules: click selects one moment, Shift extends from
//! the anchor to a moment (a contiguous range in timeline order), Ctrl/Cmd toggles one, Esc clears.
//! Ranges are filled from the history (every moment between the two ends, not only the ones
//! loaded), so the app asks for the range's moments and hands them to [`Picks::apply_range`].
//!
//! Distinct from the *stage* selection (the one moment the detail pane shows): picks are what
//! "Copy image", "Save PNG" and the clip exports act on.

use crate::timeline::FrameKey;
use rsrewind_core::{TimelineCursor, TimelineEntry, Timestamp};
use std::collections::{BTreeMap, HashSet};

#[derive(Debug, Clone, Default)]
pub struct Picks {
    /// Where Shift-ranges start: the last moment clicked without Shift.
    anchor: Option<TimelineEntry>,
    items: BTreeMap<TimelineCursor, TimelineEntry>,
    /// The last range held more moments than a selection may.
    pub truncated: bool,
}

impl Picks {
    /// Only this moment; it becomes the anchor.
    pub fn single(&mut self, entry: &TimelineEntry) {
        self.items.clear();
        self.items.insert(entry.cursor(), entry.clone());
        self.anchor = Some(entry.clone());
        self.truncated = false;
    }

    /// Adds or removes one moment; it becomes the anchor either way.
    pub fn toggle(&mut self, entry: &TimelineEntry) {
        let key = entry.cursor();
        if self.items.remove(&key).is_none() {
            self.items.insert(key, entry.clone());
        }
        self.anchor = Some(entry.clone());
    }

    pub fn clear(&mut self) {
        self.items.clear();
        self.anchor = None;
        self.truncated = false;
    }

    /// The time range to fetch for a Shift-extension to `target` (a moment's start), from the
    /// anchor, or from `fallback` (the moment on stage) when nothing is anchored yet. Oldest first.
    pub fn range_to(
        &self,
        target: Timestamp,
        fallback: Option<Timestamp>,
    ) -> Option<(Timestamp, Timestamp)> {
        let from = self.anchor.as_ref().map(|a| a.started_at).or(fallback)?;
        Some((from.min(target), from.max(target)))
    }

    /// Sets the anchor without selecting anything else (used when extending from the stage).
    pub fn anchor_at(&mut self, entry: &TimelineEntry) {
        if self.anchor.is_none() {
            self.anchor = Some(entry.clone());
        }
    }

    /// Replaces the selection with a fetched range (the anchor stays, so extending again from the
    /// same anchor shrinks or grows the range, as in any list).
    pub fn apply_range(&mut self, entries: Vec<TimelineEntry>, truncated: bool) {
        self.items = entries.into_iter().map(|e| (e.cursor(), e)).collect();
        if let Some(anchor) = &self.anchor {
            self.items.insert(anchor.cursor(), anchor.clone());
        }
        self.truncated = truncated;
    }

    /// Every loaded moment (Ctrl+A); the oldest becomes the anchor.
    pub fn all(&mut self, entries: &[TimelineEntry]) {
        self.items = entries.iter().map(|e| (e.cursor(), e.clone())).collect();
        self.anchor = self.items.values().next().cloned();
        self.truncated = false;
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Oldest first.
    pub fn entries(&self) -> impl Iterator<Item = &TimelineEntry> {
        self.items.values()
    }

    pub fn frames(&self) -> HashSet<FrameKey> {
        self.items
            .values()
            .map(|e| (e.source, e.visual_state_id))
            .collect()
    }

    /// From the first moment's start to the last one's end.
    pub fn span(&self) -> Option<(Timestamp, Timestamp)> {
        let first = self.items.values().next()?;
        let last = self.items.values().next_back()?;
        Some((first.started_at, last.ended_at.max(last.started_at)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsrewind_core::{EventId, SourceId, VisualStateId};

    fn e(at: i64, id: i64, source: Option<SourceId>) -> TimelineEntry {
        TimelineEntry {
            event_id: EventId(id),
            visual_state_id: VisualStateId(id),
            started_at: Timestamp(at),
            ended_at: Timestamp(at + 400),
            application: None,
            window_title: None,
            monitor: None,
            media_path: String::new(),
            ocr_status: "done".into(),
            source,
        }
    }

    fn ids(p: &Picks) -> Vec<i64> {
        p.entries().map(|e| e.event_id.0).collect()
    }

    #[test]
    fn click_shift_click_and_toggle_behave_like_a_list() {
        let mut p = Picks::default();
        p.single(&e(3_000, 3, None));
        assert_eq!(ids(&p), [3]);
        // Shift-click an older moment: the range between, both ends included.
        assert_eq!(
            p.range_to(Timestamp(1_000), None),
            Some((Timestamp(1_000), Timestamp(3_000)))
        );
        p.apply_range(
            vec![e(1_000, 1, None), e(2_000, 2, None), e(3_000, 3, None)],
            false,
        );
        assert_eq!(ids(&p), [1, 2, 3]);
        // Extending again from the same anchor replaces the range (shrinks it).
        p.apply_range(vec![e(2_000, 2, None), e(3_000, 3, None)], false);
        assert_eq!(ids(&p), [2, 3]);
        // Ctrl-click removes, then adds back; the toggled one is the new anchor.
        p.toggle(&e(2_000, 2, None));
        assert_eq!(ids(&p), [3]);
        p.toggle(&e(9_000, 9, None));
        assert_eq!(ids(&p), [3, 9]);
        assert_eq!(
            p.range_to(Timestamp(5_000), None),
            Some((Timestamp(5_000), Timestamp(9_000)))
        );
        assert_eq!(p.span(), Some((Timestamp(3_000), Timestamp(9_400))));
        p.clear();
        assert!(p.is_empty() && p.span().is_none());
        assert_eq!(p.range_to(Timestamp(5), None), None, "no anchor, no stage");
        assert_eq!(
            p.range_to(Timestamp(5), Some(Timestamp(50))),
            Some((Timestamp(5), Timestamp(50)))
        );
    }

    #[test]
    fn same_time_on_two_sources_are_two_picks() {
        let other = Some(SourceId::from_bytes([4; 16]));
        let mut p = Picks::default();
        p.all(&[e(1_000, 1, None), e(1_000, 1, other)]);
        assert_eq!(p.len(), 2);
        assert_eq!(p.frames().len(), 2);
        assert_eq!(p.entries().next().map(|e| e.source), Some(None));
    }

    #[test]
    fn a_range_too_long_says_so() {
        let mut p = Picks::default();
        p.single(&e(1, 1, None));
        p.apply_range(vec![e(2, 2, None)], true);
        assert!(p.truncated);
        p.single(&e(3, 3, None));
        assert!(!p.truncated);
    }
}
