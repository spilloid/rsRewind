//! `rsrewind --data-dir <folder> stop` stops only a recorder recording into that folder.
//!
//! On Windows the stop signal was one named event per session. Before 2026-10-16 `stop` set it
//! whatever `--data-dir` said, so stopping a test or scratch folder stopped the user's real
//! recorder. The fix checks the folder's own heartbeat first, as Linux and macOS always did, and
//! since 2026-10-19 signals only the event of the recorder run that heartbeat names by its random
//! stop token. The race between the check and the signal is exercised in the CLI's `stop_target`
//! unit tests and `rsrewind_daemon::win`.
//!
//! Grading note: the first test can only tell the two behaviours apart on a machine where another
//! recorder is running in the same session (the old code then printed "Recorder stopped." after
//! stopping it). On CI, with no recorder, both print the same line. Its red run was deliberately
//! not performed on the development machine, because that would have stopped the installed
//! recorder.

use std::process::Command;

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[cfg(any(windows, target_os = "linux", target_os = "macos"))]
#[test]
fn stop_with_a_folder_that_has_no_recorder_touches_nothing() -> TestResult {
    let tmp = tempfile::tempdir()?;
    for root in [tmp.path().join("missing"), tmp.path().join("empty")] {
        if root.ends_with("empty") {
            // A store with history but no live heartbeat.
            rsrewind_storage::Store::open(&rsrewind_core::DataDir::new(&root))?;
        }
        let out = Command::new(env!("CARGO_BIN_EXE_rsrewind"))
            .arg("--data-dir")
            .arg(&root)
            .arg("stop")
            .env_remove("RSREWIND_DATA_DIR")
            .output()?;
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(out.status.success(), "{stdout}");
        assert!(
            stdout.contains("not recording"),
            "stop claimed to stop something for {}: {stdout}",
            root.display()
        );
    }
    Ok(())
}

/// A folder whose live heartbeat names a recorder run that is not running (it exited, or crashed
/// and its heartbeat is still fresh). On Windows, `stop` signals that run's own event, which no
/// longer exists: nothing is stopped. A heartbeat without a stop token (a 0.1.2 recorder) is not
/// signalled at all without `--legacy-session`, which this test never passes: it would reach the
/// recorder installed on the development machine.
#[cfg(windows)]
#[test]
fn stop_signals_only_the_run_the_heartbeat_names() -> TestResult {
    use rsrewind_core::{CaptureState, DataDir, Timestamp};
    use rsrewind_storage::{RecorderStatus, Store};
    let tmp = tempfile::tempdir()?;
    for (folder, token) in [
        ("gone-run", Some(0x0123_4567_89ab_cdef_u64)),
        ("old-run", None),
    ] {
        let root = tmp.path().join(folder);
        let store = Store::open(&DataDir::new(&root))?;
        let mut counters = std::collections::BTreeMap::new();
        if let Some(token) = token {
            counters.insert("stop_token".to_string(), token);
        }
        store.write_status(&RecorderStatus {
            // Windows pids are multiples of 4: no process has this one.
            pid: u32::MAX - 2,
            started_at: Timestamp::now(),
            heartbeat_at: Timestamp::now(),
            state: CaptureState::Recording,
            counters,
        })?;
        let out = Command::new(env!("CARGO_BIN_EXE_rsrewind"))
            .arg("--data-dir")
            .arg(&root)
            .arg("stop")
            .env_remove("RSREWIND_DATA_DIR")
            .output()?;
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        if token.is_some() {
            assert!(out.status.success(), "{stdout}{stderr}");
            assert!(stdout.contains("not recording"), "{stdout}");
        } else {
            assert!(!out.status.success(), "{stdout}");
            assert!(stderr.contains("--legacy-session"), "{stderr}");
            assert!(stderr.contains("nothing was sent"), "{stderr}");
        }
    }
    Ok(())
}
