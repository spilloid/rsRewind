//! `status` and `doctor` keep saying "privacy rules NOT enforced" while the newest recording
//! session ran without enforceable rules, and say nothing new otherwise.

use rsrewind_core::{Capabilities, DataDir, PrivacyPolicy, SessionCapabilities, Timestamp};
use rsrewind_storage::Store;
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

fn session(store: &Store, capabilities: Capabilities) -> TestResult {
    let id = store.begin_session(Timestamp::now(), "HOST", "0.0.2")?;
    store.record_session_capabilities(
        id,
        &SessionCapabilities::new(capabilities, &PrivacyPolicy::suggested_defaults()),
    )?;
    Ok(())
}

#[test]
fn the_badge_follows_the_newest_session() -> TestResult {
    let tmp = tempfile::tempdir()?;
    let root = tmp.path().join("data");
    let store = Store::open(&DataDir::new(&root))?;

    // No session yet, then a fully enforced one: no badge anywhere.
    for _ in 0..2 {
        let status = stdout(&rsrewind(&root, &["status"])?);
        assert!(!status.contains("NOT enforced"), "{status}");
        let json = stdout(&rsrewind(&root, &["status", "--json"])?);
        assert!(!json.contains("privacy_unenforced"), "{json}");
        let doctor = stdout(&rsrewind(&root, &["doctor"])?);
        assert!(!doctor.contains("NOT enforced"), "{doctor}");
        session(&store, Capabilities::WINDOWS)?;
    }

    // A session that could not list windows.
    session(
        &store,
        Capabilities {
            list_windows: false,
            ..Capabilities::WINDOWS
        },
    )?;
    let status = stdout(&rsrewind(&root, &["status"])?);
    assert!(status.contains("Privacy rules NOT enforced"), "{status}");
    assert!(status.contains("window list"), "{status}");
    let json: serde_json::Value =
        serde_json::from_str(&stdout(&rsrewind(&root, &["status", "--json"])?))?;
    assert!(json["privacy_unenforced"].is_string(), "{json}");
    let doctor = stdout(&rsrewind(&root, &["doctor"])?);
    assert!(doctor.contains("privacy rules NOT enforced"), "{doctor}");

    // An enforcing session afterwards clears it.
    session(&store, Capabilities::WINDOWS)?;
    let status = stdout(&rsrewind(&root, &["status"])?);
    assert!(!status.contains("NOT enforced"), "{status}");
    Ok(())
}
