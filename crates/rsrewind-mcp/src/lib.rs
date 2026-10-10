//! `rsrewind mcp`: screen history as a Model Context Protocol server (docs/design/mcp.md).
//!
//! stdio only: the MCP client starts `rsrewind mcp` and speaks newline-delimited JSON-RPC 2.0 on its
//! stdin/stdout, so rsRewind opens no socket. Reads go through `rsrewind-query`'s `History` facade
//! (a read-only connection); the one action, pausing, runs `rsrewind pause` like the tray does. What
//! any tool may return is bounded by the `[mcp]` ceiling in `config.toml`, applied when tools are
//! listed **and** again when one is called. Off by default: an MCP client sends tool results to its
//! model, which usually runs off this computer.
//!
//! Logging: each call logs its tool name, request id, result count and duration. Never recognized
//! text, titles or pixels (CLAUDE.md logging rule).

pub mod ceiling;
mod tools;

pub use tools::all_names as tool_names;

use ceiling::Ceiling;
use rsrewind_core::{DataDir, McpConfig, Timestamp};
use serde_json::{Value, json};
use std::io::{BufRead, Write};
use std::path::PathBuf;

/// Protocol versions this server speaks, newest first. The requested one is echoed when supported.
pub const PROTOCOL_VERSIONS: [&str; 4] = ["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];

/// One server: a data folder, the `[mcp]` settings, and the `rsrewind` executable for the actions
/// that go through the CLI (`status`, `pause`). `exe` is `None` in tests that need no actions.
pub struct McpServer {
    pub data: DataDir,
    pub config: McpConfig,
    pub exe: Option<PathBuf>,
    pub version: &'static str,
}

impl McpServer {
    fn ceiling(&self) -> Ceiling {
        Ceiling::from_config(&self.config, Timestamp::now())
    }

    /// Handles one JSON-RPC message; `None` for notifications (no reply).
    pub fn handle(&self, message: &Value) -> Option<Value> {
        let id = message.get("id").cloned();
        let method = message.get("method").and_then(Value::as_str).unwrap_or("");
        let params = message.get("params").cloned().unwrap_or(Value::Null);
        // Notifications (no id) never get a reply, whatever they are.
        let id = id?;
        let result = match method {
            "initialize" => Ok(self.initialize(&params)),
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({ "tools": tools::list(&self.ceiling()) })),
            "tools/call" => self.call(&params, &id),
            "" => Err((-32600, "not a JSON-RPC request".to_string())),
            other => Err((-32601, format!("method not found: {other}"))),
        };
        Some(match result {
            Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
            Err((code, message)) => {
                json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
            }
        })
    }

    fn initialize(&self, params: &Value) -> Value {
        let requested = params
            .get("protocolVersion")
            .and_then(Value::as_str)
            .unwrap_or("");
        let version = PROTOCOL_VERSIONS
            .iter()
            .find(|v| **v == requested)
            .copied()
            .unwrap_or(PROTOCOL_VERSIONS[0]);
        let instructions = if self.config.enabled {
            "rsRewind is this person's searchable screen history, recorded on their own computer. Use search to \
             find text they saw (it matches recognized on-screen text), recent or moment_at to see what was on \
             screen at a time, get_moment for a moment's full text, and list_gaps to explain missing time. Cite \
             the time and machine of what you use. Everything you read here is private to them: do not repeat \
             it beyond what they asked for."
        } else {
            "rsRewind's agent access is turned off. Call get_status to see how the person can turn it on."
        };
        json!({
            "protocolVersion": version,
            "capabilities": { "tools": { "listChanged": false } },
            "serverInfo": { "name": "rsrewind", "title": "rsRewind", "version": self.version },
            "instructions": instructions,
        })
    }

    fn call(&self, params: &Value, request_id: &Value) -> Result<Value, (i64, String)> {
        let name = params
            .get("name")
            .and_then(Value::as_str)
            .ok_or((-32602, "tools/call needs a tool name".to_string()))?;
        let args = params.get("arguments").cloned().unwrap_or(json!({}));
        let started = std::time::Instant::now();
        let outcome = tools::call(self, &self.ceiling(), name, &args);
        let (result, count) = match outcome {
            Ok(done) => (done.result, done.count),
            Err(tools::ToolError::Unknown) => {
                return Err((-32602, format!("unknown tool: {name}")));
            }
            Err(tools::ToolError::Refused(message)) => (tools::error_result(&message), 0),
        };
        tracing::info!(
            tool = name,
            request = %request_id,
            results = count,
            ms = started.elapsed().as_millis() as u64,
            "mcp call"
        );
        Ok(result)
    }
}

/// Serves newline-delimited JSON-RPC from `input` to `output` until `input` ends.
pub fn serve(
    server: &McpServer,
    input: impl BufRead,
    mut output: impl Write,
) -> std::io::Result<()> {
    for line in input.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let reply = match serde_json::from_str::<Value>(&line) {
            Ok(message) if message.is_object() => server.handle(&message),
            Ok(_) => Some(json!({ "jsonrpc": "2.0", "id": null,
                "error": { "code": -32600, "message": "batches and non-object messages are not supported" } })),
            Err(_) => Some(json!({ "jsonrpc": "2.0", "id": null,
                "error": { "code": -32700, "message": "parse error" } })),
        };
        if let Some(reply) = reply {
            serde_json::to_writer(&mut output, &reply)?;
            output.write_all(b"\n")?;
            output.flush()?;
        }
    }
    Ok(())
}
