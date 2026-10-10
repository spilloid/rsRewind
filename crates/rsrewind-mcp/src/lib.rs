//! `rsrewind mcp`: screen history as a Model Context Protocol server (docs/design/mcp.md).
//!
//! stdio only: the MCP client starts `rsrewind mcp` and speaks newline-delimited JSON-RPC 2.0 on its
//! stdin/stdout, so rsRewind opens no socket. Reads go through `rsrewind-query`'s `History` facade
//! (a read-only connection); the one action, pausing, runs `rsrewind pause --extend-only` like the
//! tray runs the CLI. What any tool may return is bounded by the `[mcp]` ceiling in `config.toml`,
//! **re-read on every request** (so `rsrewind mcp --disable` takes effect on open connections) and
//! applied when tools are listed and again when one is called. A config that cannot be read means
//! access is off. Off by default: an MCP client sends tool results to its model, which usually runs
//! off this computer.
//!
//! Logging: each call logs its tool name, a server-generated call number, result count and duration.
//! Never recognized text, titles, pixels, or anything the client sent (CLAUDE.md logging rule).

pub mod ceiling;
mod tools;

pub use tools::all_names as tool_names;

use ceiling::Ceiling;
use rsrewind_core::{Config, DataDir, McpConfig, Timestamp};
use serde_json::{Value, json};
use std::io::{BufRead, Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

/// Protocol versions this server speaks, newest first. The requested one is echoed when supported.
/// (2025-03-26 is the one that requires accepting JSON-RPC batches, which `serve` does.)
pub const PROTOCOL_VERSIONS: [&str; 4] = ["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];

/// A single message larger than this is refused without reading it into memory.
pub const MAX_MESSAGE_BYTES: usize = 1024 * 1024;

/// Where the `[mcp]` settings come from.
pub enum Settings {
    /// Read `config.toml` again for every request; unreadable or invalid means access off.
    File(PathBuf),
    /// Fixed settings (tests).
    Fixed(McpConfig),
}

/// One server: a data folder, its settings, and the `rsrewind` executable for the actions that go
/// through the CLI (`status`, `pause`). `exe` is `None` where no actions are needed.
pub struct McpServer {
    pub data: DataDir,
    pub settings: Settings,
    pub exe: Option<PathBuf>,
    pub version: &'static str,
    calls: AtomicU64,
}

impl McpServer {
    pub fn new(
        data: DataDir,
        settings: Settings,
        exe: Option<PathBuf>,
        version: &'static str,
    ) -> Self {
        Self {
            data,
            settings,
            exe,
            version,
            calls: AtomicU64::new(0),
        }
    }

    /// The settings in force now. Fails closed: an unreadable or invalid `config.toml` reads as
    /// access off.
    pub fn config(&self) -> McpConfig {
        match &self.settings {
            Settings::Fixed(config) => config.clone(),
            Settings::File(path) => match Config::load_or_default(path) {
                Ok(config) => config.mcp,
                Err(error) => {
                    tracing::warn!(%error, "config.toml unreadable: agent access off until it is fixed");
                    McpConfig::default()
                }
            },
        }
    }

    /// Handles one JSON-RPC message; `None` for notifications and client responses (no reply).
    pub fn handle(&self, message: &Value) -> Option<Value> {
        let Some(obj) = message.as_object() else {
            return Some(invalid("a JSON-RPC message must be an object"));
        };
        let id = obj.get("id");
        if obj.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
            return Some(invalid("jsonrpc must be \"2.0\""));
        }
        let Some(method) = obj.get("method").and_then(Value::as_str) else {
            // A response from the client (we never send requests) needs no reply; anything else is invalid.
            return if id.is_some() && (obj.contains_key("result") || obj.contains_key("error")) {
                None
            } else {
                Some(invalid("a request needs a method"))
            };
        };
        let Some(id) = id else {
            return None; // a notification
        };
        let id_ok = id.is_string() || id.is_i64() || id.is_u64();
        if !id_ok {
            return Some(invalid("an id must be a string or an integer"));
        }
        let params = obj.get("params").cloned().unwrap_or(Value::Null);
        if !params.is_null() && !params.is_object() {
            return Some(json!({ "jsonrpc": "2.0", "id": id,
                "error": { "code": -32602, "message": "params must be an object" } }));
        }
        let result = match method {
            "initialize" => {
                if params
                    .get("protocolVersion")
                    .and_then(Value::as_str)
                    .is_none()
                    || !params.get("capabilities").is_some_and(Value::is_object)
                    || params
                        .pointer("/clientInfo/name")
                        .and_then(Value::as_str)
                        .is_none()
                    || params
                        .pointer("/clientInfo/version")
                        .and_then(Value::as_str)
                        .is_none()
                {
                    Err((
                        -32602,
                        "initialize needs protocolVersion, capabilities and clientInfo".into(),
                    ))
                } else {
                    Ok(self.initialize(&params))
                }
            }
            "ping" => Ok(json!({})),
            "tools/list" => {
                let ceiling = Ceiling::from_config(&self.config(), Timestamp::now());
                Ok(json!({ "tools": tools::list(&ceiling) }))
            }
            "tools/call" => self.call(&params),
            other => Err((-32601, format!("method not found: {}", clip(other, 64)))),
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
        let instructions = if self.config().enabled {
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

    fn call(&self, params: &Value) -> Result<Value, (i64, String)> {
        let name = params
            .get("name")
            .and_then(Value::as_str)
            .ok_or((-32602, "tools/call needs a tool name".to_string()))?;
        let args = match params.get("arguments") {
            None | Some(Value::Null) => json!({}),
            Some(Value::Object(map)) => Value::Object(map.clone()),
            Some(_) => return Err((-32602, "arguments must be an object".to_string())),
        };
        let call_number = self.calls.fetch_add(1, Ordering::Relaxed) + 1;
        let started = std::time::Instant::now();
        let ceiling = Ceiling::from_config(&self.config(), Timestamp::now());
        let (result, count, tool, decision) = match tools::call(self, &ceiling, name, &args) {
            Ok(done) => (done.result, done.count, done.tool, "executed"),
            Err(tools::ToolError::Unknown) => {
                tracing::info!(
                    channel = "mcp",
                    principal = "os_process_owner",
                    process_id = std::process::id(),
                    call = call_number,
                    tool = "unknown",
                    decision = "refused",
                    "mcp call"
                );
                return Err((-32602, format!("unknown tool: {}", clip(name, 64))));
            }
            Err(tools::ToolError::Refused(tool, message)) => {
                (tools::error_result(&message), 0, tool, "refused")
            }
        };
        // Only a known tool's own (static) name, a server-side counter and numbers: nothing the client sent.
        tracing::info!(
            tool,
            channel = "mcp",
            principal = "os_process_owner",
            process_id = std::process::id(),
            decision,
            call = call_number,
            results = count,
            ms = started.elapsed().as_millis() as u64,
            "mcp call"
        );
        Ok(result)
    }

    fn handle_payload(&self, payload: &Value, session: &mut Session) -> Option<Value> {
        match payload {
            Value::Array(items) if items.is_empty() => Some(invalid("an empty batch")),
            Value::Array(_) if session.version != Some("2025-03-26") => {
                Some(invalid("batches require negotiated protocol 2025-03-26"))
            }
            Value::Array(items) => {
                let replies: Vec<Value> = items
                    .iter()
                    .filter_map(|m| session.handle(self, m))
                    .collect();
                (!replies.is_empty()).then_some(Value::Array(replies))
            }
            other => session.handle(self, other),
        }
    }
}

/// Lifecycle belongs to the transport connection, rather than the history/query dispatcher.
#[derive(Default)]
struct Session {
    version: Option<&'static str>,
    ready: bool,
}

impl Session {
    fn handle(&mut self, server: &McpServer, message: &Value) -> Option<Value> {
        let method = message.get("method").and_then(Value::as_str);
        let valid = message.get("jsonrpc").and_then(Value::as_str) == Some("2.0");
        if valid && method == Some("notifications/initialized") && message.get("id").is_none() {
            self.ready = self.version.is_some();
            return None;
        }
        if valid
            && message.get("id").is_some()
            && ((method == Some("initialize") && self.version.is_some())
                || (method.is_some()
                    && !self.ready
                    && !matches!(method, Some("initialize" | "ping"))))
        {
            return Some(json!({ "jsonrpc": "2.0", "id": message["id"],
                "error": { "code": -32600, "message": "complete initialization once before using tools" } }));
        }
        let reply = server.handle(message);
        if method == Some("initialize")
            && let Some(version) = reply
                .as_ref()
                .and_then(|r| r.pointer("/result/protocolVersion"))
                .and_then(Value::as_str)
        {
            self.version = PROTOCOL_VERSIONS.iter().copied().find(|v| *v == version);
        }
        reply
    }
}

fn invalid(message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": null, "error": { "code": -32600, "message": message } })
}

fn clip(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}

/// Serves newline-delimited JSON-RPC from `input` to `output` until `input` ends. Each message is
/// read with a size limit; an oversized one is skipped (and answered with an error) without being
/// held in memory, and bytes that are not UTF-8 are a parse error, not the end of the session.
pub fn serve(
    server: &McpServer,
    mut input: impl BufRead,
    mut output: impl Write,
) -> std::io::Result<()> {
    let mut buf = Vec::with_capacity(4096);
    let mut session = Session::default();
    loop {
        buf.clear();
        let read = (&mut input)
            .take(MAX_MESSAGE_BYTES as u64 + 1)
            .read_until(b'\n', &mut buf)?;
        if read == 0 {
            return Ok(());
        }
        let reply = if buf.len() > MAX_MESSAGE_BYTES {
            // Discard the rest of this line in bounded chunks.
            while !buf.ends_with(b"\n") {
                buf.clear();
                let n = (&mut input).take(64 * 1024).read_until(b'\n', &mut buf)?;
                if n == 0 || buf.ends_with(b"\n") {
                    break;
                }
            }
            Some(json!({ "jsonrpc": "2.0", "id": null,
                "error": { "code": -32600, "message": format!("message larger than {MAX_MESSAGE_BYTES} bytes") } }))
        } else {
            match std::str::from_utf8(&buf) {
                Ok(text) if text.trim().is_empty() => None,
                Ok(text) => match serde_json::from_str::<Value>(text) {
                    Ok(payload) => server.handle_payload(&payload, &mut session),
                    Err(_) => Some(json!({ "jsonrpc": "2.0", "id": null,
                        "error": { "code": -32700, "message": "parse error" } })),
                },
                Err(_) => Some(json!({ "jsonrpc": "2.0", "id": null,
                    "error": { "code": -32700, "message": "parse error: not UTF-8" } })),
            }
        };
        if let Some(reply) = reply {
            serde_json::to_writer(&mut output, &reply)?;
            output.write_all(b"\n")?;
            output.flush()?;
        }
    }
}
