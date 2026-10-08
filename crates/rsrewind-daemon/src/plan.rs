//! The per-tick decision, kept free of Windows and SQLite so the rules that matter most — never
//! store pixels while paused, idle or privacy-excluded — are unit-tested directly.

use rsrewind_core::{CaptureState, PrivacyDecision, PrivacyPolicy, Timestamp};

/// Everything the capture thread knows about one monitor at one tick.
#[derive(Debug, Clone)]
pub struct MonitorTick {
    /// Windows produced a new frame since the last tick.
    pub has_new_frame: bool,
    /// The new frame differs meaningfully from the last *persisted* state on this monitor.
    pub changed: bool,
    /// A visual state has already been persisted for this monitor in this session.
    pub has_current_state: bool,
    /// Privacy verdict for the windows visible on this monitor.
    pub privacy: PrivacyDecision,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MonitorAction {
    /// Store a new visual state and start a new observation.
    Persist,
    /// Same picture: extend the current observation's end time.
    Extend,
    /// Store nothing. `reason` is safe to log.
    Skip(SkipReason),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkipReason {
    Paused,
    Idle,
    Privacy(String),
    /// No frame yet on a monitor that has never been persisted (e.g. right after start).
    NoFrame,
}

pub fn plan_monitor(
    control: CaptureState,
    now: Timestamp,
    idle: bool,
    tick: &MonitorTick,
) -> MonitorAction {
    match control.effective_at(now) {
        CaptureState::Recording => {}
        CaptureState::Paused { .. } | CaptureState::Stopped | CaptureState::Error => {
            return MonitorAction::Skip(SkipReason::Paused);
        }
    }
    // Privacy before idle: an excluded window must never be extended into, even while idle.
    if let PrivacyDecision::Exclude { rule } = &tick.privacy {
        return MonitorAction::Skip(SkipReason::Privacy(rule.clone()));
    }
    if idle {
        return MonitorAction::Skip(SkipReason::Idle);
    }
    match (tick.has_new_frame && tick.changed, tick.has_current_state) {
        (true, _) => MonitorAction::Persist,
        (false, true) => MonitorAction::Extend,
        (false, false) if tick.has_new_frame => MonitorAction::Persist,
        (false, false) => MonitorAction::Skip(SkipReason::NoFrame),
    }
}

/// Privacy verdict for a monitor: excluded if *any* visible window on it matches a rule, so an
/// unfocused password manager on a second screen is still never recorded.
pub fn monitor_privacy<'a>(
    policy: &PrivacyPolicy,
    windows_on_monitor: impl IntoIterator<
        Item = (
            &'a rsrewind_core::ApplicationContext,
            &'a rsrewind_core::WindowContext,
        ),
    >,
) -> PrivacyDecision {
    for (app, window) in windows_on_monitor {
        let decision = policy.evaluate(Some(app), Some(window));
        if decision.is_excluded() {
            return decision;
        }
    }
    PrivacyDecision::Allow
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsrewind_core::{ApplicationContext, WindowContext};

    fn tick(has_new_frame: bool, changed: bool, has_current_state: bool) -> MonitorTick {
        MonitorTick {
            has_new_frame,
            changed,
            has_current_state,
            privacy: PrivacyDecision::Allow,
        }
    }

    const NOW: Timestamp = Timestamp(10_000);

    #[test]
    fn recording_persists_changes_and_extends_otherwise() {
        let rec = CaptureState::Recording;
        assert_eq!(
            plan_monitor(rec, NOW, false, &tick(true, true, true)),
            MonitorAction::Persist
        );
        assert_eq!(
            plan_monitor(rec, NOW, false, &tick(true, false, true)),
            MonitorAction::Extend
        );
        assert_eq!(
            plan_monitor(rec, NOW, false, &tick(false, false, true)),
            MonitorAction::Extend
        );
        assert_eq!(
            plan_monitor(rec, NOW, false, &tick(true, false, false)),
            MonitorAction::Persist
        );
        assert_eq!(
            plan_monitor(rec, NOW, false, &tick(false, false, false)),
            MonitorAction::Skip(SkipReason::NoFrame)
        );
    }

    #[test]
    fn paused_never_stores() {
        for state in [
            CaptureState::Paused { until: None },
            CaptureState::Paused {
                until: Some(Timestamp(NOW.0 + 1)),
            },
            CaptureState::Stopped,
            CaptureState::Error,
        ] {
            assert_eq!(
                plan_monitor(state, NOW, false, &tick(true, true, true)),
                MonitorAction::Skip(SkipReason::Paused),
                "{state:?}"
            );
        }
    }

    #[test]
    fn expired_pause_records_again() {
        let state = CaptureState::Paused { until: Some(NOW) };
        assert_eq!(
            plan_monitor(state, NOW, false, &tick(true, true, true)),
            MonitorAction::Persist
        );
    }

    #[test]
    fn privacy_wins_over_everything_but_pause() {
        let mut t = tick(true, true, true);
        t.privacy = PrivacyDecision::Exclude {
            rule: "process:1Password.exe".into(),
        };
        let expected = MonitorAction::Skip(SkipReason::Privacy("process:1Password.exe".into()));
        assert_eq!(
            plan_monitor(CaptureState::Recording, NOW, false, &t),
            expected
        );
        assert_eq!(
            plan_monitor(CaptureState::Recording, NOW, true, &t),
            expected
        );
        t.has_new_frame = false;
        t.changed = false;
        assert_eq!(
            plan_monitor(CaptureState::Recording, NOW, false, &t),
            expected
        );
    }

    #[test]
    fn idle_stores_nothing() {
        assert_eq!(
            plan_monitor(CaptureState::Recording, NOW, true, &tick(true, true, true)),
            MonitorAction::Skip(SkipReason::Idle)
        );
    }

    #[test]
    fn any_visible_excluded_window_excludes_the_monitor() {
        let policy = PrivacyPolicy {
            excluded_processes: vec!["KeePassXC.exe".into()],
            excluded_title_patterns: vec![],
            ..Default::default()
        };
        let editor = (
            ApplicationContext {
                process_name: "Code.exe".into(),
                exe_path: None,
            },
            WindowContext {
                title: "main.rs".into(),
                class_name: None,
            },
        );
        let vault = (
            ApplicationContext {
                process_name: "keepassxc.exe".into(),
                exe_path: None,
            },
            WindowContext {
                title: "Passwords".into(),
                class_name: None,
            },
        );
        let windows = [&editor, &vault];
        assert!(monitor_privacy(&policy, windows.iter().map(|(a, w)| (a, w))).is_excluded());
        let windows = [&editor];
        assert_eq!(
            monitor_privacy(&policy, windows.iter().map(|(a, w)| (a, w))),
            PrivacyDecision::Allow
        );
    }
}
