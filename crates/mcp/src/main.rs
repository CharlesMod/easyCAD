//! `easycad-mcp`: stdio MCP server for Claude Code that forwards to the running
//! easyCAD app.
//!
//! Each JSON-RPC line from stdin is POSTed to the app's loopback server, found
//! through the endpoint file the app writes when its bridge starts (re-read on
//! every message, so restarting the app is fine). When the app is not running,
//! `initialize` and `tools/list` are still answered here, so Claude Code lists the
//! tools, and each tool call returns an error asking the user to start the app.
//!
//! `easycad-mcp --check` prints whether the app is reachable.

use std::io::{BufRead, Write};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use easycad_mcp::{handle, post, read_endpoint, Handler, OFFLINE};
use serde_json::{json, Value};

/// Long enough for a slow regeneration or an export.
const CALL_TIMEOUT: Duration = Duration::from_secs(600);

enum Forward {
    Reply(Option<String>),
    Offline(String),
}

fn forward(line: &str) -> Forward {
    let Some(ep) = read_endpoint() else {
        return Forward::Offline("no endpoint file".into());
    };
    match post(&ep.url, &ep.token, line, CALL_TIMEOUT) {
        Ok((200, body)) => Forward::Reply(Some(body)),
        Ok((202, _)) => Forward::Reply(None),
        Ok((401, _)) => Forward::Offline("the app refused the token (stale endpoint file?)".into()),
        Ok((code, body)) => Forward::Offline(format!("HTTP {code}: {body}")),
        Err(e) => Forward::Offline(e.to_string()),
    }
}

fn offline_handler(why: String) -> Handler {
    Arc::new(move |_: &str, _: Value| Err(format!("{OFFLINE}\n(detail: {why})")))
}

fn main() {
    if std::env::args().nth(1).as_deref() == Some("--check") {
        match read_endpoint() {
            None => {
                println!("no endpoint file at {:?}: easyCAD's bridge is not running", easycad_mcp::endpoint_file());
                std::process::exit(1);
            }
            Some(ep) => {
                let ping = json!({"jsonrpc": "2.0", "id": 1, "method": "ping"}).to_string();
                match post(&ep.url, &ep.token, &ping, Duration::from_secs(5)) {
                    Ok((200, _)) => println!("easyCAD is reachable at {}", ep.url),
                    Ok((c, b)) => {
                        println!("{} answered HTTP {c}: {b}", ep.url);
                        std::process::exit(1);
                    }
                    Err(e) => {
                        println!("{} is not reachable: {e}", ep.url);
                        std::process::exit(1);
                    }
                }
            }
        }
        return;
    }
    let out = Arc::new(Mutex::new(std::io::stdout()));
    let stdin = std::io::stdin();
    let mut workers = Vec::new();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        let line = line.trim().to_string();
        if line.is_empty() {
            continue;
        }
        let out = out.clone();
        // one thread per message: a slow tool call does not hold up pings or cancellations
        workers.push(std::thread::spawn(move || {
            let reply = match forward(&line) {
                Forward::Reply(r) => r,
                Forward::Offline(why) => match serde_json::from_str::<Value>(&line) {
                    Ok(msg) => handle(&msg, &offline_handler(why)).map(|v| v.to_string()),
                    Err(e) => Some(json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32700, "message": format!("parse error: {e}")}}).to_string()),
                },
            };
            if let Some(r) = reply {
                let r = r.replace('\n', " ");
                let mut o = out.lock().unwrap_or_else(|e| e.into_inner());
                let _ = writeln!(o, "{}", r.trim());
                let _ = o.flush();
            }
        }));
        workers.retain(|w| !w.is_finished());
    }
    for w in workers {
        let _ = w.join();
    }
}
