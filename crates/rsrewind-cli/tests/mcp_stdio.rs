//! The real `rsrewind mcp` binary over real pipes: what an MCP client sees.

use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};

type Fallible<T> = Result<T, Box<dyn std::error::Error>>;

fn session(data: &std::path::Path, messages: &[Value]) -> Fallible<Vec<Value>> {
    let mut child = Command::new(env!("CARGO_BIN_EXE_rsrewind"))
        .arg("--data-dir")
        .arg(data)
        .arg("mcp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let mut stdin = child.stdin.take().ok_or("stdin")?;
    for m in messages {
        writeln!(stdin, "{m}")?;
    }
    drop(stdin); // end of input: the server exits
    let stdout = child.stdout.take().ok_or("stdout")?;
    let replies = BufReader::new(stdout)
        .lines()
        .map(|l| Ok(serde_json::from_str(&l?)?))
        .collect::<Fallible<Vec<Value>>>()?;
    let status = child.wait()?;
    assert!(status.success(), "{status}");
    Ok(replies)
}

#[test]
fn the_binary_speaks_mcp_on_stdio_and_is_off_by_default() -> Fallible<()> {
    let dir = tempfile::tempdir()?;
    let replies = session(
        dir.path(),
        &[
            json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": { "protocolVersion": "2025-06-18" } }),
            json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
            json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }),
            json!({ "jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": { "name": "get_status", "arguments": {} } }),
        ],
    )?;
    assert_eq!(
        replies.len(),
        3,
        "only requests are answered, and nothing else is printed: {replies:?}"
    );
    assert_eq!(
        replies[0]["result"]["serverInfo"]["version"],
        env!("CARGO_PKG_VERSION")
    );
    let tools: Vec<&str> = replies[1]["result"]["tools"]
        .as_array()
        .ok_or("tools")?
        .iter()
        .filter_map(|t| t["name"].as_str())
        .collect();
    assert_eq!(tools, ["get_status"], "off by default");
    let status = &replies[2]["result"];
    assert_eq!(
        status["structuredContent"]["agent_access"]["enabled"], false,
        "{status}"
    );
    // The status went through the real CLI (`rsrewind status --json`).
    assert!(
        status["structuredContent"]["recorder"]["state"].is_string(),
        "{status}"
    );
    Ok(())
}

#[test]
fn enable_turns_the_tools_on() -> Fallible<()> {
    let dir = tempfile::tempdir()?;
    let out = Command::new(env!("CARGO_BIN_EXE_rsrewind"))
        .arg("--data-dir")
        .arg(dir.path())
        .args(["mcp", "--enable"])
        .output()?;
    assert!(out.status.success());
    assert!(String::from_utf8(out.stdout)?.contains("leaves this"));
    let replies = session(
        dir.path(),
        &[json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" })],
    )?;
    assert_eq!(
        replies[0]["result"]["tools"].as_array().map(Vec::len),
        Some(8),
        "all but screenshots"
    );
    Ok(())
}
