//! Shared fixture: a real store with three moments, and helpers that speak JSON-RPC to `serve`.
#![allow(dead_code)]

use rsrewind_core::paths::media_relative_path;
use rsrewind_core::{
    ApplicationContext, BgraFrame, DataDir, McpConfig, MonitorInfo, OcrBlock, Timestamp,
    WindowContext,
};
use rsrewind_mcp::{McpServer, serve};
use rsrewind_storage::media::{encode_webp, write_webp_exclusive};
use rsrewind_storage::{NewVisualState, Observation, Store};
use serde_json::{Value, json};

pub type Fallible<T> = Result<T, Box<dyn std::error::Error>>;

pub const MIN: i64 = 60_000;
pub const DAY: i64 = 86_400_000;

/// A history with three moments on this machine: two recent ones with text, one 40 days old.
pub fn history() -> Fallible<(tempfile::TempDir, DataDir)> {
    let dir = tempfile::tempdir()?;
    let data = DataDir::new(dir.path());
    let store = Store::open(&data)?;
    let now = Timestamp::now().0;
    let session = store.begin_session(Timestamp(now - 41 * DAY), "host", "test")?;
    let monitor = store.upsert_monitor(
        &MonitorInfo {
            device_name: "eDP-1".into(),
            left: 0,
            top: 0,
            width: 8,
            height: 8,
            dpi: 96,
            primary: true,
        },
        Timestamp(now - 41 * DAY),
    )?;
    for (i, (ago, app, title, text)) in [
        (40 * DAY, "mail", "Old invoice", "invoice from long ago"),
        (
            30 * MIN,
            "firefox",
            "Shop - Blue Screen Babe",
            "Blue Screen Babe 100 Servings $40",
        ),
        (
            5 * MIN,
            "konsole",
            "build",
            "error: linker failed with exit code 1",
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let at = Timestamp(now - ago);
        let frame = BgraFrame {
            width: 8,
            height: 8,
            stride: 32,
            pixels: (0..256)
                .map(|p| (p as u8).wrapping_mul(i as u8 + 3))
                .collect(),
        };
        let bytes = encode_webp(&frame, 80)?;
        let relative = media_relative_path(at, monitor);
        write_webp_exclusive(&data, &relative, &bytes)?;
        let state = store.insert_visual_state(&NewVisualState {
            monitor,
            captured_at: at,
            media_path: relative.clone(),
            width: 8,
            height: 8,
            byte_size: bytes.len() as u64,
            fingerprint: None,
            ocr_enabled: true,
        })?;
        store.save_ocr(
            state,
            &relative,
            &[OcrBlock {
                text: text.into(),
                x: 1.0,
                y: 2.0,
                width: 3.0,
                height: 1.0,
                confidence: None,
                line_index: 0,
            }],
            "test",
            1,
        )?;
        let application = store.upsert_application(
            &ApplicationContext {
                process_name: app.into(),
                exe_path: None,
            },
            at,
        )?;
        let window = store.upsert_window(
            application,
            &WindowContext {
                title: title.into(),
                class_name: None,
            },
            at,
        )?;
        for step in [0, 2_000] {
            store.record_observation(&Observation {
                session,
                monitor,
                visual_state: state,
                application: Some(application),
                window: Some(window),
                at: Timestamp(at.0 + step),
                max_gap_ms: 5_000,
            })?;
        }
    }
    Ok((dir, data))
}

pub fn server(data: &DataDir, config: McpConfig) -> McpServer {
    McpServer {
        data: data.clone(),
        config,
        exe: None,
        version: "9.9.9-test",
    }
}

pub fn enabled() -> McpConfig {
    McpConfig {
        enabled: true,
        ..McpConfig::default()
    }
}

/// Sends messages through `serve` and returns every reply line, parsed.
pub fn exchange(server: &McpServer, messages: &[Value]) -> Fallible<Vec<Value>> {
    let input: String = messages.iter().map(|m| format!("{m}\n")).collect();
    let mut out = Vec::new();
    serve(server, input.as_bytes(), &mut out)?;
    Ok(String::from_utf8(out)?
        .lines()
        .map(serde_json::from_str)
        .collect::<Result<_, _>>()?)
}

pub fn call(server: &McpServer, name: &str, arguments: Value) -> Fallible<Value> {
    let replies = exchange(
        server,
        &[json!({ "jsonrpc": "2.0", "id": 7, "method": "tools/call",
                  "params": { "name": name, "arguments": arguments } })],
    )?;
    Ok(replies.into_iter().next().ok_or("no reply")?["result"].clone())
}

pub fn tool_names(server: &McpServer) -> Fallible<Vec<String>> {
    let replies = exchange(
        server,
        &[json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" })],
    )?;
    Ok(replies[0]["result"]["tools"]
        .as_array()
        .ok_or("tools")?
        .iter()
        .filter_map(|t| t["name"].as_str().map(String::from))
        .collect())
}
