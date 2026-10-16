//! Issue #33: while the recorder records but stores nothing, `status`, `status --json` and
//! `doctor` say so (and why); while it saves, their output carries nothing new.

use rsrewind_core::{CaptureState, DataDir, Timestamp};
use rsrewind_storage::{RecorderStatus, Store};
use std::collections::BTreeMap;
use std::process::{Command, Output};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn rsrewind(data: &std::path::Path, args: &[&str]) -> Result<Output, Box<dyn std::error::Error>> {
    Ok(Command::new(env!("CARGO_BIN_EXE_rsrewind"))
        .arg("--data-dir")
        .arg(data)
        .args(args)
        .env_remove("RSREWIND_DATA_DIR")
        .output()?)
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// A live heartbeat (just written, recording) publishing `health`.
fn beat(store: &Store, health: Option<u64>) -> TestResult {
    let mut counters = BTreeMap::new();
    counters.insert("window_list_unknown_ticks".to_string(), 42);
    if let Some(code) = health {
        counters.insert("health".to_string(), code);
        counters.insert("health_since_ms".to_string(), 1_790_000_000_000);
    }
    store.write_status(&RecorderStatus {
        pid: std::process::id(),
        started_at: Timestamp::now(),
        heartbeat_at: Timestamp::now(),
        state: CaptureState::Recording,
        counters,
    })?;
    Ok(())
}

#[test]
fn status_and_doctor_say_when_nothing_is_saved() -> TestResult {
    let tmp = tempfile::tempdir()?;
    let root = tmp.path().join("data");
    let store = Store::open(&DataDir::new(&root))?;

    // A recorder older than the signal (no `health` key), and one that is saving: no new output.
    for health in [None, Some(0)] {
        beat(&store, health)?;
        let status = stdout(&rsrewind(&root, &["status"])?);
        assert!(status.contains("● Recording"), "{status}");
        assert!(!status.contains("saved"), "{status}");
        let json: serde_json::Value =
            serde_json::from_str(&stdout(&rsrewind(&root, &["status", "--json"])?))?;
        assert_eq!(json["state"], "recording");
        assert!(json.get("not_saving").is_none(), "{json}");
    }
    let doctor = stdout(&rsrewind(&root, &["doctor"])?);
    assert!(!doctor.contains("NOTHING"), "{doctor}");

    // Window list unknown on every tick.
    beat(&store, Some(1))?;
    let status = stdout(&rsrewind(&root, &["status"])?);
    assert!(status.contains("● Recording"), "{status}");
    assert!(
        status.contains("Nothing is being saved") && status.contains("window context unavailable"),
        "{status}"
    );
    let json: serde_json::Value =
        serde_json::from_str(&stdout(&rsrewind(&root, &["status", "--json"])?))?;
    assert_eq!(json["not_saving"]["reason"], "window_context_unavailable");
    assert_eq!(json["not_saving"]["fault"], true);
    assert_eq!(json["not_saving"]["since"], 1_790_000_000_000i64);
    let doctor = stdout(&rsrewind(&root, &["doctor"])?);
    let line = doctor
        .lines()
        .find(|l| l.contains("saving"))
        .ok_or("doctor has no saving line")?;
    assert!(line.starts_with("[warn]"), "{doctor}");
    assert!(line.contains("NOTHING has been saved"), "{line}");
    assert!(
        line.contains("42 tick(s) with the window list unknown"),
        "{line}"
    );

    // Idle is reported, but not as a fault.
    beat(&store, Some(5))?;
    let status = stdout(&rsrewind(&root, &["status"])?);
    assert!(status.contains("Not saving right now"), "{status}");
    assert!(!status.contains("⚠ Nothing"), "{status}");
    Ok(())
}
