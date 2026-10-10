//! The real `rsrewind mcp` binary over real pipes: what an MCP client sees.

use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};

type Fallible<T> = Result<T, Box<dyn std::error::Error>>;

#[path = "../../rsrewind-mcp/tests/common/mod.rs"]
mod fixture;

fn initialize(id: &str) -> Value {
    json!({"jsonrpc":"2.0", "id":id, "method":"initialize", "params": {
        "protocolVersion":"2025-11-25", "capabilities":{}, "clientInfo":{"name":"contract", "version":"1"}}})
}

fn session(data: &std::path::Path, messages: &[Value]) -> Fallible<Vec<Value>> {
    let mut messages = messages.to_vec();
    let bootstrap = messages.first().is_none_or(|m| m["method"] != "initialize");
    if bootstrap {
        messages.insert(0, initialize("test-init"));
        messages.insert(
            1,
            json!({"jsonrpc":"2.0", "method":"notifications/initialized"}),
        );
    }
    let mut child = Command::new(env!("CARGO_BIN_EXE_rsrewind"))
        .arg("--data-dir")
        .arg(data)
        .arg("mcp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let mut stdin = child.stdin.take().ok_or("stdin")?;
    for m in &messages {
        writeln!(stdin, "{m}")?;
    }
    drop(stdin); // end of input: the server exits
    let stdout = child.stdout.take().ok_or("stdout")?;
    let mut replies = BufReader::new(stdout)
        .lines()
        .map(|l| Ok(serde_json::from_str(&l?)?))
        .collect::<Fallible<Vec<Value>>>()?;
    let status = child.wait()?;
    assert!(status.success(), "{status}");
    if bootstrap {
        replies.remove(0);
    }
    Ok(replies)
}

#[test]
fn the_binary_speaks_mcp_on_stdio_and_is_off_by_default() -> Fallible<()> {
    let dir = tempfile::tempdir()?;
    let replies = session(
        dir.path(),
        &[
            initialize("1"),
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
fn every_workflow_runs_through_the_real_stdio_binary() -> Fallible<()> {
    let (_dir, data) = fixture::history()?;
    std::fs::write(
        data.root().join("config.toml"),
        "[mcp]\nenabled = true\nallow_screenshots = true\n",
    )?;
    let calls = [
        ("search", json!({"query":"linker"})),
        ("recent", json!({"limit":2})),
        ("moment_at", json!({"time":"4m"})),
        (
            "get_moment",
            json!({"moment":"this:3", "include_lines":true}),
        ),
        ("get_screenshot", json!({"moment":"this:3"})),
        ("list_gaps", json!({"since":"1h"})),
        ("list_sources", json!({})),
        ("get_status", json!({})),
        ("pause_recording", json!({"minutes":10})),
    ];
    let mut messages = vec![json!({"jsonrpc":"2.0", "id":0, "method":"tools/list"})];
    messages.extend(calls.iter().enumerate().map(|(id, (name, args))| json!({
        "jsonrpc":"2.0", "id":id+1, "method":"tools/call", "params":{"name":name, "arguments":args}})));
    let replies = session(data.root(), &messages)?;
    let tools = replies[0]["result"]["tools"].as_array().ok_or("tools")?;
    assert_eq!(tools.len(), calls.len());
    for (index, (name, _)) in calls.iter().enumerate() {
        assert!(
            tools.iter().any(|tool| tool["name"] == *name),
            "{name} missing"
        );
        assert_eq!(
            replies[index + 1]["result"]["isError"],
            false,
            "{name}: {}",
            replies[index + 1]
        );
    }
    assert_eq!(
        replies[1]["result"]["structuredContent"]["moments"][0]["moment"],
        "this:3"
    );
    assert_eq!(
        replies[2]["result"]["structuredContent"]["moments"]
            .as_array()
            .map(Vec::len),
        Some(2)
    );
    assert_eq!(
        replies[4]["result"]["structuredContent"]["text"],
        "error: linker failed with exit code 1"
    );
    assert_eq!(replies[5]["result"]["content"][0]["type"], "image");
    // Protective action shares the CLI's monotone pause semantics and persisted control table.
    let status = Command::new(env!("CARGO_BIN_EXE_rsrewind"))
        .arg("--data-dir")
        .arg(data.root())
        .args(["status", "--json"])
        .output()?;
    assert_eq!(
        serde_json::from_slice::<Value>(&status.stdout)?["state"],
        "paused"
    );
    Ok(())
}

#[test]
fn disabling_an_open_connection_revokes_history_immediately() -> Fallible<()> {
    let (_dir, data) = fixture::history()?;
    let path = data.root().join("config.toml");
    std::fs::write(&path, "[mcp]\nenabled = true\n")?;
    let mut child = Command::new(env!("CARGO_BIN_EXE_rsrewind"))
        .arg("--data-dir")
        .arg(data.root())
        .arg("mcp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let result = (|| -> Fallible<()> {
        let mut input = child.stdin.take().ok_or("stdin")?;
        let mut output = BufReader::new(child.stdout.take().ok_or("stdout")?);
        writeln!(input, "{}", initialize("init"))?;
        let mut line = String::new();
        output.read_line(&mut line)?;
        assert!(serde_json::from_str::<Value>(&line)?["result"].is_object());
        writeln!(
            input,
            "{}",
            json!({"jsonrpc":"2.0","method":"notifications/initialized"})
        )?;
        for (id, enabled) in [(1, true), (2, false), (3, true)] {
            std::fs::write(&path, format!("[mcp]\nenabled = {enabled}\n"))?;
            writeln!(
                input,
                "{}",
                json!({"jsonrpc":"2.0","id":id,"method":"tools/call",
                "params":{"name":"search","arguments":{"query":"linker"}}})
            )?;
            line.clear();
            output.read_line(&mut line)?;
            let reply: Value = serde_json::from_str(&line)?;
            assert_eq!(reply["result"]["isError"], !enabled, "{reply}");
        }
        std::fs::write(&path, "malformed [toml")?;
        writeln!(
            input,
            "{}",
            json!({"jsonrpc":"2.0","id":4,"method":"tools/list"})
        )?;
        line.clear();
        output.read_line(&mut line)?;
        let reply: Value = serde_json::from_str(&line)?;
        assert_eq!(reply["result"]["tools"].as_array().map(Vec::len), Some(1));
        drop(input);
        Ok(())
    })();
    let _ = child.kill();
    let _ = child.wait();
    result
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
