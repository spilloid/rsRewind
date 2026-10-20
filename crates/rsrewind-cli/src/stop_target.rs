//! Which recorder `rsrewind stop` signals on Windows, as a decision over a [`StopPort`] so the
//! race between checking a data folder's heartbeat and signalling can be tested without touching
//! a real recorder.
#![cfg_attr(not(windows), allow(dead_code))]

use anyhow::Result;
use rsrewind_daemon::counters::STOP_TOKEN_KEY;
use rsrewind_storage::RecorderStatus;

/// What `stop` can observe and do (the real one: the data folder's store and Win32 events).
pub(crate) trait StopPort {
    /// The data folder's heartbeat, if it is live.
    fn live_heartbeat(&mut self) -> Result<Option<RecorderStatus>>;
    /// Sets the stop event of the recorder run that published `token`. `false` if that run is not
    /// running.
    fn signal_run(&mut self, token: u64) -> Result<bool>;
    /// Sets the per-session stop event recorders up to 0.1.2 listen on. `false` if none runs.
    fn signal_legacy_session(&mut self) -> Result<bool>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StopOutcome {
    /// No recorder is recording into this folder (or it exited before the signal).
    NotRunning,
    Signalled,
    /// This folder's recorder is from 0.1.2 or earlier and publishes no stop token; nothing was
    /// signalled (it can only be reached with `--legacy-session`).
    LegacyRecorder {
        pid: u32,
    },
}

/// Signals exactly the recorder run named by the folder's live heartbeat: if that run exits before
/// the signal, its event is gone and nothing is signalled. A 0.1.2 recorder (no token) is only
/// signalled with `legacy_session`, through the per-session event, which reaches whichever 0.1.2
/// recorder runs in the session at that moment.
pub(crate) fn stop_recorder(port: &mut impl StopPort, legacy_session: bool) -> Result<StopOutcome> {
    let Some(status) = port.live_heartbeat()? else {
        return Ok(StopOutcome::NotRunning);
    };
    let signalled = match status.counters.get(STOP_TOKEN_KEY) {
        Some(&token) => port.signal_run(token)?,
        None if legacy_session => port.signal_legacy_session()?,
        None => return Ok(StopOutcome::LegacyRecorder { pid: status.pid }),
    };
    Ok(if signalled {
        StopOutcome::Signalled
    } else {
        StopOutcome::NotRunning
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsrewind_core::{CaptureState, Timestamp};
    use std::collections::BTreeMap;

    /// One run of a recorder: its pid, the folder it records into, and its stop token (`None`:
    /// 0.1.2 or earlier, listening on the per-session event).
    #[derive(Debug, Clone, PartialEq, Eq)]
    struct Run {
        pid: u32,
        folder: &'static str,
        token: Option<u64>,
    }

    impl Run {
        fn heartbeat(&self) -> RecorderStatus {
            let mut counters = BTreeMap::new();
            if let Some(token) = self.token {
                counters.insert(STOP_TOKEN_KEY.to_string(), token);
            }
            RecorderStatus {
                pid: self.pid,
                started_at: Timestamp(0),
                heartbeat_at: Timestamp(0),
                state: CaptureState::Recording,
                counters,
            }
        }
    }

    /// One Windows session, at most one recorder at a time (the session-wide mutex).
    struct Session {
        /// The live heartbeat in the folder `stop --data-dir` was given. Kept apart from `running`:
        /// a crashed run's heartbeat stays live for up to 20 s.
        heartbeat: Option<RecorderStatus>,
        running: Option<Run>,
        /// Takes over after the CLI read the heartbeat and before it signals: the race.
        replace_before_signal: Option<Run>,
        stopped: Vec<Run>,
    }

    impl Session {
        fn new(heartbeat_of: &Run, running: Option<Run>, replacement: Option<Run>) -> Self {
            Self {
                heartbeat: Some(heartbeat_of.heartbeat()),
                running,
                replace_before_signal: replacement,
                stopped: Vec::new(),
            }
        }

        fn race(&mut self) {
            if let Some(next) = self.replace_before_signal.take() {
                self.running = Some(next);
            }
        }
    }

    impl StopPort for Session {
        fn live_heartbeat(&mut self) -> Result<Option<RecorderStatus>> {
            let heartbeat = self.heartbeat.clone();
            self.race();
            Ok(heartbeat)
        }

        fn signal_run(&mut self, token: u64) -> Result<bool> {
            match &self.running {
                Some(run) if run.token == Some(token) => {
                    self.stopped.push(run.clone());
                    Ok(true)
                }
                _ => Ok(false),
            }
        }

        fn signal_legacy_session(&mut self) -> Result<bool> {
            match &self.running {
                Some(run) if run.token.is_none() => {
                    self.stopped.push(run.clone());
                    Ok(true)
                }
                _ => Ok(false),
            }
        }
    }

    fn run(pid: u32, folder: &'static str, token: Option<u64>) -> Run {
        Run { pid, folder, token }
    }

    #[test]
    fn the_folders_own_recorder_is_stopped() -> Result<()> {
        let a = run(100, "A", Some(0xA));
        let mut session = Session::new(&a, Some(a.clone()), None);
        assert_eq!(stop_recorder(&mut session, false)?, StopOutcome::Signalled);
        assert_eq!(session.stopped, [a]);
        Ok(())
    }

    /// astra re-review, 2026-10-19: `stop --data-dir A` checks that A's 0.1.2 recorder is live,
    /// then A exits and a 0.1.2 recorder for folder B starts before the signal. No pid is reused,
    /// yet the automatic fallback to the per-session event stopped B. There is no automatic
    /// fallback any more: nothing is signalled, and `stop` says how to reach a 0.1.2 recorder.
    #[test]
    fn a_legacy_recorder_replaced_between_check_and_signal_is_not_stopped() -> Result<()> {
        let a = run(100, "A", None);
        let b = run(200, "B", None);
        let mut session = Session::new(&a, Some(a.clone()), Some(b));
        assert_eq!(
            stop_recorder(&mut session, false)?,
            StopOutcome::LegacyRecorder { pid: 100 }
        );
        assert_eq!(session.stopped, []);
        Ok(())
    }

    /// The same race with recorders of this version: the token names A's run, whose event is gone.
    #[test]
    fn a_recorder_replaced_between_check_and_signal_is_not_stopped() -> Result<()> {
        let a = run(100, "A", Some(0xA));
        let b = run(200, "B", Some(0xB));
        let mut session = Session::new(&a, Some(a.clone()), Some(b));
        assert_eq!(stop_recorder(&mut session, false)?, StopOutcome::NotRunning);
        assert_eq!(session.stopped, []);
        Ok(())
    }

    /// A crashed run's heartbeat stays live for up to 20 s; a new run for another folder that got
    /// the same pid meanwhile is not the run the heartbeat names.
    #[test]
    fn a_stale_heartbeat_does_not_reach_a_new_run_with_the_same_pid() -> Result<()> {
        let crashed = run(100, "A", Some(0xA));
        let reused_pid = run(100, "B", Some(0xB));
        let mut session = Session::new(&crashed, Some(reused_pid), None);
        assert_eq!(stop_recorder(&mut session, false)?, StopOutcome::NotRunning);
        assert_eq!(session.stopped, []);
        Ok(())
    }

    /// Opted in, the per-session event is used, and it reaches whichever 0.1.2 recorder runs then,
    /// which is what `--legacy-session` documents.
    #[test]
    fn legacy_session_signals_the_sessions_old_recorder() -> Result<()> {
        let a = run(100, "A", None);
        let mut session = Session::new(&a, Some(a.clone()), None);
        assert_eq!(stop_recorder(&mut session, true)?, StopOutcome::Signalled);
        assert_eq!(session.stopped, [a]);
        Ok(())
    }
}
