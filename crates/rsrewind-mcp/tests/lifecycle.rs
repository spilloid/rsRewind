mod common;

use common::*;
use rsrewind_core::Config;
use rsrewind_mcp::{MAX_MESSAGE_BYTES, McpServer, Settings, serve};
use serde_json::{Value, json};

fn init(version: &str) -> Value {
    json!({"jsonrpc":"2.0", "id":"init", "method":"initialize", "params":{
        "protocolVersion":version, "capabilities":{}, "clientInfo":{"name":"contract","version":"1"}}})
}

fn raw(server: &McpServer, messages: &[Value]) -> Fallible<Vec<Value>> {
    let input = messages
        .iter()
        .map(|m| format!("{m}\n"))
        .collect::<String>();
    let mut output = Vec::new();
    serve(server, input.as_bytes(), &mut output)?;
    String::from_utf8(output)?
        .lines()
        .map(|l| Ok(serde_json::from_str(l)?))
        .collect()
}

#[test]
fn tools_require_initialize_and_initialized_and_initialize_is_once() -> Fallible<()> {
    let (_dir, data) = history()?;
    let server = server(&data, enabled());
    let list = json!({"jsonrpc":"2.0","id":1,"method":"tools/list"});
    let replies = raw(
        &server,
        &[
            list.clone(),
            init("2025-11-25"),
            list.clone(),
            json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
            list,
            init("2025-11-25"),
        ],
    )?;
    assert_eq!(replies[0]["error"]["code"], -32600);
    assert!(replies[1]["result"].is_object());
    assert_eq!(replies[2]["error"]["code"], -32600);
    assert_eq!(
        replies[3]["result"]["tools"].as_array().map(Vec::len),
        Some(8)
    );
    assert_eq!(replies[4]["error"]["code"], -32600);
    Ok(())
}

#[test]
fn batches_only_run_on_the_version_that_supports_them() -> Fallible<()> {
    let (_dir, data) = history()?;
    let server = server(&data, enabled());
    let batch = json!([
        {"jsonrpc":"2.0","id":1,"method":"ping"},
        {"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":99}},
        {"jsonrpc":"2.0","id":2,"method":"tools/list"}
    ]);
    for (version, supported) in [
        ("2025-03-26", true),
        ("2025-06-18", false),
        ("2025-11-25", false),
    ] {
        let replies = raw(
            &server,
            &[
                init(version),
                json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
                batch.clone(),
            ],
        )?;
        if supported {
            assert_eq!(replies[1].as_array().map(Vec::len), Some(2));
        } else {
            assert_eq!(replies[1]["error"]["code"], -32600);
        }
    }
    Ok(())
}

#[test]
fn oversize_and_non_utf8_messages_do_not_desynchronize_the_stream() -> Fallible<()> {
    let (_dir, data) = history()?;
    let server = server(&data, enabled());
    for exact_newline in [false, true] {
        let mut input = vec![b'x'; MAX_MESSAGE_BYTES + 1];
        if exact_newline {
            input[MAX_MESSAGE_BYTES] = b'\n';
        } else {
            input.push(b'\n');
        }
        input.extend_from_slice(b"\xff\n");
        input.extend_from_slice(format!("{}\n", init("2025-11-25")).as_bytes());
        let mut output = Vec::new();
        serve(&server, input.as_slice(), &mut output)?;
        let replies: Vec<Value> = String::from_utf8(output)?
            .lines()
            .map(serde_json::from_str)
            .collect::<Result<_, _>>()?;
        assert_eq!(replies.len(), 3);
        assert_eq!(replies[0]["error"]["code"], -32600);
        assert_eq!(replies[1]["error"]["code"], -32700);
        assert!(replies[2]["result"].is_object());
    }
    Ok(())
}

#[test]
fn ceiling_is_reread_and_invalid_configuration_fails_closed() -> Fallible<()> {
    let (_dir, data) = history()?;
    let path = data.root().join("config.toml");
    let server = McpServer::new(data.clone(), Settings::File(path.clone()), None, "test");
    let mut config = Config::default();
    config.mcp = enabled();
    config.save(&path)?;
    assert_eq!(
        call(&server, "search", json!({"query":"linker"}))?["isError"],
        false
    );
    config.mcp.sources = vec!["0123456789abcdef0123456789abcdef".into()];
    config.save(&path)?;
    assert_eq!(
        call(&server, "search", json!({"query":"linker"}))?["structuredContent"]["moments"],
        json!([])
    );
    config.mcp.enabled = false;
    config.save(&path)?;
    assert_eq!(
        call(&server, "search", json!({"query":"linker"}))?["isError"],
        true
    );
    std::fs::write(&path, "malformed [toml")?;
    assert_eq!(tool_names(&server)?, ["get_status"]);
    std::fs::remove_file(&path)?;
    assert_eq!(tool_names(&server)?, ["get_status"]);
    Ok(())
}
