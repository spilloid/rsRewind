//! The MCP server through the protocol itself, AnchorDesk-style: JSON-RPC lines in, JSON-RPC lines out,
//! over a real SQLite store written by `rsrewind-storage` exactly as the recorder writes it. This catches a
//! tool missing from `tools/list`, a schema or annotation drift, a ceiling that only filters discovery,
//! and content leaking into the log.

mod common;

use common::*;
use rsrewind_core::McpConfig;
use rsrewind_mcp::{PROTOCOL_VERSIONS, serve};
use serde_json::{Value, json};

#[test]
fn initialize_negotiates_the_version_and_names_the_server() -> Fallible<()> {
    let (_dir, data) = history()?;
    let s = server(&data, enabled());
    let replies = exchange(
        &s,
        &[
            json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize",
                    "params": { "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": { "name": "t", "version": "1" } } }),
            json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
            json!({ "jsonrpc": "2.0", "id": 3, "method": "ping" }),
        ],
    )?;
    assert_eq!(
        replies.len(),
        2,
        "the notification gets no reply: {replies:?}"
    );
    assert_eq!(replies[0]["result"]["protocolVersion"], "2025-06-18");
    assert_eq!(replies[0]["result"]["serverInfo"]["name"], "rsrewind");
    assert_eq!(replies[0]["result"]["serverInfo"]["version"], "9.9.9-test");
    assert_eq!(
        replies[0]["result"]["capabilities"]["tools"]["listChanged"],
        false
    );
    let negotiated = exchange(
        &s,
        &[json!({ "jsonrpc":"2.0", "id":2,"method":"initialize",
        "params":{"protocolVersion":"1999-01-01", "capabilities":{},"clientInfo":{"name":"t","version":"1"}}})],
    )?;
    assert_eq!(
        negotiated[0]["result"]["protocolVersion"], PROTOCOL_VERSIONS[0],
        "unknown version: offer the newest"
    );
    assert_eq!(replies[1]["result"], json!({}));
    Ok(())
}

#[test]
fn the_tool_contract_is_exact_and_annotated() -> Fallible<()> {
    let (_dir, data) = history()?;
    let all = server(
        &data,
        McpConfig {
            enabled: true,
            allow_screenshots: true,
            ..McpConfig::default()
        },
    );
    let replies = exchange(
        &all,
        &[json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" })],
    )?;
    let tools = replies[0]["result"]["tools"].as_array().ok_or("tools")?;
    let names: Vec<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();
    assert_eq!(
        names,
        [
            "search",
            "recent",
            "moment_at",
            "get_moment",
            "get_screenshot",
            "list_gaps",
            "list_sources",
            "get_status",
            "pause_recording"
        ]
    );
    for tool in tools {
        assert_eq!(tool["inputSchema"]["type"], "object", "{tool}");
        assert_eq!(tool["inputSchema"]["additionalProperties"], false, "{tool}");
        assert!(
            tool["description"].as_str().is_some_and(|d| d.len() > 30),
            "{tool}"
        );
        let a = &tool["annotations"];
        assert_eq!(
            a["destructiveHint"], false,
            "no destructive tool is exposed: {tool}"
        );
        assert_eq!(a["openWorldHint"], false);
        let read_only = tool["name"] != "pause_recording";
        assert_eq!(a["readOnlyHint"], read_only, "{tool}");
    }
    // Nothing that turns recording back on, deletes or moves history.
    for forbidden in [
        "resume", "forget", "delete", "export", "import", "start", "stop",
    ] {
        assert!(!names.iter().any(|n| n.contains(forbidden)), "{forbidden}");
    }
    Ok(())
}

#[test]
fn off_by_default_lists_and_allows_only_status() -> Fallible<()> {
    let (_dir, data) = history()?;
    let off = server(&data, McpConfig::default());
    assert_eq!(tool_names(&off)?, ["get_status"]);
    let refused = call(&off, "search", json!({ "query": "Blue" }))?;
    assert_eq!(
        refused["isError"], true,
        "hidden is also refused: {refused}"
    );
    assert!(
        refused["content"][0]["text"]
            .as_str()
            .is_some_and(|t| t.contains("rsrewind mcp --enable"))
    );
    let status = call(&off, "get_status", json!({}))?;
    assert_eq!(status["isError"], false);
    assert_eq!(
        status["structuredContent"]["agent_access"]["enabled"],
        false
    );
    Ok(())
}

#[test]
fn search_then_moment_then_screenshot() -> Fallible<()> {
    let (_dir, data) = history()?;
    let s = server(&data, enabled());
    let found = call(&s, "search", json!({ "query": "\"Blue Screen Babe\"" }))?;
    assert_eq!(found["isError"], false, "{found}");
    let moments = found["structuredContent"]["moments"]
        .as_array()
        .ok_or("moments")?;
    assert_eq!(moments.len(), 1, "{found}");
    assert_eq!(moments[0]["app"], "firefox");
    assert_eq!(moments[0]["machine"], "this machine");
    let id = moments[0]["moment"].as_str().ok_or("id")?.to_string();
    assert!(id.starts_with("this:"), "{id}");

    let moment = call(
        &s,
        "get_moment",
        json!({ "moment": id, "include_lines": true }),
    )?;
    assert_eq!(
        moment["structuredContent"]["text"],
        "Blue Screen Babe 100 Servings $40"
    );
    assert_eq!(moment["structuredContent"]["lines"][0]["x"], 1.0);

    // Screenshots are off unless allowed: refused even when called directly.
    let refused = call(&s, "get_screenshot", json!({ "moment": id }))?;
    assert_eq!(refused["isError"], true);
    let pics = server(
        &data,
        McpConfig {
            allow_screenshots: true,
            ..enabled()
        },
    );
    let picture = call(&pics, "get_screenshot", json!({ "moment": id }))?;
    assert_eq!(picture["content"][0]["type"], "image");
    assert_eq!(picture["content"][0]["mimeType"], "image/webp");
    assert!(
        picture["content"][0]["data"]
            .as_str()
            .is_some_and(|d| d.starts_with("UklGR")),
        "base64 of a RIFF/WebP file"
    );
    Ok(())
}

#[test]
fn the_time_window_bounds_every_tool() -> Fallible<()> {
    let (_dir, data) = history()?;
    let s = server(&data, enabled()); // 30 days
    let old = call(&s, "search", json!({ "query": "invoice" }))?;
    assert_eq!(
        old["structuredContent"]["moments"],
        json!([]),
        "40-day-old text is out of reach"
    );
    let recent = call(&s, "recent", json!({ "limit": 10 }))?;
    assert_eq!(
        recent["structuredContent"]["moments"]
            .as_array()
            .map(Vec::len),
        Some(2)
    );
    let open = server(
        &data,
        McpConfig {
            max_age_days: 0,
            ..enabled()
        },
    );
    let all = call(&open, "recent", json!({ "limit": 10 }))?;
    assert_eq!(
        all["structuredContent"]["moments"].as_array().map(Vec::len),
        Some(3)
    );
    // The old moment, by id, is refused under the window.
    let old_id = all["structuredContent"]["moments"][2]["moment"]
        .as_str()
        .ok_or("id")?
        .to_string();
    let refused = call(&s, "get_moment", json!({ "moment": old_id }))?;
    assert_eq!(refused["isError"], true, "{refused}");
    let at = call(&s, "moment_at", json!({ "time": "4m" }))?;
    assert_eq!(at["structuredContent"]["moment"]["app"], "konsole");
    Ok(())
}

#[test]
fn a_machine_allowlist_excludes_everything_else() -> Fallible<()> {
    let (_dir, data) = history()?;
    let other = "0123456789abcdef0123456789abcdef".to_string();
    let s = server(
        &data,
        McpConfig {
            sources: vec![other],
            ..enabled()
        },
    );
    let hits = call(&s, "search", json!({ "query": "error" }))?;
    assert_eq!(hits["structuredContent"]["moments"], json!([]));
    assert_eq!(
        call(&s, "list_sources", json!({}))?["structuredContent"]["machines"],
        json!([])
    );
    let refused = call(&s, "get_moment", json!({ "moment": "this:1" }))?;
    assert_eq!(refused["isError"], true);
    let mine = server(
        &data,
        McpConfig {
            sources: vec!["this".into()],
            ..enabled()
        },
    );
    let hits = call(&mine, "search", json!({ "query": "error" }))?;
    assert_eq!(
        hits["structuredContent"]["moments"]
            .as_array()
            .map(Vec::len),
        Some(1)
    );
    Ok(())
}

#[test]
fn gaps_and_sources_answer() -> Fallible<()> {
    let (_dir, data) = history()?;
    let s = server(&data, enabled());
    let sources = call(&s, "list_sources", json!({}))?;
    assert_eq!(
        sources["structuredContent"]["machines"][0]["machine"],
        "this"
    );
    let gaps = call(&s, "list_gaps", json!({ "since": "1h", "min_minutes": 5 }))?;
    assert_eq!(gaps["isError"], false, "{gaps}");
    assert!(
        gaps["structuredContent"]["gaps"]
            .as_array()
            .is_some_and(|g| !g.is_empty()),
        "{gaps}"
    );
    Ok(())
}

#[test]
fn protocol_errors_are_errors_and_tool_errors_are_results() -> Fallible<()> {
    let (_dir, data) = history()?;
    let s = server(&data, enabled());
    let mut out = Vec::new();
    serve(&s, "not json\n[1,2]\n{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"nope\"}\n{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/call\",\"params\":{\"name\":\"drop_tables\"}}\n".as_bytes(), &mut out)?;
    let replies: Vec<Value> = String::from_utf8(out)?
        .lines()
        .map(serde_json::from_str)
        .collect::<Result<_, _>>()?;
    assert_eq!(replies[0]["error"]["code"], -32700);
    assert_eq!(replies[1]["error"]["code"], -32600);
    assert_eq!(replies[2]["error"]["code"], -32600);
    assert_eq!(replies[3]["error"]["code"], -32600);
    let bad_time = call(&s, "moment_at", json!({ "time": "around lunch" }))?;
    assert_eq!(bad_time["isError"], true);
    let bad_id = call(&s, "get_moment", json!({ "moment": "../../etc/passwd" }))?;
    assert_eq!(bad_id["isError"], true);
    Ok(())
}
