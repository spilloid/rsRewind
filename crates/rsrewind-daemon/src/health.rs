//! Whether a recording recorder is actually *saving* anything, and if not, why.
//!
//! "Recording" only says the recorder runs and is not paused. Before issue #33 a recorder whose
//! window list was unknown on every tick showed "Recording" for hours while storing nothing, and
//! said so nowhere above debug level. [`Health`] is that missing signal: the capture loop
//! classifies each recording tick, [`HealthTracker`] turns those into transitions (debounced, so
//! one odd tick does not flap), and the current value is published in the heartbeat's counters
//! (`health`, `health_since_ms`) where `rsrewind status`, `doctor`, the window and the tray read
//! it. No schema change: the heartbeat row already stores a name→number map.
//!
//! Health names a *condition*, never content: no titles, no process names, no rule names.

use rsrewind_core::Timestamp;

/// Why a recording recorder is not saving, or [`Health::Saving`]. The numeric code is what the
/// heartbeat stores; codes are append-only (a newer recorder may publish one an older CLI does
/// not know, which reads as [`Health::Other`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Health {
    /// Storing or extending moments (or nothing has been classified yet).
    Saving,
    /// The window list is unknown, so every monitor is skipped (unknown means do not record).
    WindowListUnknown,
    /// Idle time / session state is unknown (locked, not on the console, ...), so nothing is
    /// stored.
    IdleUnknown,
    /// Every monitor's capture is failing.
    CaptureFailing,
    /// No displays are known.
    NoDisplays,
    /// The user is idle (no input for longer than `capture.idle_after_secs`). Expected.
    Idle,
    /// Every monitor shows a window an exclusion rule matches. Expected.
    PrivacyExcluded,
    /// A code this build does not know.
    Other(u64),
}

impl Health {
    pub fn code(self) -> u64 {
        match self {
            Self::Saving => 0,
            Self::WindowListUnknown => 1,
            Self::IdleUnknown => 2,
            Self::CaptureFailing => 3,
            Self::NoDisplays => 4,
            Self::Idle => 5,
            Self::PrivacyExcluded => 6,
            Self::Other(code) => code,
        }
    }

    pub fn from_code(code: u64) -> Self {
        match code {
            0 => Self::Saving,
            1 => Self::WindowListUnknown,
            2 => Self::IdleUnknown,
            3 => Self::CaptureFailing,
            4 => Self::NoDisplays,
            5 => Self::Idle,
            6 => Self::PrivacyExcluded,
            other => Self::Other(other),
        }
    }

    /// Stable identifier for JSON output (`rsrewind status --json`'s `not_saving.reason`).
    pub fn name(self) -> &'static str {
        match self {
            Self::Saving => "saving",
            Self::WindowListUnknown => "window_context_unavailable",
            Self::IdleUnknown => "session_state_unknown",
            Self::CaptureFailing => "capture_failing",
            Self::NoDisplays => "no_displays",
            Self::Idle => "idle",
            Self::PrivacyExcluded => "privacy_excluded",
            Self::Other(_) => "unknown",
        }
    }

    /// A short phrase for "Recording · nothing saved: <phrase>".
    pub fn phrase(self) -> &'static str {
        match self {
            Self::Saving => "saving",
            Self::WindowListUnknown => "window context unavailable",
            Self::IdleUnknown => "session state unknown (locked or switched away?)",
            Self::CaptureFailing => "screen capture is failing",
            Self::NoDisplays => "no displays found",
            Self::Idle => "you are idle",
            Self::PrivacyExcluded => "an excluded window is on every screen",
            Self::Other(_) => "unrecognised condition",
        }
    }

    /// A problem the user should act on, as opposed to an expected pause in saving.
    pub fn is_fault(self) -> bool {
        !matches!(self, Self::Saving | Self::Idle | Self::PrivacyExcluded)
    }
}

/// Counter keys the heartbeat carries health under.
pub const HEALTH_KEY: &str = "health";
pub const HEALTH_SINCE_KEY: &str = "health_since_ms";

/// Reads the health a heartbeat published: `None` from a recorder that predates it.
pub fn from_counters(
    counters: &std::collections::BTreeMap<String, u64>,
) -> Option<(Health, Option<Timestamp>)> {
    let health = Health::from_code(*counters.get(HEALTH_KEY)?);
    let since = counters
        .get(HEALTH_SINCE_KEY)
        .and_then(|ms| i64::try_from(*ms).ok())
        .filter(|ms| *ms > 0)
        .map(Timestamp);
    Some((health, since))
}

/// What one recording tick looked like, for [`classify`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TickFacts {
    pub monitors: usize,
    pub window_list_unknown: bool,
    pub idle_unknown: bool,
    pub idle: bool,
    /// Monitors that queued a new state or extended the current one.
    pub saving: usize,
    /// Monitors whose capture failed this tick.
    pub capture_failed: usize,
    /// Monitors skipped because a configured exclusion rule matched.
    pub rule_excluded: usize,
}

/// The health one recording tick shows. A tick that saved anything is [`Health::Saving`]; else the
/// most fundamental reason wins.
pub fn classify(t: &TickFacts) -> Health {
    if t.monitors == 0 {
        return Health::NoDisplays;
    }
    if t.saving > 0 {
        return Health::Saving;
    }
    if t.window_list_unknown {
        return Health::WindowListUnknown;
    }
    if t.idle_unknown {
        return Health::IdleUnknown;
    }
    if t.capture_failed == t.monitors {
        return Health::CaptureFailing;
    }
    if t.rule_excluded == t.monitors {
        return Health::PrivacyExcluded;
    }
    if t.idle {
        return Health::Idle;
    }
    // Waiting for a first frame, or a mix (some monitors excluded, some unchanged).
    Health::Saving
}

/// Consecutive ticks a new not-saving condition must persist before it is reported. Recovery to
/// [`Health::Saving`] is reported at once.
pub const ENTER_AFTER: u32 = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Transition {
    pub from: Health,
    pub to: Health,
    /// When the new condition was first seen (its first tick, not the confirming one).
    pub since: Timestamp,
}

#[derive(Debug)]
pub struct HealthTracker {
    current: Health,
    since: Option<Timestamp>,
    pending: Option<(Health, u32, Timestamp)>,
}

impl Default for HealthTracker {
    fn default() -> Self {
        Self {
            current: Health::Saving,
            since: None,
            pending: None,
        }
    }
}

impl HealthTracker {
    pub fn current(&self) -> Health {
        self.current
    }

    pub fn since(&self) -> Option<Timestamp> {
        self.since
    }

    /// Feeds one tick's health; returns the transition when the reported health changes.
    pub fn observe(&mut self, health: Health, now: Timestamp) -> Option<Transition> {
        if health == self.current {
            self.pending = None;
            return None;
        }
        let (count, first) = match self.pending {
            Some((pending, count, first)) if pending == health => (count + 1, first),
            _ => (1, now),
        };
        if health != Health::Saving && count < ENTER_AFTER {
            self.pending = Some((health, count, first));
            return None;
        }
        let transition = Transition {
            from: self.current,
            to: health,
            since: first,
        };
        self.current = health;
        self.since = Some(first);
        self.pending = None;
        Some(transition)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(s: i64) -> Timestamp {
        Timestamp(1_000_000 + s * 1_000)
    }

    #[test]
    fn codes_round_trip_and_unknown_codes_survive() {
        for h in [
            Health::Saving,
            Health::WindowListUnknown,
            Health::IdleUnknown,
            Health::CaptureFailing,
            Health::NoDisplays,
            Health::Idle,
            Health::PrivacyExcluded,
        ] {
            assert_eq!(Health::from_code(h.code()), h);
        }
        assert_eq!(Health::from_code(99), Health::Other(99));
        assert_eq!(Health::Other(99).code(), 99);
        assert!(Health::Other(99).is_fault());
        assert!(!Health::Idle.is_fault());
    }

    #[test]
    fn a_condition_is_reported_only_after_it_persists_and_recovery_is_immediate() {
        let mut t = HealthTracker::default();
        assert_eq!(t.observe(Health::WindowListUnknown, at(0)), None);
        assert_eq!(t.observe(Health::WindowListUnknown, at(1)), None);
        assert_eq!(
            t.observe(Health::WindowListUnknown, at(2)),
            Some(Transition {
                from: Health::Saving,
                to: Health::WindowListUnknown,
                since: at(0)
            })
        );
        assert_eq!(t.current(), Health::WindowListUnknown);
        assert_eq!(t.since(), Some(at(0)));
        // Steady state: no repeated transitions (so no repeated log lines).
        assert_eq!(t.observe(Health::WindowListUnknown, at(3)), None);
        assert_eq!(
            t.observe(Health::Saving, at(4)),
            Some(Transition {
                from: Health::WindowListUnknown,
                to: Health::Saving,
                since: at(4)
            })
        );
    }

    #[test]
    fn a_single_odd_tick_does_not_flap() {
        let mut t = HealthTracker::default();
        for s in 0..10 {
            let h = if s % 2 == 0 {
                Health::CaptureFailing
            } else {
                Health::Saving
            };
            assert_eq!(t.observe(h, at(s)), None, "tick {s}");
        }
        assert_eq!(t.current(), Health::Saving);
        // Alternating between two different faults never confirms either.
        for s in 10..20 {
            let h = if s % 2 == 0 {
                Health::IdleUnknown
            } else {
                Health::WindowListUnknown
            };
            assert_eq!(t.observe(h, at(s)), None);
        }
    }

    #[test]
    fn classification_prefers_saving_then_the_most_fundamental_reason() {
        let base = TickFacts {
            monitors: 2,
            ..TickFacts::default()
        };
        assert_eq!(classify(&base), Health::Saving);
        assert_eq!(
            classify(&TickFacts {
                monitors: 0,
                ..base
            }),
            Health::NoDisplays
        );
        let everything_wrong = TickFacts {
            window_list_unknown: true,
            idle_unknown: true,
            idle: true,
            capture_failed: 2,
            rule_excluded: 2,
            ..base
        };
        assert_eq!(classify(&everything_wrong), Health::WindowListUnknown);
        assert_eq!(
            classify(&TickFacts {
                window_list_unknown: false,
                ..everything_wrong
            }),
            Health::IdleUnknown
        );
        assert_eq!(
            classify(&TickFacts {
                capture_failed: 2,
                ..base
            }),
            Health::CaptureFailing
        );
        assert_eq!(
            classify(&TickFacts {
                rule_excluded: 2,
                ..base
            }),
            Health::PrivacyExcluded
        );
        assert_eq!(
            classify(&TickFacts {
                rule_excluded: 1,
                ..base
            }),
            Health::Saving
        );
        assert_eq!(classify(&TickFacts { idle: true, ..base }), Health::Idle);
        // One monitor saving is saving, whatever the others do.
        assert_eq!(
            classify(&TickFacts {
                saving: 1,
                ..everything_wrong
            }),
            Health::Saving
        );
    }

    #[test]
    fn reads_health_from_a_heartbeat() {
        let mut counters = std::collections::BTreeMap::new();
        assert_eq!(from_counters(&counters), None);
        counters.insert(HEALTH_KEY.to_string(), 1);
        assert_eq!(
            from_counters(&counters),
            Some((Health::WindowListUnknown, None))
        );
        counters.insert(HEALTH_SINCE_KEY.to_string(), 42);
        assert_eq!(
            from_counters(&counters),
            Some((Health::WindowListUnknown, Some(Timestamp(42))))
        );
    }
}
