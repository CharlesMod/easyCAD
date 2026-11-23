//! The `easycad-mcp` stdio shim: forwards to a running server found through the
//! endpoint file, and answers `initialize` / `tools/list` itself when nothing runs.

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::sync::Arc;

use serde_json::{json, Value};

fn run_shim(endpoint: &std::path::Path, lines: &[Value]) -> Vec<Value> {
    let mut child = Command::new(env!("CARGO_BIN_EXE_easycad-mcp"))
        .env("EASYCAD_MCP_ENDPOINT", endpoint)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut out = BufReader::new(child.stdout.take().unwrap());
    let mut replies = Vec::new();
    for l in lines {
        writeln!(stdin, "{l}").unwrap();
        stdin.flush().unwrap();
        if l.get("id").is_some() {
            let mut s = String::new();
            out.read_line(&mut s).unwrap();
            replies.push(serde_json::from_str(&s).unwrap());
        }
    }
    drop(stdin);
    child.wait().unwrap();
    replies
}

fn msgs() -> Vec<Value> {
    vec![
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "t", "version": "0"}}}),
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
        json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"name": "get_document", "arguments": {}}}),
    ]
}

#[test]
fn shim_forwards_to_the_app() {
    let dir = std::env::temp_dir().join(format!("easycad-shim-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let ep = dir.join("endpoint.json");
    let srv = easycad_mcp::serve(0, "tok123".into(), Arc::new(|name: &str, _| Ok(vec![easycad_mcp::text(format!("ran {name}"))]))).unwrap();
    std::fs::write(&ep, json!({"url": srv.url(), "token": "tok123", "pid": 1}).to_string()).unwrap();
    let r = run_shim(&ep, &msgs());
    assert_eq!(r[0]["result"]["serverInfo"]["name"], "easycad");
    assert!(r[1]["result"]["tools"].as_array().unwrap().len() >= 16);
    assert_eq!(r[2]["result"]["content"][0]["text"], "ran get_document");
    assert_eq!(r[2]["id"], 3);
}

#[test]
fn shim_without_the_app_still_lists_tools() {
    let ep = std::env::temp_dir().join(format!("easycad-shim-none-{}.json", std::process::id()));
    let _ = std::fs::remove_file(&ep);
    let r = run_shim(&ep, &msgs());
    assert_eq!(r[0]["result"]["protocolVersion"], "2025-06-18");
    assert!(r[1]["result"]["tools"].as_array().unwrap().iter().any(|t| t["name"] == "screenshot"));
    assert_eq!(r[2]["result"]["isError"], true);
    assert!(r[2]["result"]["content"][0]["text"].as_str().unwrap().contains("easycad --mcp"));
}
