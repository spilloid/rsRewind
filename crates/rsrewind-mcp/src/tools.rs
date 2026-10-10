//! The tools: definitions (name, schema, annotations) and handlers. Every handler re-checks the
//! ceiling; `list` filtering is only discovery.

use crate::McpServer;
use crate::ceiling::{self, Ceiling, moment_id, parse_moment_id, parse_time, time_text};
use rsrewind_core::{SearchQuery, SourceId, TimelineCursor, Timestamp};
use rsrewind_query::{History, SourceKind};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::process::{Command, Stdio};

/// Recognized text longer than this is cut, with a marker, in `get_moment`.
const MAX_TEXT_CHARS: usize = 20_000;
/// Pictures larger than this are refused rather than sent.
const MAX_SCREENSHOT_BYTES: usize = 4 * 1024 * 1024;

pub enum ToolError {
    Unknown,
    /// A tool-level failure the agent should read (returned as `isError: true`, not a protocol error).
    Refused(String),
}

pub struct Done {
    pub result: Value,
    pub count: usize,
}

type ToolResult = Result<Done, ToolError>;

fn refused(message: impl Into<String>) -> ToolError {
    ToolError::Refused(message.into())
}

pub fn error_result(message: &str) -> Value {
    json!({ "content": [{ "type": "text", "text": message }], "isError": true })
}

fn ok(text: String, structured: Value, count: usize) -> ToolResult {
    Ok(Done {
        result: json!({
            "content": [{ "type": "text", "text": text }],
            "structuredContent": structured,
            "isError": false,
        }),
        count,
    })
}

struct Def {
    name: &'static str,
    title: &'static str,
    description: &'static str,
    schema: fn() -> Value,
    read_only: bool,
    idempotent: bool,
}

const TIME_HELP: &str =
    "RFC 3339, 'YYYY-MM-DD HH:MM' (local), 'YYYY-MM-DD', or a duration ago like 30m, 2h, 7d";

fn defs() -> Vec<Def> {
    vec![
        Def {
            name: "search",
            title: "Search screen history",
            description: "Find moments whose on-screen text matches. Quote a phrase for an exact match; end a word with * \
                          for a prefix. Filter by application, window title and time. Returns moments with their time, \
                          machine, app, window and a snippet; pass a moment to get_moment for its full text.",
            schema: || {
                json!({ "type": "object", "required": ["query"], "properties": {
                    "query": { "type": "string", "description": "Words that were on screen" },
                    "app": { "type": "string", "description": "Only this application (process name, e.g. firefox or Teams.exe)" },
                    "title": { "type": "string", "description": "Only windows whose title contains this" },
                    "since": { "type": "string", "description": TIME_HELP },
                    "until": { "type": "string", "description": TIME_HELP },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 50, "default": 10 }
                }, "additionalProperties": false })
            },
            read_only: true,
            idempotent: true,
        },
        Def {
            name: "recent",
            title: "Recent moments",
            description: "The most recent moments, newest first, or those at or before a time. Each is one stretch of time \
                          a screen showed the same picture.",
            schema: || {
                json!({ "type": "object", "properties": {
                    "before": { "type": "string", "description": format!("Start at or before this time ({TIME_HELP})") },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 100, "default": 20 }
                }, "additionalProperties": false })
            },
            read_only: true,
            idempotent: true,
        },
        Def {
            name: "moment_at",
            title: "What was on screen at a time",
            description: "The moment on screen at a given time (or the nearest one before it), on any machine.",
            schema: || {
                json!({ "type": "object", "required": ["time"], "properties": {
                    "time": { "type": "string", "description": TIME_HELP }
                }, "additionalProperties": false })
            },
            read_only: true,
            idempotent: true,
        },
        Def {
            name: "get_moment",
            title: "Moment details",
            description: "Everything about one moment: time, machine, app, window, and the full recognized text (optionally \
                          with each line's position on screen).",
            schema: || {
                json!({ "type": "object", "required": ["moment"], "properties": {
                    "moment": { "type": "string", "description": "A moment id from search, recent or moment_at (e.g. this:123)" },
                    "include_lines": { "type": "boolean", "default": false, "description": "Also return each text line with its box in screen pixels" }
                }, "additionalProperties": false })
            },
            read_only: true,
            idempotent: true,
        },
        Def {
            name: "get_screenshot",
            title: "Moment screenshot",
            description: "The picture of one moment (WebP). Only available when the person allowed screenshots for agents.",
            schema: || {
                json!({ "type": "object", "required": ["moment"], "properties": {
                    "moment": { "type": "string", "description": "A moment id (e.g. this:123)" }
                }, "additionalProperties": false })
            },
            read_only: true,
            idempotent: true,
        },
        Def {
            name: "list_gaps",
            title: "Gaps in recording",
            description: "Stretches with nothing recorded and why: recorder off, stopped unexpectedly, paused, idle or locked, \
                          or nothing stored while recording. A screen that did not change is never a gap.",
            schema: || {
                json!({ "type": "object", "properties": {
                    "since": { "type": "string", "description": format!("Default 24h. {TIME_HELP}") },
                    "until": { "type": "string", "description": format!("Default now. {TIME_HELP}") },
                    "min_minutes": { "type": "integer", "minimum": 1, "default": 1 }
                }, "additionalProperties": false })
            },
            read_only: true,
            idempotent: true,
        },
        Def {
            name: "list_sources",
            title: "Machines in this history",
            description: "The machines whose history is here (this computer and any imported ones), with their time spans.",
            schema: || json!({ "type": "object", "properties": {}, "additionalProperties": false }),
            read_only: true,
            idempotent: true,
        },
        Def {
            name: "get_status",
            title: "rsRewind status",
            description: "Whether rsRewind is recording, paused or stopped, how much text recognition is waiting, and what \
                          agents may access (and how the person can change that).",
            schema: || json!({ "type": "object", "properties": {}, "additionalProperties": false }),
            read_only: true,
            idempotent: true,
        },
        Def {
            name: "pause_recording",
            title: "Pause recording",
            description: "Pause recording, for some minutes or until the person resumes it. Agents cannot resume recording \
                          or delete history; those stay with the person.",
            schema: || {
                json!({ "type": "object", "properties": {
                    "minutes": { "type": "integer", "minimum": 1, "maximum": 1440, "description": "Omit to pause until resumed" }
                }, "additionalProperties": false })
            },
            read_only: false,
            idempotent: true,
        },
    ]
}

/// Every tool name, whatever the ceiling (for parity checks).
pub fn all_names() -> Vec<&'static str> {
    defs().into_iter().map(|d| d.name).collect()
}

/// Whether the ceiling lets this tool be listed and called; `Err` says why not.
fn allowed(ceiling: &Ceiling, name: &str) -> Result<(), String> {
    if name == "get_status" {
        return Ok(());
    }
    if !ceiling.enabled {
        return Err("rsRewind's agent access is turned off. The person can turn it on with `rsrewind mcp --enable` \
                    (which explains what is shared), then restart this connection."
            .into());
    }
    if name == "get_screenshot" && !ceiling.allow_screenshots {
        return Err(
            "Screenshots are not shared with agents. The person can allow them by setting \
                    `allow_screenshots = true` under [mcp] in rsRewind's config.toml."
                .into(),
        );
    }
    Ok(())
}

/// The advertised tools for this ceiling.
pub fn list(ceiling: &Ceiling) -> Vec<Value> {
    defs()
        .into_iter()
        .filter(|d| allowed(ceiling, d.name).is_ok())
        .map(|d| {
            json!({
                "name": d.name,
                "title": d.title,
                "description": d.description,
                "inputSchema": (d.schema)(),
                "annotations": {
                    "title": d.title,
                    "readOnlyHint": d.read_only,
                    "destructiveHint": false,
                    "idempotentHint": d.idempotent,
                    "openWorldHint": false,
                },
            })
        })
        .collect()
}

pub fn call(server: &McpServer, ceiling: &Ceiling, name: &str, args: &Value) -> ToolResult {
    if !defs().iter().any(|d| d.name == name) {
        return Err(ToolError::Unknown);
    }
    allowed(ceiling, name).map_err(ToolError::Refused)?;
    let now = chrono::Local::now();
    match name {
        "search" => search(server, ceiling, args, now),
        "recent" => recent(server, ceiling, args, now),
        "moment_at" => moment_at(server, ceiling, args, now),
        "get_moment" => get_moment(server, ceiling, args),
        "get_screenshot" => get_screenshot(server, ceiling, args),
        "list_gaps" => list_gaps(server, ceiling, args, now),
        "list_sources" => list_sources(server, ceiling),
        "get_status" => get_status(server, ceiling),
        "pause_recording" => pause(server, args),
        _ => Err(ToolError::Unknown),
    }
}

fn history(server: &McpServer) -> History {
    History::open(&server.data)
}

fn read_err(e: impl std::fmt::Display) -> ToolError {
    refused(format!("could not read history: {e}"))
}

fn str_arg<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

fn time_arg(
    args: &Value,
    key: &str,
    now: chrono::DateTime<chrono::Local>,
) -> Result<Option<Timestamp>, ToolError> {
    str_arg(args, key)
        .map(|t| parse_time(t, now).map_err(ToolError::Refused))
        .transpose()
}

fn limit_arg(args: &Value, default: u32, max: u32) -> u32 {
    args.get("limit")
        .and_then(Value::as_u64)
        .map_or(default, |n| u32::try_from(n).unwrap_or(max))
        .clamp(1, max)
}

/// Display names: "this machine" or the probe's label (with its short id).
fn machines(history: &History) -> HashMap<Option<SourceId>, String> {
    history
        .sources()
        .unwrap_or_default()
        .into_iter()
        .map(|s| {
            let name = match (s.source, s.label) {
                (None, _) => "this machine".to_string(),
                (Some(id), Some(label)) => format!("{label} ({})", id.short()),
                (Some(id), None) => id.short(),
            };
            (s.source, name)
        })
        .collect()
}

fn machine(names: &HashMap<Option<SourceId>, String>, source: Option<SourceId>) -> String {
    names
        .get(&source)
        .cloned()
        .unwrap_or_else(|| source.map_or_else(|| "this machine".to_string(), |s| s.short()))
}

fn search(
    server: &McpServer,
    ceiling: &Ceiling,
    args: &Value,
    now: chrono::DateTime<chrono::Local>,
) -> ToolResult {
    let text = str_arg(args, "query").ok_or_else(|| refused("search needs a query"))?;
    let limit = limit_arg(args, 10, 50);
    let query = SearchQuery {
        text: text.to_string(),
        since: ceiling.clamp_since(time_arg(args, "since", now)?),
        until: time_arg(args, "until", now)?,
        application: str_arg(args, "app").map(String::from),
        title_contains: str_arg(args, "title").map(String::from),
        limit,
    };
    let history = history(server);
    let names = machines(&history);
    let mut hits = Vec::new();
    for filter in ceiling.filters() {
        hits.extend(history.search(&query, filter).map_err(read_err)?);
    }
    hits.retain(|h| ceiling.admits(h.source) && ceiling.admits_time(h.timestamp));
    hits.sort_by(|a, b| {
        a.rank
            .total_cmp(&b.rank)
            .then_with(|| b.timestamp.cmp(&a.timestamp))
    });
    hits.truncate(limit as usize);
    let items: Vec<Value> = hits
        .iter()
        .map(|h| {
            json!({
                "moment": moment_id(h.source, h.visual_state_id),
                "time": time_text(h.timestamp),
                "machine": machine(&names, h.source),
                "app": h.application,
                "window": h.window_title,
                "snippet": h.snippet,
            })
        })
        .collect();
    let text = if hits.is_empty() {
        format!("No moments match \"{text}\".")
    } else {
        hits.iter()
            .map(|h| {
                format!(
                    "{} · {} · {} — {}\n  {}\n  moment {}",
                    time_text(h.timestamp),
                    machine(&names, h.source),
                    h.application.as_deref().unwrap_or("?"),
                    h.window_title.as_deref().unwrap_or(""),
                    h.snippet.split_whitespace().collect::<Vec<_>>().join(" "),
                    moment_id(h.source, h.visual_state_id)
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    let n = items.len();
    ok(text, json!({ "moments": items }), n)
}

fn entry_json(
    names: &HashMap<Option<SourceId>, String>,
    e: &rsrewind_core::TimelineEntry,
) -> Value {
    json!({
        "moment": moment_id(e.source, e.visual_state_id),
        "from": time_text(e.started_at),
        "until": time_text(e.ended_at),
        "machine": machine(names, e.source),
        "app": e.application,
        "window": e.window_title,
        "text_status": e.ocr_status,
    })
}

fn entry_line(
    names: &HashMap<Option<SourceId>, String>,
    e: &rsrewind_core::TimelineEntry,
) -> String {
    format!(
        "{} · {} · {} — {} (moment {})",
        time_text(e.started_at),
        machine(names, e.source),
        e.application.as_deref().unwrap_or("?"),
        e.window_title.as_deref().unwrap_or(""),
        moment_id(e.source, e.visual_state_id)
    )
}

fn recent(
    server: &McpServer,
    ceiling: &Ceiling,
    args: &Value,
    now: chrono::DateTime<chrono::Local>,
) -> ToolResult {
    let limit = limit_arg(args, 20, 100);
    let before = time_arg(args, "before", now)?.map(TimelineCursor::at_or_before);
    let history = history(server);
    let names = machines(&history);
    let mut entries = Vec::new();
    for filter in ceiling.filters() {
        entries.extend(history.recent(filter, limit, before).map_err(read_err)?);
    }
    entries.retain(|e| ceiling.admits(e.source) && ceiling.admits_time(e.started_at));
    entries.sort_by_key(|e| std::cmp::Reverse(e.cursor()));
    entries.truncate(limit as usize);
    let text = if entries.is_empty() {
        "Nothing recorded in the time agents may see.".to_string()
    } else {
        entries
            .iter()
            .map(|e| entry_line(&names, e))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let items: Vec<Value> = entries.iter().map(|e| entry_json(&names, e)).collect();
    let n = items.len();
    ok(text, json!({ "moments": items }), n)
}

fn moment_at(
    server: &McpServer,
    ceiling: &Ceiling,
    args: &Value,
    now: chrono::DateTime<chrono::Local>,
) -> ToolResult {
    let at = time_arg(args, "time", now)?.ok_or_else(|| refused("moment_at needs a time"))?;
    if !ceiling.admits_time(at) {
        return Err(refused("That time is older than agents may see."));
    }
    let history = history(server);
    let names = machines(&history);
    let mut best: Option<rsrewind_core::TimelineEntry> = None;
    for filter in ceiling.filters() {
        if let Some(e) = history.at(at, filter).map_err(read_err)?
            && ceiling.admits(e.source)
            && best.as_ref().is_none_or(|b| e.cursor() > b.cursor())
        {
            best = Some(e);
        }
    }
    match best.filter(|e| ceiling.admits_time(e.started_at)) {
        Some(e) => ok(
            entry_line(&names, &e),
            json!({ "moment": entry_json(&names, &e) }),
            1,
        ),
        None => ok(
            "Nothing was recorded at or before that time.".into(),
            json!({ "moment": null }),
            0,
        ),
    }
}

/// Looks a moment up and applies the machine and time ceiling to it.
fn checked_detail(
    history: &History,
    ceiling: &Ceiling,
    args: &Value,
) -> Result<(Option<SourceId>, rsrewind_core::VisualDetail), ToolError> {
    let text =
        str_arg(args, "moment").ok_or_else(|| refused("needs a moment id (e.g. this:123)"))?;
    let (source, id) =
        parse_moment_id(text).ok_or_else(|| refused(format!("'{text}' is not a moment id")))?;
    if !ceiling.admits(source) {
        return Err(refused("That machine's history is not shared with agents."));
    }
    let detail = history
        .visual_detail(source, id)
        .map_err(read_err)?
        .ok_or_else(|| refused(format!("No moment {text} (it may have been forgotten)")))?;
    if !ceiling.admits_time(detail.captured_at) {
        return Err(refused("That moment is older than agents may see."));
    }
    Ok((source, detail))
}

fn get_moment(server: &McpServer, ceiling: &Ceiling, args: &Value) -> ToolResult {
    let history = history(server);
    let names = machines(&history);
    let (source, d) = checked_detail(&history, ceiling, args)?;
    let mut text_out: String = d.ocr_text.chars().take(MAX_TEXT_CHARS).collect();
    let truncated = d.ocr_text.chars().count() > MAX_TEXT_CHARS;
    if truncated {
        text_out.push_str("\n[…text truncated]");
    }
    let include_lines = args
        .get("include_lines")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let lines: Option<Vec<Value>> = include_lines.then(|| {
        d.blocks
            .iter()
            .take(500)
            .map(|b| json!({ "text": b.text, "x": b.x, "y": b.y, "width": b.width, "height": b.height }))
            .collect()
    });
    let header = format!(
        "{} · {} · {} — {}\nscreen {}x{}, text {}",
        time_text(d.captured_at),
        machine(&names, source),
        d.application.as_deref().unwrap_or("?"),
        d.window_title.as_deref().unwrap_or(""),
        d.width,
        d.height,
        d.ocr_status
    );
    let summary = if text_out.is_empty() {
        format!("{header}\n(no recognized text)")
    } else {
        format!("{header}\n\n{text_out}")
    };
    ok(
        summary,
        json!({
            "moment": moment_id(source, d.visual_state_id),
            "time": time_text(d.captured_at),
            "machine": machine(&names, source),
            "app": d.application,
            "window": d.window_title,
            "monitor": d.monitor,
            "width": d.width,
            "height": d.height,
            "text_status": d.ocr_status,
            "text": text_out,
            "text_truncated": truncated,
            "lines": lines,
        }),
        1,
    )
}

fn get_screenshot(server: &McpServer, ceiling: &Ceiling, args: &Value) -> ToolResult {
    let history = history(server);
    let names = machines(&history);
    let (source, d) = checked_detail(&history, ceiling, args)?;
    let bytes = history
        .frame_bytes(source, &d.media_path)
        .map_err(read_err)?;
    if bytes.len() > MAX_SCREENSHOT_BYTES {
        return Err(refused("That picture is too large to send."));
    }
    let caption = format!(
        "{} · {} · {} — {} ({}x{})",
        time_text(d.captured_at),
        machine(&names, source),
        d.application.as_deref().unwrap_or("?"),
        d.window_title.as_deref().unwrap_or(""),
        d.width,
        d.height
    );
    Ok(Done {
        result: json!({
            "content": [
                { "type": "image", "data": ceiling::base64(&bytes), "mimeType": "image/webp" },
                { "type": "text", "text": caption },
            ],
            "isError": false,
        }),
        count: 1,
    })
}

fn list_gaps(
    server: &McpServer,
    ceiling: &Ceiling,
    args: &Value,
    now: chrono::DateTime<chrono::Local>,
) -> ToolResult {
    let since = time_arg(args, "since", now)?
        .unwrap_or_else(|| Timestamp(now.timestamp_millis() - 86_400_000));
    let since = ceiling.clamp_since(Some(since)).unwrap_or(since);
    let until = time_arg(args, "until", now)?.unwrap_or(Timestamp(now.timestamp_millis()));
    let min_ms = args
        .get("min_minutes")
        .and_then(Value::as_u64)
        .unwrap_or(1)
        .max(1) as i64
        * 60_000;
    let history = history(server);
    let names = machines(&history);
    let mut gaps = Vec::new();
    for filter in ceiling.filters() {
        gaps.extend(
            history
                .gaps(filter, since, until, min_ms)
                .map_err(read_err)?,
        );
    }
    gaps.retain(|g| ceiling.admits(g.source));
    gaps.sort_by_key(|g| (g.from, g.source));
    let text = if gaps.is_empty() {
        "No gaps: something was recorded throughout.".to_string()
    } else {
        gaps.iter()
            .map(|g| {
                format!(
                    "{} to {} · {} · {} min · {}",
                    time_text(g.from),
                    time_text(g.to),
                    machine(&names, g.source),
                    g.millis() / 60_000,
                    g.reason.describe()
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    let items: Vec<Value> = gaps
        .iter()
        .map(|g| {
            json!({ "from": time_text(g.from), "to": time_text(g.to), "machine": machine(&names, g.source),
                    "minutes": g.millis() / 60_000, "reason": g.reason, "explanation": g.reason.describe() })
        })
        .collect();
    let n = items.len();
    ok(text, json!({ "gaps": items }), n)
}

fn list_sources(server: &McpServer, ceiling: &Ceiling) -> ToolResult {
    let history = history(server);
    let sources: Vec<_> = history
        .sources()
        .map_err(read_err)?
        .into_iter()
        .filter(|s| ceiling.admits(s.source))
        .collect();
    let items: Vec<Value> = sources
        .iter()
        .map(|s| {
            json!({
                "machine": s.source.map_or_else(|| "this".to_string(), |id| id.to_string()),
                "name": s.label.clone().unwrap_or_else(|| if s.kind == SourceKind::Local { "this machine".into() } else { "unnamed".into() }),
                "kind": if s.kind == SourceKind::Local { "this machine" } else { "imported" },
                "moments": s.observations,
                "first": s.first.map(time_text),
                "last": s.last.map(time_text),
            })
        })
        .collect();
    let text = if items.is_empty() {
        "No history yet.".to_string()
    } else {
        items
            .iter()
            .map(|i| {
                format!(
                    "{} ({}): {} moments",
                    i["name"].as_str().unwrap_or("?"),
                    i["kind"].as_str().unwrap_or(""),
                    i["moments"]
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    let n = items.len();
    ok(text, json!({ "machines": items }), n)
}

fn run_cli(server: &McpServer, args: &[String]) -> Result<std::process::Output, ToolError> {
    let exe = server
        .exe
        .as_ref()
        .ok_or_else(|| refused("the rsrewind executable is not available"))?;
    let mut command = Command::new(exe);
    command
        .arg("--data-dir")
        .arg(server.data.root())
        .args(args)
        .stdin(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command
        .output()
        .map_err(|e| refused(format!("could not run rsrewind: {e}")))
}

fn get_status(server: &McpServer, ceiling: &Ceiling) -> ToolResult {
    let access = json!({
        "enabled": ceiling.enabled,
        "screenshots": ceiling.allow_screenshots,
        "window_days": server.config.max_age_days,
        "machines": if server.config.sources.is_empty() { json!("all") } else { json!(server.config.sources) },
    });
    let recorder = run_cli(server, &["status".into(), "--json".into()])
        .ok()
        .and_then(|out| serde_json::from_slice::<Value>(&out.stdout).ok());
    let state = recorder
        .as_ref()
        .and_then(|r| r["state"].as_str())
        .unwrap_or("unknown");
    let mut text = format!("Recorder: {state}.");
    if !ceiling.enabled {
        text.push_str(
            " Agent access is OFF: only this status is available. To turn it on, the person runs \
             `rsrewind mcp --enable` (it explains what is shared), then restarts this connection.",
        );
    } else {
        text.push_str(&format!(
            " Agents may read the last {} days{}; screenshots {}.",
            server.config.max_age_days,
            if server.config.sources.is_empty() {
                " on every machine"
            } else {
                " on the listed machines"
            },
            if ceiling.allow_screenshots {
                "allowed"
            } else {
                "not shared"
            }
        ));
    }
    let recorder = recorder.map(|r| {
        json!({
            "state": r["state"], "paused_until": r["paused_until"], "privacy_unenforced": r["privacy_unenforced"],
            "stats": r["stats"],
        })
    });
    ok(
        text,
        json!({ "recorder": recorder, "agent_access": access }),
        1,
    )
}

fn pause(server: &McpServer, args: &Value) -> ToolResult {
    let minutes = args
        .get("minutes")
        .and_then(Value::as_u64)
        .map(|m| m.clamp(1, 1440));
    let mut cli = vec!["pause".to_string()];
    if let Some(m) = minutes {
        cli.extend(["--minutes".to_string(), m.to_string()]);
    }
    let out = run_cli(server, &cli)?;
    if !out.status.success() {
        return Err(refused(format!(
            "could not pause: {}",
            String::from_utf8_lossy(&out.stderr)
                .lines()
                .last()
                .unwrap_or("rsrewind pause failed")
        )));
    }
    let text = match minutes {
        Some(m) => format!("Recording paused for {m} minutes."),
        None => "Recording paused until the person resumes it.".to_string(),
    };
    ok(text, json!({ "paused": true, "minutes": minutes }), 1)
}
