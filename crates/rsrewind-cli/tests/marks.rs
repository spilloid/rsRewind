//! `rsrewind mark`: the path the window takes to write marks (it may not link storage), end to end
//! through the real binary.

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

fn list(data: &std::path::Path) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    let out = rsrewind(data, &["mark", "list", "--json"])?;
    assert!(out.status.success(), "{out:?}");
    Ok(serde_json::from_slice(&out.stdout)?)
}

#[test]
fn marks_round_trip_through_the_command_line() -> TestResult {
    let tmp = tempfile::tempdir()?;
    let root = tmp.path().join("data");

    // Nothing yet: listing an empty (even missing) folder is not an error.
    assert_eq!(list(&root)?, serde_json::json!([]));

    let out = rsrewind(
        &root,
        &[
            "mark",
            "add",
            "--at-ms",
            "1790000000000",
            "--label=-starts with a dash",
            "--json",
        ],
    )?;
    assert!(out.status.success(), "{out:?}");
    let added: serde_json::Value = serde_json::from_slice(&out.stdout)?;
    let id = added["id"].as_i64().ok_or("no id")?;
    assert_eq!(added["at"], 1_790_000_000_000_i64);
    assert!(
        rsrewind(&root, &["mark", "add", "--at", "15m"])?
            .status
            .success()
    );

    let marks = list(&root)?;
    assert_eq!(marks.as_array().map(Vec::len), Some(2));
    assert_eq!(marks[0]["label"], "-starts with a dash");
    assert_eq!(marks[0]["id"], id);

    let id_text = id.to_string();
    let renamed = rsrewind(&root, &["mark", "rename", &id_text, "--label", "standup"])?;
    assert!(renamed.status.success(), "{renamed:?}");
    assert_eq!(list(&root)?[0]["label"], "standup");

    // A bad label is refused and changes nothing.
    let bad = rsrewind(&root, &["mark", "rename", &id_text, "--label", "a\nb"])?;
    assert!(!bad.status.success());
    assert_eq!(list(&root)?[0]["label"], "standup");

    assert!(
        rsrewind(&root, &["mark", "remove", &id_text])?
            .status
            .success()
    );
    let again = rsrewind(&root, &["mark", "remove", &id_text])?;
    assert!(!again.status.success(), "removing twice says so");
    assert_eq!(list(&root)?.as_array().map(Vec::len), Some(1));
    Ok(())
}

/// The window passes labels on standard input, so they never appear in the process list.
#[test]
fn labels_can_come_on_standard_input() -> TestResult {
    use std::io::Write;
    let tmp = tempfile::tempdir()?;
    let root = tmp.path().join("data");
    let send = |args: &[&str], input: &str| -> Result<Output, Box<dyn std::error::Error>> {
        let mut child = Command::new(env!("CARGO_BIN_EXE_rsrewind"))
            .arg("--data-dir")
            .arg(&root)
            .args(args)
            .env_remove("RSREWIND_DATA_DIR")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()?;
        child
            .stdin
            .take()
            .ok_or("no stdin")?
            .write_all(input.as_bytes())?;
        Ok(child.wait_with_output()?)
    };
    let out = send(
        &[
            "mark",
            "add",
            "--at-ms",
            "1790000000000",
            "--label-stdin",
            "--json",
        ],
        "secret plan\n",
    )?;
    assert!(out.status.success(), "{out:?}");
    let id: serde_json::Value = serde_json::from_slice(&out.stdout)?;
    let id = id["id"].as_i64().ok_or("id")?.to_string();
    assert_eq!(list(&root)?[0]["label"], "secret plan");
    let renamed = send(&["mark", "rename", &id, "--label-stdin"], "-dash first\r\n")?;
    assert!(renamed.status.success(), "{renamed:?}");
    assert_eq!(list(&root)?[0]["label"], "-dash first");
    // Two lines are refused (one-line labels), and nothing changes.
    let two = send(&["mark", "rename", &id, "--label-stdin"], "a\nb\n")?;
    assert!(!two.status.success());
    assert_eq!(list(&root)?[0]["label"], "-dash first");
    // Both ways at once is a usage error.
    let both = send(&["mark", "add", "--label", "x", "--label-stdin"], "y")?;
    assert!(!both.status.success());
    Ok(())
}

#[test]
fn a_mark_inside_forgotten_time_is_refused() -> TestResult {
    let tmp = tempfile::tempdir()?;
    let root = tmp.path().join("data");
    assert!(
        rsrewind(&root, &["mark", "add", "--at", "1m"])?
            .status
            .success()
    );
    let forget = rsrewind(&root, &["forget", "5m", "--yes"])?;
    assert!(forget.status.success(), "{forget:?}");
    assert!(String::from_utf8_lossy(&forget.stdout).contains("1 mark"));
    let refused = rsrewind(&root, &["mark", "add", "--at", "2m"])?;
    assert!(!refused.status.success());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("deleted"));
    assert_eq!(list(&root)?, serde_json::json!([]));
    Ok(())
}
