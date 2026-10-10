//! The tools: definitions (name, schema, annotations), strict argument checking, and handlers. Every
//! handler re-checks the ceiling; `list` filtering is only discovery.

use crate::McpServer;
use crate::ceiling::{self, Ceiling, moment_id, parse_moment_id, parse_time, time_text};
use rsrewind_core::{SearchQuery, SourceId, TimelineCursor, Timestamp};
use rsrewind_query::{History, SourceKind};
use serde_json::{Map, Value, json};
use std::collections::HashMap;
use std::process::{Command, Stdio};

/// The most text one response may carry (recognized text plus line texts), in characters.
const MAX_TEXT_CHARS: usize = 20_000;
/// One line's text is cut beyond this, in characters.
const MAX_LINE_CHARS: usize = 2_000;
/// Pictures larger than this are refused rather than sent.
const MAX_SCREENSHOT_BYTES: usize = 4 * 1024 * 1024;
/// Any string argument longer than this is refused.
const MAX_ARG_CHARS: usize = 1_000;
const MAX_ROWS: usize = 500;
const MAX_RESULT_BYTES: usize = 512 * 1024;

pub enum ToolError {
    Unknown,
    /// A tool-level failure the agent should read (returned as `isError: true`, not a protocol
    /// error), with the tool's own name for the log.
    Refused(&'static str, String),
}

pub struct Done {
    pub tool: &'static str,
    pub result: Value,
    pub count: usize,
}

type ToolResult = Result<Done, String>;

pub fn error_result(message: &str) -> Value {
    json!({ "content": [{ "type": "text", "text": message }], "isError": true })
}

/// A result with a readable summary, the same data as JSON text (for clients that only read
/// `content`), and `structuredContent`.
fn ok(text: String, structured: Value, count: usize) -> ToolResult {
    let json_text = serde_json::to_string(&structured).unwrap_or_default();
    if text.len().saturating_add(json_text.len()) > MAX_RESULT_BYTES {
        return Err("Result too large; narrow the time window or reduce the page size.".into());
    }
    Ok(Done {
        tool: "",
        result: json!({
            "content": [{ "type": "text", "text": text }, { "type": "text", "text": json_text }],
            "structuredContent": structured,
            "isError": false,
        }),
        count,
    })
}

#[derive(Clone, Copy)]
enum Kind {
    Str,
    Bool,
    Int { min: i64, max: i64 },
}

struct Param {
    name: &'static str,
    kind: Kind,
    required: bool,
    description: &'static str,
}

const fn p(name: &'static str, kind: Kind, required: bool, description: &'static str) -> Param {
    Param {
        name,
        kind,
        required,
        description,
    }
}

struct Def {
    name: &'static str,
    title: &'static str,
    description: &'static str,
    params: &'static [Param],
    read_only: bool,
}

const TIME_HELP: &str =
    "RFC 3339, 'YYYY-MM-DD HH:MM' (local), 'YYYY-MM-DD', or a duration ago like 30m, 2h, 7d";

const DEFS: &[Def] = &[
    Def {
        name: "search",
        title: "Search screen history",
        description: "Find moments whose on-screen text matches. Quote a phrase for an exact match; end a word with * \
                      for a prefix. Filter by application, window title and time. Returns moments with their time, \
                      machine, app, window and a snippet; pass a moment to get_moment for its full text.",
        params: &[
            p("query", Kind::Str, true, "Words that were on screen"),
            p(
                "app",
                Kind::Str,
                false,
                "Only this application (process name, e.g. firefox or Teams.exe)",
            ),
            p(
                "title",
                Kind::Str,
                false,
                "Only windows whose title contains this",
            ),
            p("since", Kind::Str, false, TIME_HELP),
            p("until", Kind::Str, false, TIME_HELP),
            p(
                "limit",
                Kind::Int { min: 1, max: 50 },
                false,
                "Most results (default 10)",
            ),
        ],
        read_only: true,
    },
    Def {
        name: "recent",
        title: "Recent moments",
        description: "The most recent moments, newest first, or those at or before a time. Each is one stretch of time \
                      a screen showed the same picture.",
        params: &[
            p("before", Kind::Str, false, TIME_HELP),
            p(
                "limit",
                Kind::Int { min: 1, max: 100 },
                false,
                "Most results (default 20)",
            ),
        ],
        read_only: true,
    },
    Def {
        name: "moment_at",
        title: "What was on screen at a time",
        description: "The moment on screen at a given time (or the nearest one before it), on any machine.",
        params: &[p("time", Kind::Str, true, TIME_HELP)],
        read_only: true,
    },
    Def {
        name: "get_moment",
        title: "Moment details",
        description: "Everything about one moment: time, machine, app, window, and the full recognized text (optionally \
                      with each line's position on screen).",
        params: &[
            p(
                "moment",
                Kind::Str,
                true,
                "A moment id from search, recent or moment_at (e.g. this:123)",
            ),
            p(
                "include_lines",
                Kind::Bool,
                false,
                "Also return each text line with its box in screen pixels",
            ),
        ],
        read_only: true,
    },
    Def {
        name: "get_screenshot",
        title: "Moment screenshot",
        description: "The picture of one moment (WebP). Only available when the person allowed screenshots for agents.",
        params: &[p("moment", Kind::Str, true, "A moment id (e.g. this:123)")],
        read_only: true,
    },
    Def {
        name: "list_gaps",
        title: "Gaps in recording",
        description: "Stretches with nothing recorded and why: recorder off, stopped unexpectedly, paused, idle or locked, \
                      or nothing stored while recording. A screen that did not change is never a gap.",
        params: &[
            p(
                "since",
                Kind::Str,
                false,
                "Default 24h ago. Same forms as other times",
            ),
            p(
                "until",
                Kind::Str,
                false,
                "Default now. Same forms as other times",
            ),
            p(
                "min_minutes",
                Kind::Int { min: 1, max: 1440 },
                false,
                "Shortest gap to report (default 1)",
            ),
        ],
        read_only: true,
    },
    Def {
        name: "list_sources",
        title: "Machines in this history",
        description: "The machines whose history is here (this computer and any imported ones), with their time spans \
                      inside the period agents may see.",
        params: &[],
        read_only: true,
    },
    Def {
        name: "get_status",
        title: "rsRewind status",
        description: "Whether rsRewind is recording, paused or stopped, and what agents may access (and how the person \
                      can change that).",
        params: &[],
        read_only: true,
    },
    Def {
        name: "pause_recording",
        title: "Pause recording",
        description: "Pause recording, for some minutes or until the person resumes it. It never shortens or ends a pause \
                      already in force. Agents cannot resume recording or delete history; those stay with the person.",
        params: &[p(
            "minutes",
            Kind::Int { min: 1, max: 1440 },
            false,
            "Omit to pause until resumed",
        )],
        read_only: false,
    },
];

fn schema(def: &Def) -> Value {
    let mut properties = Map::new();
    for param in def.params {
        let mut s = match param.kind {
            Kind::Str => json!({ "type": "string" }),
            Kind::Bool => json!({ "type": "boolean" }),
            Kind::Int { min, max } => json!({ "type": "integer", "minimum": min, "maximum": max }),
        };
        s["description"] = json!(param.description);
        properties.insert(param.name.into(), s);
    }
    let required: Vec<&str> = def
        .params
        .iter()
        .filter(|p| p.required)
        .map(|p| p.name)
        .collect();
    let mut schema =
        json!({ "type": "object", "properties": properties, "additionalProperties": false });
    if !required.is_empty() {
        schema["required"] = json!(required);
    }
    schema
}

/// The top-level typed result contract; nested records remain extensible for future metadata.
fn output_schema(name: &str) -> Option<Value> {
    let fields: &[(&str, Value)] = match name {
        "search" | "recent" => &[(
            "moments",
            json!({"type":"array", "maxItems":100, "items":{"type":"object"}}),
        )],
        "moment_at" => &[("moment", json!({"type":["object", "null"]}))],
        "get_moment" => &[
            ("moment", json!({"type":"string"})),
            ("time", json!({"type":"string"})),
            ("machine", json!({"type":"string"})),
            ("app", json!({"type":["string","null"]})),
            ("window", json!({"type":["string","null"]})),
            ("monitor", json!({"type":"integer"})),
            ("width", json!({"type":"integer"})),
            ("height", json!({"type":"integer"})),
            ("text_status", json!({"type":"string"})),
            ("text", json!({"type":"string", "maxLength":MAX_TEXT_CHARS})),
            ("truncated", json!({"type":"boolean"})),
            (
                "lines",
                json!({"type":["array","null"], "items":{"type":"object"}}),
            ),
        ],
        "list_gaps" => &[
            (
                "gaps",
                json!({"type":"array", "maxItems":MAX_ROWS, "items":{"type":"object"}}),
            ),
            ("truncated", json!({"type":"boolean"})),
        ],
        "list_sources" => &[
            (
                "machines",
                json!({"type":"array", "maxItems":MAX_ROWS, "items":{"type":"object"}}),
            ),
            ("truncated", json!({"type":"boolean"})),
        ],
        "get_status" => &[
            ("recorder", json!({"type":["object","null"]})),
            ("agent_access", json!({"type":"object"})),
        ],
        "pause_recording" => &[
            ("paused", json!({"type":"boolean"})),
            ("minutes", json!({"type":["integer","null"]})),
        ],
        _ => return None,
    };
    let properties: Map<String, Value> = fields
        .iter()
        .map(|(key, value)| (key.to_string(), value.clone()))
        .collect();
    let required: Vec<&str> = fields.iter().map(|(key, _)| *key).collect();
    Some(
        json!({"type":"object", "properties":properties, "required":required, "additionalProperties":false}),
    )
}

/// Rejects unknown, missing, mistyped and out-of-range arguments: an argument the tool cannot use
/// exactly as given is an error, never a silent default.
fn validate(def: &Def, args: &Value) -> Result<(), String> {
    let empty = Map::new();
    let given = args.as_object().unwrap_or(&empty);
    for (key, value) in given {
        let Some(param) = def.params.iter().find(|p| p.name == key) else {
            return Err(format!(
                "{} has no argument '{}'",
                def.name,
                key.chars().take(40).collect::<String>()
            ));
        };
        let fine = match param.kind {
            Kind::Str => value
                .as_str()
                .is_some_and(|s| s.chars().count() <= MAX_ARG_CHARS),
            Kind::Bool => value.is_boolean(),
            Kind::Int { min, max } => value.as_i64().is_some_and(|n| (min..=max).contains(&n)),
        };
        if !fine {
            let expected = match param.kind {
                Kind::Str => format!("a string of at most {MAX_ARG_CHARS} characters"),
                Kind::Bool => "true or false".to_string(),
                Kind::Int { min, max } => format!("an integer from {min} to {max}"),
            };
            return Err(format!("{}: '{}' must be {expected}", def.name, param.name));
        }
    }
    for param in def.params.iter().filter(|p| p.required) {
        if !given.contains_key(param.name) {
            return Err(format!("{} needs '{}'", def.name, param.name));
        }
    }
    Ok(())
}

/// Every tool name, whatever the ceiling (for parity checks).
pub fn all_names() -> Vec<&'static str> {
    DEFS.iter().map(|d| d.name).collect()
}

/// Whether the ceiling lets this tool be listed and called; `Err` says why not.
fn allowed(ceiling: &Ceiling, name: &str) -> Result<(), String> {
    if name == "get_status" {
        return Ok(());
    }
    if !ceiling.enabled {
        return Err("rsRewind's agent access is turned off. The person can turn it on with `rsrewind mcp --enable` \
                    (which explains what is shared)."
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
    DEFS.iter()
        .filter(|d| allowed(ceiling, d.name).is_ok())
        .map(|d| {
            let mut tool = json!({
                "name": d.name,
                "title": d.title,
                "description": d.description,
                "inputSchema": schema(d),
                "annotations": {
                    "title": d.title,
                    "readOnlyHint": d.read_only,
                    "destructiveHint": false,
                    "idempotentHint": d.read_only,
                    "openWorldHint": false,
                },
            });
            if let Some(schema) = output_schema(d.name) {
                tool["outputSchema"] = schema;
            }
            tool
        })
        .collect()
}

pub fn call(
    server: &McpServer,
    ceiling: &Ceiling,
    name: &str,
    args: &Value,
) -> Result<Done, ToolError> {
    let def = DEFS
        .iter()
        .find(|d| d.name == name)
        .ok_or(ToolError::Unknown)?;
    let tool = def.name;
    let refuse = |message: String| ToolError::Refused(tool, message);
    allowed(ceiling, tool).map_err(refuse)?;
    validate(def, args).map_err(refuse)?;
    let now = chrono::Local::now();
    let result = match tool {
        "search" => search(server, ceiling, args, now),
        "recent" => recent(server, ceiling, args, now),
        "moment_at" => moment_at(server, ceiling, args, now),
        "get_moment" => get_moment(server, ceiling, args),
        "get_screenshot" => get_screenshot(server, ceiling, args),
        "list_gaps" => list_gaps(server, ceiling, args, now),
        "list_sources" => list_sources(server, ceiling),
        "get_status" => get_status(server, ceiling),
        "pause_recording" => pause(server, args),
        _ => return Err(ToolError::Unknown),
    };
    result.map(|done| Done { tool, ..done }).map_err(refuse)
}

fn history(server: &McpServer) -> History {
    History::open(&server.data)
}

fn read_err(e: impl std::fmt::Display) -> String {
    format!("could not read history: {e}")
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
) -> Result<Option<Timestamp>, String> {
    str_arg(args, key).map(|t| parse_time(t, now)).transpose()
}

/// Validated already; the default applies only when the argument is absent.
fn int_arg(args: &Value, key: &str, default: i64) -> i64 {
    args.get(key).and_then(Value::as_i64).unwrap_or(default)
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
    let text = str_arg(args, "query").ok_or("search needs a non-empty query")?;
    let limit = int_arg(args, "limit", 10) as u32;
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
    let summary = if hits.is_empty() {
        "No moments match.".to_string()
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
    ok(summary, json!({ "moments": items }), n)
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
    let limit = int_arg(args, "limit", 20) as u32;
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
    let summary = if entries.is_empty() {
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
    ok(summary, json!({ "moments": items }), n)
}

fn moment_at(
    server: &McpServer,
    ceiling: &Ceiling,
    args: &Value,
    now: chrono::DateTime<chrono::Local>,
) -> ToolResult {
    let at = time_arg(args, "time", now)?.ok_or("moment_at needs a time")?;
    if !ceiling.admits_time(at) {
        return Err("That time is older than agents may see.".into());
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
) -> Result<(Option<SourceId>, rsrewind_core::VisualDetail), String> {
    let text = str_arg(args, "moment").ok_or("needs a moment id (e.g. this:123)")?;
    let (source, id) =
        parse_moment_id(text).ok_or("that is not a moment id (they look like this:123)")?;
    if !ceiling.admits(source) {
        return Err("That machine's history is not shared with agents.".into());
    }
    let detail = history
        .visual_detail(source, id)
        .map_err(read_err)?
        .ok_or("No such moment (it may have been forgotten).")?;
    if !ceiling.admits_time(detail.captured_at) {
        return Err("That moment is older than agents may see.".into());
    }
    Ok((source, detail))
}

/// Text cut to `max` characters; whether anything was cut.
fn cut(text: &str, max: usize) -> (String, bool) {
    let mut chars = text.chars();
    let out: String = chars.by_ref().take(max).collect();
    (out, chars.next().is_some())
}

fn get_moment(server: &McpServer, ceiling: &Ceiling, args: &Value) -> ToolResult {
    let history = history(server);
    let names = machines(&history);
    let (source, d) = checked_detail(&history, ceiling, args)?;
    // One budget for everything textual this response carries.
    let mut budget = MAX_TEXT_CHARS;
    let (text_out, mut truncated) = cut(&d.ocr_text, budget);
    budget -= text_out.chars().count();
    let include_lines = args
        .get("include_lines")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let lines: Option<Vec<Value>> = include_lines.then(|| {
        let mut lines = Vec::new();
        for b in &d.blocks {
            if budget == 0 {
                truncated = true;
                break;
            }
            let (line, cut_line) = cut(&b.text, MAX_LINE_CHARS.min(budget));
            truncated |= cut_line;
            budget -= line.chars().count();
            lines.push(
                json!({ "text": line, "x": b.x, "y": b.y, "width": b.width, "height": b.height }),
            );
        }
        lines
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
    let summary = match (text_out.is_empty(), truncated) {
        (true, _) => format!("{header}\n(no recognized text)"),
        (false, false) => format!("{header}\n\n{text_out}"),
        (false, true) => format!("{header}\n\n{text_out}\n[…cut to fit]"),
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
            "truncated": truncated,
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
        return Err("That picture is too large to send.".into());
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
        tool: "",
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
    let now_ts = Timestamp(now.timestamp_millis());
    let since =
        time_arg(args, "since", now)?.unwrap_or_else(|| now_ts.saturating_sub_millis(86_400_000));
    let since = ceiling.clamp_since(Some(since)).unwrap_or(since);
    let until = time_arg(args, "until", now)?.unwrap_or(now_ts);
    if until <= since {
        return Err("until must be after since.".into());
    }
    if until.0.saturating_sub(since.0) > 366 * 86_400_000 {
        return Err("Request at most 366 days of gaps at a time.".into());
    }
    let min_ms = int_arg(args, "min_minutes", 1) * 60_000; // 1..=1440 by validation
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
    let truncated = gaps.len() > MAX_ROWS;
    gaps.truncate(MAX_ROWS);
    let summary = if gaps.is_empty() {
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
    ok(summary, json!({ "gaps": items, "truncated": truncated }), n)
}

fn list_sources(server: &McpServer, ceiling: &Ceiling) -> ToolResult {
    let history = history(server);
    // Counts and spans inside the window only; machines with nothing in it are not listed.
    let mut sources: Vec<_> = history
        .sources_since(ceiling.oldest)
        .map_err(read_err)?
        .into_iter()
        .filter(|s| ceiling.admits(s.source) && s.observations > 0)
        .collect();
    let truncated = sources.len() > MAX_ROWS;
    sources.truncate(MAX_ROWS);
    let items: Vec<Value> = sources
        .iter()
        .map(|s| {
            let local = s.kind == SourceKind::Local;
            json!({
                "machine": s.source.map_or_else(|| "this".to_string(), |id| id.to_string()),
                "name": s.label.clone().unwrap_or_else(|| if local { "this machine".into() } else { "unnamed".into() }),
                "kind": if local { "this machine" } else { "imported" },
                "moments": s.observations,
                "first": s.first.map(time_text),
                "last": s.last.map(time_text),
            })
        })
        .collect();
    let summary = if items.is_empty() {
        "No history in the period agents may see.".to_string()
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
    ok(
        summary,
        json!({ "machines": items, "truncated": truncated }),
        n,
    )
}

fn run_cli(server: &McpServer, args: &[&str]) -> Result<std::process::Output, String> {
    let exe = server
        .exe
        .as_ref()
        .ok_or("the rsrewind executable is not available")?;
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
        .map_err(|e| format!("could not run rsrewind: {e}"))
}

/// Operational state only, from an explicit list of fields: never history statistics (how much was
/// recorded and when), which the ceiling would otherwise have to filter.
fn get_status(server: &McpServer, ceiling: &Ceiling) -> ToolResult {
    let config = server.config();
    let access = json!({
        "enabled": ceiling.enabled,
        "screenshots": ceiling.allow_screenshots,
        "window_days": config.max_age_days,
        "machines": if config.sources.is_empty() { json!("all") } else { json!(config.sources) },
    });
    let status = run_cli(server, &["status", "--json"])
        .ok()
        .and_then(|out| serde_json::from_slice::<Value>(&out.stdout).ok());
    let recorder = status.map(|r| {
        json!({
            "state": r.get("state").and_then(Value::as_str),
            "paused_until": r.get("paused_until").and_then(Value::as_i64).map(|ms| time_text(Timestamp(ms))),
            "privacy_rules_enforced": r.get("privacy_unenforced").is_none_or(Value::is_null),
        })
    });
    let state = recorder
        .as_ref()
        .and_then(|r| r["state"].as_str())
        .unwrap_or("unknown")
        .to_string();
    let mut summary = format!("Recorder: {state}.");
    if !ceiling.enabled {
        summary.push_str(
            " Agent access is OFF: only this status is available. To turn it on, the person runs \
             `rsrewind mcp --enable` (it explains what is shared).",
        );
    } else {
        let window = if config.max_age_days == 0 {
            "all history".to_string()
        } else {
            format!("the last {} days", config.max_age_days)
        };
        summary.push_str(&format!(
            " Agents may read {window}{}; screenshots {}.",
            if config.sources.is_empty() {
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
    ok(
        summary,
        json!({ "recorder": recorder, "agent_access": access }),
        1,
    )
}

fn pause(server: &McpServer, args: &Value) -> ToolResult {
    let minutes = args.get("minutes").and_then(Value::as_i64); // 1..=1440 by validation
    let minutes_text = minutes.map(|m| m.to_string());
    let mut cli = vec!["pause", "--extend-only"];
    if let Some(m) = &minutes_text {
        cli.extend(["--minutes", m.as_str()]);
    }
    let out = run_cli(server, &cli)?;
    if !out.status.success() {
        return Err(format!(
            "could not pause: {}",
            String::from_utf8_lossy(&out.stderr)
                .lines()
                .last()
                .unwrap_or("rsrewind pause failed")
        ));
    }
    let summary = match minutes {
        Some(m) => format!(
            "Recording is paused for at least {m} minutes (a longer pause already in force is kept)."
        ),
        None => "Recording is paused until the person resumes it.".to_string(),
    };
    ok(summary, json!({ "paused": true, "minutes": minutes }), 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn def(name: &str) -> &'static Def {
        DEFS.iter().find(|d| d.name == name).unwrap_or(&DEFS[0])
    }

    #[test]
    fn arguments_are_checked_strictly() {
        let pause = def("pause_recording");
        assert_eq!(pause.name, "pause_recording");
        assert!(validate(pause, &json!({})).is_ok());
        assert!(validate(pause, &json!({ "minutes": 5 })).is_ok());
        assert!(
            validate(pause, &json!({ "minutes": "5" })).is_err(),
            "a string is not an integer"
        );
        assert!(validate(pause, &json!({ "minutes": 0 })).is_err());
        assert!(validate(pause, &json!({ "minutes": 99_999 })).is_err());
        assert!(
            validate(pause, &json!({ "minuets": 5 })).is_err(),
            "unknown argument"
        );
        let recent = def("recent");
        assert!(validate(recent, &json!({ "before": 123 })).is_err());
        assert!(validate(recent, &json!({ "limit": -1 })).is_err());
        let get = def("get_moment");
        assert!(validate(get, &json!({})).is_err(), "missing required");
        assert!(validate(get, &json!({ "moment": "x".repeat(MAX_ARG_CHARS + 1) })).is_err());
    }

    #[test]
    fn schemas_match_what_is_validated() {
        for def in DEFS {
            let s = schema(def);
            let props = s["properties"].as_object().map_or(0, |p| p.len());
            assert_eq!(props, def.params.len(), "{}", def.name);
            assert_eq!(s["additionalProperties"], false);
        }
    }

    #[test]
    fn cut_reports_truncation() {
        assert_eq!(cut("abc", 5), ("abc".into(), false));
        assert_eq!(cut("abcdef", 3), ("abc".into(), true));
        assert_eq!(cut("ééé", 2), ("éé".into(), true));
    }
}
