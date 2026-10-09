//! Finding gaps and explaining them. Pure: works on plain millisecond numbers read by
//! [`crate::QueryDb::gaps`], so every rule here is unit-tested without a database.
//!
//! 1. A *hole* is time inside the asked range that no observation covers on any monitor (the
//!    union of all observations), at least `min_gap` long.
//! 2. Each hole is cut at every session start/end and pause/idle marker inside it, and each piece
//!    gets the state the recorder was in at its start. Adjacent pieces with the same reason merge.
//!
//! State at an instant `t`:
//! - inside a session whose recorder was running: paused > idle > "running, nothing stored";
//! - outside every session: the recorder was off, unless the latest session before `t` never wrote
//!   its end, in which case it died at the last thing it wrote (its `last_seen`).

use rsrewind_core::GapReason;

/// One recording session, in milliseconds. `end` is `None` when the recorder never wrote its stop
/// (still running, or it died). `last_seen` is the end of the last event it wrote.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SessionSpan {
    pub id: i64,
    pub start: i64,
    pub end: Option<i64>,
    pub last_seen: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mark {
    Paused,
    Resumed,
    IdleStart,
    IdleEnd,
}

impl Mark {
    pub fn parse(kind: &str) -> Option<Self> {
        match kind {
            "paused" => Some(Self::Paused),
            "resumed" => Some(Self::Resumed),
            "idle_start" => Some(Self::IdleStart),
            "idle_end" => Some(Self::IdleEnd),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Marker {
    pub session: i64,
    pub at: i64,
    pub mark: Mark,
}

/// `(from, to, reason)`, oldest first. `covered` are observation `(started_at, ended_at)` pairs in
/// any order; `sessions` and `markers` in any order.
pub(crate) fn classify(
    range: (i64, i64),
    covered: &[(i64, i64)],
    sessions: &[SessionSpan],
    markers: &[Marker],
    min_gap: i64,
) -> Vec<(i64, i64, GapReason)> {
    let mut sessions = sessions.to_vec();
    sessions.sort_by_key(|s| (s.start, s.id));
    let mut markers = markers.to_vec();
    markers.sort_by_key(|m| m.at);

    let mut gaps: Vec<(i64, i64, GapReason)> = Vec::new();
    for (h0, h1) in holes(range, covered, min_gap) {
        let mut cuts = vec![h0, h1];
        for (i, s) in sessions.iter().enumerate() {
            cuts.push(s.start);
            cuts.push(effective_end(&sessions, i));
        }
        cuts.extend(markers.iter().map(|m| m.at));
        cuts.retain(|&c| c >= h0 && c <= h1);
        cuts.sort_unstable();
        cuts.dedup();
        let mut pieces: Vec<(i64, i64, GapReason)> = Vec::new();
        for pair in cuts.windows(2) {
            let (p, q) = (pair[0], pair[1]);
            push_merged(&mut pieces, (p, q, reason_at(p, &sessions, &markers)));
        }
        gaps.extend(absorb_short(pieces, min_gap));
    }
    gaps
}

fn push_merged(pieces: &mut Vec<(i64, i64, GapReason)>, piece: (i64, i64, GapReason)) {
    match pieces.last_mut() {
        Some(last) if last.1 == piece.0 && last.2 == piece.2 => last.1 = piece.1,
        _ => pieces.push(piece),
    }
}

/// Inside one hole, a piece shorter than `min_gap` (the seconds between the last capture and the
/// idle marker, say) is not worth its own line: it takes the reason of its longer neighbour.
fn absorb_short(
    mut pieces: Vec<(i64, i64, GapReason)>,
    min_gap: i64,
) -> Vec<(i64, i64, GapReason)> {
    let len = |p: &(i64, i64, GapReason)| p.1 - p.0;
    while pieces.len() > 1 {
        let Some(i) = (0..pieces.len()).find(|&i| len(&pieces[i]) < min_gap) else {
            break;
        };
        let into = match (i.checked_sub(1), pieces.get(i + 1)) {
            (Some(prev), Some(next)) if len(next) > len(&pieces[prev]) => i + 1,
            (Some(prev), _) => prev,
            (None, _) => i + 1,
        };
        pieces[i].2 = pieces[into].2;
        let mut merged = Vec::with_capacity(pieces.len());
        for piece in pieces {
            push_merged(&mut merged, piece);
        }
        pieces = merged;
    }
    pieces
}

/// Uncovered stretches of `range`, each at least `min_gap` long.
fn holes(range: (i64, i64), covered: &[(i64, i64)], min_gap: i64) -> Vec<(i64, i64)> {
    let (from, to) = range;
    let mut spans: Vec<(i64, i64)> = covered
        .iter()
        .filter(|(s, e)| *e >= from && *s <= to)
        .copied()
        .collect();
    spans.sort_unstable();
    let mut out = Vec::new();
    let mut cursor = from;
    for (s, e) in spans {
        if s > cursor && s - cursor >= min_gap {
            out.push((cursor, s));
        }
        cursor = cursor.max(e);
    }
    if to > cursor && to - cursor >= min_gap {
        out.push((cursor, to));
    }
    out
}

/// Where session `i` stopped recording: its written end, else (if a later session started, so it
/// is certainly over) the last thing it wrote, else still running.
fn effective_end(sessions: &[SessionSpan], i: usize) -> i64 {
    let s = sessions[i];
    match s.end {
        Some(end) => end,
        None if i + 1 < sessions.len() => s.last_seen.min(sessions[i + 1].start).max(s.start),
        None => i64::MAX,
    }
}

fn reason_at(t: i64, sessions: &[SessionSpan], markers: &[Marker]) -> GapReason {
    let active = (0..sessions.len())
        .rev()
        .find(|&i| sessions[i].start <= t && t < effective_end(sessions, i));
    let Some(i) = active else {
        let previous = sessions.iter().rev().find(|s| s.start <= t);
        return match previous {
            Some(s) if s.end.is_none() => GapReason::RecorderDied,
            _ => GapReason::RecorderOff,
        };
    };
    let session = sessions[i].id;
    let (mut paused, mut idle) = (false, false);
    for m in markers.iter().filter(|m| m.session == session && m.at <= t) {
        match m.mark {
            Mark::Paused => paused = true,
            Mark::Resumed => paused = false,
            Mark::IdleStart => idle = true,
            Mark::IdleEnd => idle = false,
        }
    }
    if paused {
        GapReason::Paused
    } else if idle {
        GapReason::Idle
    } else {
        GapReason::NotStored
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use GapReason::*;

    const MIN: i64 = 30_000;

    fn session(id: i64, start: i64, end: Option<i64>, last_seen: i64) -> SessionSpan {
        SessionSpan {
            id,
            start,
            end,
            last_seen,
        }
    }

    fn mark(session: i64, at: i64, mark: Mark) -> Marker {
        Marker { session, at, mark }
    }

    #[test]
    fn a_static_screen_is_not_a_gap() {
        // One observation extended over an hour: nothing missing.
        let gaps = classify(
            (0, 3_600_000),
            &[(0, 3_600_000)],
            &[session(1, 0, None, 3_600_000)],
            &[],
            MIN,
        );
        assert!(gaps.is_empty());
    }

    #[test]
    fn short_holes_are_ignored_and_monitors_are_unioned() {
        // Two monitors overlap; the 10 s hole between them is below the minimum.
        let covered = [(0, 50_000), (40_000, 100_000), (110_000, 200_000)];
        let gaps = classify(
            (0, 200_000),
            &covered,
            &[session(1, 0, None, 200_000)],
            &[],
            MIN,
        );
        assert!(gaps.is_empty(), "{gaps:?}");
    }

    #[test]
    fn stop_then_start_is_recorder_off() {
        let sessions = [
            session(1, 0, Some(100_000), 100_000),
            session(2, 500_000, None, 900_000),
        ];
        let covered = [(0, 100_000), (500_000, 900_000)];
        let gaps = classify((0, 900_000), &covered, &sessions, &[], MIN);
        assert_eq!(gaps, vec![(100_000, 500_000, RecorderOff)]);
    }

    #[test]
    fn a_session_without_an_end_died_at_its_last_write() {
        let sessions = [
            session(1, 0, None, 120_000),
            session(2, 600_000, None, 900_000),
        ];
        let covered = [(0, 100_000), (600_000, 900_000)];
        let gaps = classify((0, 900_000), &covered, &sessions, &[], MIN);
        // It died at its last write (120 s); the 20 s before that are too short to stand alone.
        assert_eq!(gaps, vec![(100_000, 600_000, RecorderDied)]);
        // With a longer silence before the death, both are reported.
        let sessions = [
            session(1, 0, None, 300_000),
            session(2, 600_000, None, 900_000),
        ];
        let gaps = classify((0, 900_000), &covered, &sessions, &[], MIN);
        assert_eq!(
            gaps,
            vec![
                (100_000, 300_000, NotStored),
                (300_000, 600_000, RecorderDied)
            ]
        );
    }

    #[test]
    fn idle_then_pause_then_off_is_split_by_reason() {
        let sessions = [
            session(1, 0, Some(1_000_000), 1_000_000),
            session(2, 2_000_000, None, 2_500_000),
        ];
        let markers = [
            mark(1, 100_000, Mark::IdleStart),
            mark(1, 400_000, Mark::IdleEnd),
            mark(1, 400_000, Mark::Paused),
        ];
        let covered = [(0, 100_000), (2_000_000, 2_500_000)];
        let gaps = classify((0, 2_500_000), &covered, &sessions, &markers, MIN);
        assert_eq!(
            gaps,
            vec![
                (100_000, 400_000, Idle),
                (400_000, 1_000_000, Paused),
                (1_000_000, 2_000_000, RecorderOff),
            ]
        );
    }

    #[test]
    fn markers_of_another_session_do_not_leak() {
        // Session 1 ended paused and never resumed; session 2 starts fresh.
        let sessions = [
            session(1, 0, Some(100_000), 100_000),
            session(2, 200_000, None, 500_000),
        ];
        let markers = [mark(1, 50_000, Mark::Paused)];
        let covered = [(0, 50_000), (400_000, 500_000)];
        let gaps = classify((0, 500_000), &covered, &sessions, &markers, MIN);
        assert_eq!(
            gaps,
            vec![
                (50_000, 100_000, Paused),
                (100_000, 200_000, RecorderOff),
                (200_000, 400_000, NotStored),
            ]
        );
    }

    #[test]
    fn state_before_the_range_carries_into_it() {
        let sessions = [session(1, 0, None, 1_000_000)];
        let markers = [mark(1, 10_000, Mark::IdleStart)];
        let gaps = classify(
            (500_000, 1_000_000),
            &[(0, 10_000), (900_000, 1_000_000)],
            &sessions,
            &markers,
            MIN,
        );
        assert_eq!(gaps, vec![(500_000, 900_000, Idle)]);
    }

    #[test]
    fn a_static_screen_covers_while_a_busy_one_starts_and_stops() {
        // Monitor 1 static for 10 min; monitor 2 has a short observation inside that span. The
        // static screen still covers everything: no gap after monitor 2's short one ends.
        let covered = [(0, 600_000), (100_000, 110_000)];
        let gaps = classify(
            (0, 600_000),
            &covered,
            &[session(1, 0, None, 600_000)],
            &[],
            MIN,
        );
        assert!(gaps.is_empty(), "{gaps:?}");
    }

    #[test]
    fn a_cut_that_does_not_change_the_reason_leaves_one_gap() {
        // A resume while not paused, inside an idle stretch: still one idle gap.
        let sessions = [session(1, 0, None, 1_000_000)];
        let markers = [
            mark(1, 100_000, Mark::IdleStart),
            mark(1, 300_000, Mark::Resumed),
        ];
        let gaps = classify(
            (0, 1_000_000),
            &[(0, 100_000), (900_000, 1_000_000)],
            &sessions,
            &markers,
            MIN,
        );
        assert_eq!(gaps, vec![(100_000, 900_000, Idle)]);
    }

    #[test]
    fn seconds_before_an_idle_marker_join_the_idle_gap() {
        // As recorded on 2026-10-08: last capture 22:14:30, idle marker 22:14:36, input 22:23:37.
        let sessions = [session(1, 0, None, 2_000_000)];
        let markers = [
            mark(1, 1_006_000, Mark::IdleStart),
            mark(1, 1_547_000, Mark::IdleEnd),
        ];
        let covered = [(0, 1_000_000), (1_547_000, 2_000_000)];
        let gaps = classify((0, 2_000_000), &covered, &sessions, &markers, MIN);
        assert_eq!(gaps, vec![(1_000_000, 1_547_000, Idle)]);
    }

    #[test]
    fn a_short_piece_joins_its_longer_neighbour() {
        // Off for 40 s, then a session that stores nothing for 10 s, then idle for 300 s.
        let sessions = [
            session(1, 0, Some(100_000), 100_000),
            session(2, 140_000, None, 1_000_000),
        ];
        let markers = [
            mark(2, 150_000, Mark::IdleStart),
            mark(2, 450_000, Mark::IdleEnd),
        ];
        let covered = [(0, 100_000), (450_000, 1_000_000)];
        let gaps = classify((0, 1_000_000), &covered, &sessions, &markers, MIN);
        assert_eq!(
            gaps,
            vec![(100_000, 140_000, RecorderOff), (140_000, 450_000, Idle)]
        );
    }

    #[test]
    fn nothing_recorded_ever_is_recorder_off() {
        assert_eq!(
            classify((0, 100_000), &[], &[], &[], MIN),
            vec![(0, 100_000, RecorderOff)]
        );
    }
}
