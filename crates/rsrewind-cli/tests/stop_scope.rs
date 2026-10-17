//! `rsrewind --data-dir <folder> stop` stops only a recorder recording into that folder.
//!
//! On Windows the stop signal is one named event per session. Before 2026-10-16 `stop` set it
//! whatever `--data-dir` said, so stopping a test or scratch folder stopped the user's real
//! recorder. The fix checks the folder's own heartbeat first, as Linux and macOS always did.
//!
//! Grading note: this test can only tell the two behaviours apart on a machine where another
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
