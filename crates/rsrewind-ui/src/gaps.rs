//! Gaps in the window: which gaps the camera is in, which a step jumped over, and how to say so.
//! Pure, so the rules are unit-tested; the app only draws the strings.

use rsrewind_core::{Gap, GapReason};
use rsrewind_query::SourceFilter;

/// Below this, a gap is not worth telling the user about (matches `rsrewind recent`).
pub const MIN_GAP_MS: i64 = 60_000;

/// Gaps (of admitted sources) that contain the instant `t`.
pub fn at(gaps: &[Gap], filter: SourceFilter, t: i64) -> Vec<&Gap> {
    gaps.iter()
        .filter(|g| filter.admits(g.source) && g.from.0 <= t && t < g.to.0)
        .collect()
}

/// Gaps (of admitted sources) lying between two instants, in either order: what a jump from
/// `a` to `b` skipped over.
pub fn crossed(gaps: &[Gap], filter: SourceFilter, a: i64, b: i64) -> Vec<&Gap> {
    let (lo, hi) = (a.min(b), a.max(b));
    gaps.iter()
        .filter(|g| filter.admits(g.source) && g.from.0 < hi && g.to.0 > lo)
        .collect()
}

/// `2h 06m`, `9m`, `45s`, `3d 5h`.
pub fn duration(millis: i64) -> String {
    let secs = millis.max(0) / 1000;
    let (d, h, m, s) = (secs / 86_400, secs / 3600 % 24, secs / 60 % 60, secs % 60);
    match (d, h, m) {
        (0, 0, 0) => format!("{s}s"),
        (0, 0, _) => format!("{m}m"),
        (0, _, _) => format!("{h}h {m:02}m"),
        _ => format!("{d}d {h}h"),
    }
}

/// "Not recorded for 9m: idle or locked". The caller adds the machine and the times.
pub fn sentence(gap: &Gap) -> String {
    format!(
        "Not recorded for {}: {}",
        duration(gap.millis()),
        gap.reason.describe()
    )
}

/// Whether a reason deserves the warning colour (something went wrong, not a choice).
pub fn is_warning(reason: GapReason) -> bool {
    matches!(reason, GapReason::RecorderDied | GapReason::NotStored)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsrewind_core::{SourceId, Timestamp};

    fn gap(from: i64, to: i64, reason: GapReason, source: Option<SourceId>) -> Gap {
        Gap {
            from: Timestamp(from),
            to: Timestamp(to),
            reason,
            source,
        }
    }

    #[test]
    fn the_camera_inside_a_gap_finds_it_and_only_for_admitted_sources() {
        let remote = SourceId::from_bytes([7; 16]);
        let gaps = [
            gap(100, 200, GapReason::Idle, None),
            gap(150, 400, GapReason::RecorderOff, Some(remote)),
        ];
        assert_eq!(at(&gaps, SourceFilter::All, 160).len(), 2);
        assert_eq!(at(&gaps, SourceFilter::Local, 160).len(), 1);
        assert_eq!(
            at(&gaps, SourceFilter::Remote(remote), 160)[0].reason,
            GapReason::RecorderOff
        );
        // Half-open: the gap ends where recording resumes.
        assert!(at(&gaps, SourceFilter::Local, 200).is_empty());
        assert_eq!(at(&gaps, SourceFilter::Local, 100).len(), 1);
    }

    #[test]
    fn a_step_reports_what_it_jumped_over_in_either_direction() {
        let gaps = [
            gap(100, 200, GapReason::Idle, None),
            gap(500, 900, GapReason::Paused, None),
        ];
        assert_eq!(crossed(&gaps, SourceFilter::All, 50, 300).len(), 1);
        assert_eq!(crossed(&gaps, SourceFilter::All, 300, 50).len(), 1);
        assert_eq!(crossed(&gaps, SourceFilter::All, 50, 1_000).len(), 2);
        // Touching the edge of a gap is not crossing it.
        assert!(crossed(&gaps, SourceFilter::All, 200, 500).is_empty());
    }

    #[test]
    fn sentences_say_how_long_and_why() {
        let g = gap(0, 2 * 3_600_000 + 6 * 60_000, GapReason::RecorderOff, None);
        assert_eq!(sentence(&g), "Not recorded for 2h 06m: recorder off");
        assert!(is_warning(GapReason::RecorderDied));
        assert!(!is_warning(GapReason::Idle));
        assert_eq!(duration(9 * 60_000 + 1_000), "9m");
    }
}
