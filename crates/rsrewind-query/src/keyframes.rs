//! Keyframes: automatic "interesting moment" stops, derived at read time from what the recorder
//! already stores (no recorder or schema change). See `docs/design/marks-keyframes.md`.
//!
//! A keyframe is an observation where something happened worth stopping at: recording resumed after
//! a stretch with nothing recorded, the focused application or window changed on that monitor, or
//! the screen became a different screen (its stored dHash moved by many bits). Candidates from every
//! source are merged and thinned so a burst (alt-tabbing) becomes one stop.
//!
//! [`derive`] and [`thin`] are pure and unit-tested; [`QueryDb::keyframe_candidates`] feeds them.

use crate::{QueryDb, Result};
use rsrewind_core::{EventId, SourceId, Timestamp, VisualStateId};
use rusqlite::params;
use std::collections::HashMap;

/// A stretch at least this long with nothing recorded makes the next moment a `Resume`.
pub const RESUME_MIN_GAP_MS: i64 = 60_000;
/// dHash bits (of 64) that must differ for a `VisualChange`: a different screen, not an edit.
pub const VISUAL_CHANGE_MIN_BITS: u32 = 24;
/// Within this of a kept stop, only the stronger of two candidates survives.
pub const KEYFRAME_SPACING_MS: i64 = 15_000;
/// Most observations read per source for one range; bounds memory on a dense range.
pub const MAX_KEYFRAME_ROWS: u32 = 20_000;
/// Observations read before a range to know each monitor's previous state.
const CONTEXT_ROWS: u32 = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum KeyframeKind {
    /// The first moment after a stretch with nothing recorded (or the first moment of history).
    Resume,
    /// Another application has focus on this monitor.
    AppSwitch,
    /// Same application, another window.
    WindowSwitch,
    /// The screen became a different screen.
    VisualChange,
}

impl KeyframeKind {
    /// Which candidate wins when two are too close together.
    pub fn priority(self) -> u8 {
        match self {
            Self::Resume => 3,
            Self::AppSwitch => 2,
            Self::WindowSwitch | Self::VisualChange => 1,
        }
    }

    /// Stable name for JSON and logs.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Resume => "resume",
            Self::AppSwitch => "app_switch",
            Self::WindowSwitch => "window_switch",
            Self::VisualChange => "visual_change",
        }
    }

    /// Short words for a person.
    pub fn describe(self) -> &'static str {
        match self {
            Self::Resume => "recording resumed",
            Self::AppSwitch => "switched application",
            Self::WindowSwitch => "switched window",
            Self::VisualChange => "new screen",
        }
    }
}

/// An automatic stop on the timeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Keyframe {
    /// When the observation started: where the camera goes.
    pub at: Timestamp,
    pub kind: KeyframeKind,
    /// Tie-break within a kind: dHash bits changed (0 when unknown).
    pub score: u32,
    pub source: Option<SourceId>,
    pub event_id: EventId,
    pub visual_state_id: VisualStateId,
}

impl Keyframe {
    fn rank(&self) -> (u8, u32) {
        (self.kind.priority(), self.score)
    }
}

/// One observation as derivation sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Obs {
    pub event: i64,
    pub visual_state: i64,
    pub started_at: i64,
    pub monitor: Option<i64>,
    pub application: Option<i64>,
    pub window: Option<i64>,
    /// The stored dHash (`visual_states.fingerprint`, a u64 stored as i64), if any.
    pub fingerprint: Option<i64>,
}

/// Keyframe candidates of one source. `rows` are the range's observations in timeline order;
/// `before` are observations just before the range (any order), giving each monitor its previous
/// state; `resumes` are the ends of recording gaps (Unix ms). Unthinned.
pub(crate) fn derive(
    rows: &[Obs],
    before: &[Obs],
    resumes: &[i64],
    source: Option<SourceId>,
) -> Vec<Keyframe> {
    let mut last: HashMap<Option<i64>, Obs> = HashMap::new();
    for obs in before {
        let newer = last
            .get(&obs.monitor)
            .is_none_or(|p| (obs.started_at, obs.event) > (p.started_at, p.event));
        if newer {
            last.insert(obs.monitor, *obs);
        }
    }
    // Recording resumes at the first moment that starts at or after each gap's end.
    let resumed: std::collections::HashSet<i64> = resumes
        .iter()
        .filter_map(|&end| rows.iter().find(|r| r.started_at >= end).map(|r| r.event))
        .collect();

    let mut out = Vec::new();
    for row in rows {
        let previous = last.insert(row.monitor, *row);
        let bits = previous.and_then(|p| match (p.fingerprint, row.fingerprint) {
            (Some(a), Some(b)) if p.visual_state != row.visual_state => Some((a ^ b).count_ones()),
            _ => None,
        });
        let kind = if resumed.contains(&row.event) {
            Some(KeyframeKind::Resume)
        } else if let Some(p) = previous {
            if row.application.is_some() && row.application != p.application {
                Some(KeyframeKind::AppSwitch)
            } else if row.window.is_some() && row.window != p.window {
                Some(KeyframeKind::WindowSwitch)
            } else if bits.is_some_and(|b| b >= VISUAL_CHANGE_MIN_BITS) {
                Some(KeyframeKind::VisualChange)
            } else {
                None
            }
        } else {
            None
        };
        if let Some(kind) = kind {
            out.push(Keyframe {
                at: Timestamp(row.started_at),
                kind,
                score: bits.unwrap_or(0),
                source,
                event_id: EventId(row.event),
                visual_state_id: VisualStateId(row.visual_state),
            });
        }
    }
    out
}

/// Merges candidates (from any number of sources) into stops at least `spacing_ms` apart in
/// practice: a candidate within `spacing_ms` of the last kept stop replaces it only if it ranks
/// higher (priority, then score); on a tie the earlier one stays. Oldest first.
pub(crate) fn thin(mut candidates: Vec<Keyframe>, spacing_ms: i64) -> Vec<Keyframe> {
    candidates.sort_by_key(|k| (k.at, k.source, k.event_id));
    let mut out: Vec<Keyframe> = Vec::with_capacity(candidates.len());
    for candidate in candidates {
        match out.last_mut() {
            Some(kept) if candidate.at.0 - kept.at.0 < spacing_ms => {
                if candidate.rank() > kept.rank() {
                    *kept = candidate;
                }
            }
            _ => out.push(candidate),
        }
    }
    out
}

const OBS_SELECT: &str = "
    SELECT e.id, e.visual_state_id, e.started_at, COALESCE(e.monitor_id, v.monitor_id),
           e.application_id, e.window_id, v.fingerprint
    FROM events e JOIN visual_states v ON v.id = e.visual_state_id";

fn obs(row: &rusqlite::Row<'_>) -> rusqlite::Result<Obs> {
    Ok(Obs {
        event: row.get(0)?,
        visual_state: row.get(1)?,
        started_at: row.get(2)?,
        monitor: row.get(3)?,
        application: row.get(4)?,
        window: row.get(5)?,
        fingerprint: row.get(6)?,
    })
}

impl QueryDb {
    /// Unthinned keyframe candidates of this store for observations starting in `[from, to]`.
    pub(crate) fn keyframe_candidates(
        &self,
        from: Timestamp,
        to: Timestamp,
    ) -> Result<Vec<Keyframe>> {
        if to < from {
            return Ok(Vec::new());
        }
        let rows: Vec<Obs> = {
            let mut stmt = self.conn.prepare(&format!(
                "{OBS_SELECT}
                 WHERE e.kind = 'observation' AND e.started_at >= ?1 AND e.started_at <= ?2
                 ORDER BY e.started_at, e.id LIMIT ?3"
            ))?;
            stmt.query_map(params![from.0, to.0, MAX_KEYFRAME_ROWS], obs)?
                .collect::<rusqlite::Result<_>>()?
        };
        if rows.is_empty() {
            return Ok(Vec::new());
        }
        let before: Vec<Obs> = {
            let mut stmt = self.conn.prepare(&format!(
                "{OBS_SELECT}
                 WHERE e.kind = 'observation' AND e.started_at < ?1
                 ORDER BY e.started_at DESC, e.id DESC LIMIT ?2"
            ))?;
            stmt.query_map(params![from.0, CONTEXT_ROWS], obs)?
                .collect::<rusqlite::Result<_>>()?
        };
        let resumes: Vec<i64> = self
            .gaps(from, Timestamp(to.0.saturating_add(1)), RESUME_MIN_GAP_MS)?
            .into_iter()
            .map(|g| g.to.0)
            .collect();
        Ok(derive(&rows, &before, &resumes, self.source))
    }

    /// Start of the first and of the last observation, if any (index-only lookups).
    pub(crate) fn observation_bounds(&self) -> Result<Option<(i64, i64)>> {
        let first: Option<i64> = self.conn.query_row(
            "SELECT MIN(started_at) FROM events WHERE kind = 'observation'",
            [],
            |row| row.get(0),
        )?;
        let last: Option<i64> = self.conn.query_row(
            "SELECT MAX(started_at) FROM events WHERE kind = 'observation'",
            [],
            |row| row.get(0),
        )?;
        Ok(first.zip(last))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const T: i64 = 1_790_000_000_000;

    fn o(event: i64, at: i64, monitor: i64, app: i64, window: i64, fp: Option<i64>) -> Obs {
        Obs {
            event,
            visual_state: event,
            started_at: T + at,
            monitor: Some(monitor),
            application: Some(app),
            window: Some(window),
            fingerprint: fp,
        }
    }

    fn kinds(keys: &[Keyframe]) -> Vec<(i64, KeyframeKind)> {
        keys.iter().map(|k| (k.event_id.0, k.kind)).collect()
    }

    #[test]
    fn switches_are_judged_per_monitor() {
        // Two monitors, each steady in its own application: alternating rows are not switches.
        let rows = [
            o(1, 0, 1, 10, 100, None),
            o(2, 1_000, 2, 20, 200, None),
            o(3, 2_000, 1, 10, 100, None),
            o(4, 3_000, 2, 20, 200, None),
            o(5, 4_000, 1, 30, 300, None), // monitor 1 switches app
            o(6, 5_000, 2, 20, 201, None), // monitor 2 switches window
        ];
        let keys = derive(&rows, &[], &[], None);
        assert_eq!(
            kinds(&keys),
            [
                (5, KeyframeKind::AppSwitch),
                (6, KeyframeKind::WindowSwitch)
            ]
        );
    }

    #[test]
    fn the_monitor_before_the_range_is_the_comparison() {
        let before = [
            o(1, -9_000, 1, 10, 100, None),
            o(2, -5_000, 1, 11, 110, None),
        ];
        // Same app as the newest earlier row (11): no switch. Older row (10) must not be used.
        let keys = derive(&[o(3, 0, 1, 11, 110, None)], &before, &[], None);
        assert!(keys.is_empty(), "{keys:?}");
        // Without context, the first row of a monitor is no switch either.
        assert!(derive(&[o(3, 0, 1, 11, 110, None)], &[], &[], None).is_empty());
    }

    #[test]
    fn a_different_screen_is_a_visual_change_and_an_edit_is_not() {
        let a = 0x0f0f_0f0f_0f0f_0f0f_i64;
        let edit = a ^ 0b111; // 3 bits
        let other = !a; // 64 bits
        let rows = [
            o(1, 0, 1, 10, 100, Some(a)),
            o(2, 1_000, 1, 10, 100, Some(edit)),
            o(3, 2_000, 1, 10, 100, Some(other)),
            o(4, 3_000, 1, 10, 100, None),
        ];
        let keys = derive(&rows, &[], &[], None);
        assert_eq!(kinds(&keys), [(3, KeyframeKind::VisualChange)]);
        assert_eq!(keys[0].score, (edit ^ other).count_ones());
    }

    #[test]
    fn the_first_moment_after_a_gap_is_a_resume_whatever_else_it_is() {
        let rows = [
            o(1, 0, 1, 10, 100, None),
            o(2, 120_000, 1, 20, 200, None), // app switch, but after a gap
            o(3, 121_000, 1, 20, 200, None),
        ];
        let keys = derive(&rows, &[], &[100_000 + T], None);
        assert_eq!(kinds(&keys), [(2, KeyframeKind::Resume)]);
        // A gap that ends after the last row resumes nothing in range.
        assert!(derive(&rows[..1], &[], &[T + 50_000], None).is_empty());
    }

    #[test]
    fn thinning_keeps_the_strongest_stop_of_a_burst() {
        let k = |at: i64, kind: KeyframeKind, score: u32, event: i64| Keyframe {
            at: Timestamp(T + at),
            kind,
            score,
            source: None,
            event_id: EventId(event),
            visual_state_id: VisualStateId(event),
        };
        let burst = vec![
            k(0, KeyframeKind::WindowSwitch, 0, 1),
            k(2_000, KeyframeKind::AppSwitch, 0, 2),
            k(4_000, KeyframeKind::VisualChange, 40, 3),
            k(30_000, KeyframeKind::VisualChange, 30, 4),
            k(31_000, KeyframeKind::VisualChange, 50, 5),
            k(60_000, KeyframeKind::WindowSwitch, 0, 6),
            k(61_000, KeyframeKind::WindowSwitch, 0, 7),
        ];
        let thinned = thin(burst, KEYFRAME_SPACING_MS);
        assert_eq!(
            kinds(&thinned),
            [
                (2, KeyframeKind::AppSwitch),
                (5, KeyframeKind::VisualChange),
                (6, KeyframeKind::WindowSwitch),
            ]
        );
        // Unsorted input from several sources comes out in time order.
        let mixed = vec![
            k(90_000, KeyframeKind::Resume, 0, 9),
            k(0, KeyframeKind::Resume, 0, 8),
        ];
        let out = thin(mixed, KEYFRAME_SPACING_MS);
        assert!(out[0].at < out[1].at);
        assert!(thin(Vec::new(), KEYFRAME_SPACING_MS).is_empty());
    }
}
