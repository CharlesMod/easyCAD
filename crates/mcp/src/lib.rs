//! Model Context Protocol bridge for easyCAD (see `docs/MCP.md`).
//!
//! - [`tools`]: the tool list (names, descriptions, JSON schemas) the app serves.
//! - [`handle`]: JSON-RPC 2.0 dispatch for `initialize`, `ping`, `tools/list` and
//!   `tools/call`; the tool calls themselves go to a [`Handler`].
//! - [`serve`]: a loopback-only HTTP server for MCP's "streamable HTTP" transport
//!   (plain JSON responses, no SSE), protected by a bearer token. The app runs it
//!   and writes the URL and token to [`endpoint_file`].
//! - [`post`]: the HTTP client the `easycad-mcp` stdio shim uses to forward to it.
//!
//! Only `std` and `serde_json`: the shim builds in seconds without the CAD kernel.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};

/// Default port of the app's server (the next free one up to +10 is used if busy).
pub const DEFAULT_PORT: u16 = 47_311;
/// Protocol revision answered when the client asks for one we do not know.
pub const PROTOCOL_VERSION: &str = "2025-06-18";
const KNOWN_VERSIONS: &[&str] = &["2024-11-05", "2025-03-26", "2025-06-18", "2025-11-25"];

/// Sent to the client at `initialize`.
pub const INSTRUCTIONS: &str = "easyCAD is a parametric CAD app the user has open and watches while you work. \
Every change you make appears live in their viewport and is one undo step labelled \"Claude: ...\". \
Before each change call get_document and read `user_changes`: the user may have edited by hand. \
Work parametrically: set_param first, then add_sketch, then add_feature; read every regen report; \
use measure / get_selection for selectors (never raw indices); check significant steps with screenshot.";

/// Text of a tool error when the app is not reachable (the shim answers it).
pub const OFFLINE: &str = "easyCAD is not running with the Claude bridge on. Ask the user to start it with \
`easycad --mcp` (or turn on Settings → Claude bridge) and keep the window open, then retry.";

/// The tool list served by `tools/list`.
pub fn tools() -> Value {
    serde_json::from_str(include_str!("tools.json")).expect("tools.json is valid JSON")
}

/// Names of every tool.
pub fn tool_names() -> Vec<String> {
    tools().as_array().map(|a| a.iter().filter_map(|t| t["name"].as_str().map(String::from)).collect()).unwrap_or_default()
}

/// Content blocks of a successful call (`[{"type":"text","text":..}, {"type":"image",..}]`)
/// or an error message (returned as a tool result with `isError: true`, so the model
/// reads it and can correct itself).
pub type ToolResult = Result<Vec<Value>, String>;

/// Runs one tool call: `(name, arguments)`.
pub type Handler = Arc<dyn Fn(&str, Value) -> ToolResult + Send + Sync>;

/// A text content block.
pub fn text(s: impl Into<String>) -> Value {
    json!({"type": "text", "text": s.into()})
}

/// A text content block holding JSON, indented, with short arrays of numbers or
/// strings on one line (vectors stay readable without costing five lines each).
pub fn json_text(v: &Value) -> Value {
    let mut s = String::new();
    pretty(v, 0, &mut s);
    text(s)
}

fn pretty(v: &Value, depth: usize, out: &mut String) {
    let pad = |d: usize| "  ".repeat(d);
    match v {
        Value::Object(o) if !o.is_empty() => {
            out.push_str("{\n");
            for (i, (k, x)) in o.iter().enumerate() {
                out.push_str(&pad(depth + 1));
                out.push_str(&Value::String(k.clone()).to_string());
                out.push_str(": ");
                pretty(x, depth + 1, out);
                out.push_str(if i + 1 < o.len() { ",\n" } else { "\n" });
            }
            out.push_str(&pad(depth));
            out.push('}');
        }
        Value::Array(a) if !a.is_empty() => {
            let flat = a.iter().all(|x| !x.is_object() && !x.is_array() || x.as_array().is_some_and(|y| y.iter().all(|z| z.is_number())));
            let one_line = v.to_string();
            if flat && one_line.len() <= 100 {
                out.push_str(&one_line.replace(',', ", "));
                return;
            }
            out.push_str("[\n");
            for (i, x) in a.iter().enumerate() {
                out.push_str(&pad(depth + 1));
                pretty(x, depth + 1, out);
                out.push_str(if i + 1 < a.len() { ",\n" } else { "\n" });
            }
            out.push_str(&pad(depth));
            out.push(']');
        }
        x => out.push_str(&x.to_string()),
    }
}

/// An image content block.
pub fn image(png: &[u8]) -> Value {
    json!({"type": "image", "data": base64(png), "mimeType": "image/png"})
}

/// Answer one JSON-RPC message (or a batch). `None` for notifications and
/// responses, which get no reply.
pub fn handle(msg: &Value, handler: &Handler) -> Option<Value> {
    if let Some(batch) = msg.as_array() {
        let out: Vec<Value> = batch.iter().filter_map(|m| handle(m, handler)).collect();
        return (!out.is_empty()).then_some(Value::Array(out));
    }
    let id = msg.get("id").cloned();
    let Some(method) = msg.get("method").and_then(Value::as_str) else {
        // a response to something we never send, or garbage
        return id.map(|id| rpc_error(id, -32600, "invalid request"));
    };
    let id = id?; // notifications (`notifications/initialized`, cancellations) need no answer
    let params = msg.get("params").cloned().unwrap_or(Value::Null);
    let result = match method {
        "initialize" => {
            let asked = params.get("protocolVersion").and_then(Value::as_str).unwrap_or(PROTOCOL_VERSION);
            let version = if KNOWN_VERSIONS.contains(&asked) { asked } else { PROTOCOL_VERSION };
            json!({
                "protocolVersion": version,
                "capabilities": {"tools": {"listChanged": false}},
                "serverInfo": {"name": "easycad", "title": "easyCAD", "version": env!("CARGO_PKG_VERSION")},
                "instructions": INSTRUCTIONS,
            })
        }
        "ping" => json!({}),
        "tools/list" => json!({"tools": tools()}),
        "tools/call" => {
            let Some(name) = params.get("name").and_then(Value::as_str) else {
                return Some(rpc_error(id, -32602, "tools/call needs a tool `name`"));
            };
            if !tool_names().iter().any(|n| n == name) {
                return Some(rpc_error(id, -32602, &format!("unknown tool `{name}`")));
            }
            let args = match params.get("arguments") {
                Some(Value::Object(_)) => params["arguments"].clone(),
                _ => json!({}),
            };
            match handler(name, args) {
                Ok(content) => json!({"content": content, "isError": false}),
                Err(e) => json!({"content": [text(e)], "isError": true}),
            }
        }
        m => return Some(rpc_error(id, -32601, &format!("method not found: {m}"))),
    };
    Some(json!({"jsonrpc": "2.0", "id": id, "result": result}))
}

fn rpc_error(id: Value, code: i64, msg: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": msg}})
}

// ------------------------------------------------------------------ server

/// A running server; dropping it (or [`Server::stop`]) closes the listener.
pub struct Server {
    pub addr: SocketAddr,
    pub token: String,
    stop: Arc<AtomicBool>,
}

impl Server {
    pub fn url(&self) -> String {
        format!("http://127.0.0.1:{}/mcp", self.addr.port())
    }

    pub fn stop(&self) {
        if !self.stop.swap(true, Ordering::SeqCst) {
            // wake the blocking accept
            let _ = TcpStream::connect_timeout(&self.addr, Duration::from_millis(200));
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Listen on 127.0.0.1 from `port` (0 = any free port; else up to `port + 10` if busy).
pub fn serve(port: u16, token: String, handler: Handler) -> std::io::Result<Server> {
    let listener = if port == 0 {
        TcpListener::bind(("127.0.0.1", 0))?
    } else {
        let mut last = None;
        let mut found = None;
        for p in port..=port.saturating_add(10) {
            match TcpListener::bind(("127.0.0.1", p)) {
                Ok(l) => {
                    found = Some(l);
                    break;
                }
                Err(e) => last = Some(e),
            }
        }
        match found {
            Some(l) => l,
            None => return Err(last.unwrap_or_else(|| std::io::Error::other("no free port"))),
        }
    };
    let addr = listener.local_addr()?;
    let stop = Arc::new(AtomicBool::new(false));
    let (st, tok) = (stop.clone(), token.clone());
    std::thread::Builder::new().name("mcp-accept".into()).spawn(move || {
        for conn in listener.incoming() {
            if st.load(Ordering::SeqCst) {
                break;
            }
            let Ok(conn) = conn else { continue };
            let (h, tok, st) = (handler.clone(), tok.clone(), st.clone());
            let _ = std::thread::Builder::new().name("mcp-conn".into()).spawn(move || connection(conn, &tok, &h, &st));
        }
    })?;
    Ok(Server { addr, token, stop })
}

struct HttpRequest {
    method: String,
    path: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl HttpRequest {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
    }
}

fn read_request(r: &mut BufReader<TcpStream>) -> std::io::Result<Option<HttpRequest>> {
    let mut line = String::new();
    if r.read_line(&mut line)? == 0 {
        return Ok(None);
    }
    let mut parts = line.split_whitespace();
    let (Some(method), Some(path)) = (parts.next(), parts.next()) else {
        return Err(std::io::Error::other("bad request line"));
    };
    let (method, path) = (method.to_string(), path.to_string());
    let mut headers = Vec::new();
    loop {
        line.clear();
        if r.read_line(&mut line)? == 0 {
            return Ok(None);
        }
        let l = line.trim_end();
        if l.is_empty() {
            break;
        }
        if let Some((k, v)) = l.split_once(':') {
            headers.push((k.trim().to_ascii_lowercase(), v.trim().to_string()));
        }
        if headers.len() > 100 {
            return Err(std::io::Error::other("too many headers"));
        }
    }
    let len: usize = headers.iter().find(|(k, _)| k == "content-length").and_then(|(_, v)| v.parse().ok()).unwrap_or(0);
    if len > 64 << 20 {
        return Err(std::io::Error::other("body too large"));
    }
    let mut body = vec![0; len];
    r.read_exact(&mut body)?;
    Ok(Some(HttpRequest { method, path, headers, body }))
}

fn respond(w: &mut TcpStream, status: &str, ctype: &str, body: &[u8], close: bool) -> std::io::Result<()> {
    let conn = if close { "close" } else { "keep-alive" };
    write!(w, "HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: {conn}\r\n\r\n", body.len())?;
    w.write_all(body)?;
    w.flush()
}

/// A browser page on another site may try to reach the server (DNS rebinding /
/// cross-site requests): only loopback origins are accepted.
fn origin_ok(o: Option<&str>) -> bool {
    let Some(o) = o else { return true };
    let host = o.split("://").nth(1).unwrap_or(o);
    let host = host.split('/').next().unwrap_or(host);
    let host = if host.starts_with('[') { host.split(']').next().map(|h| &h[1..]).unwrap_or(host) } else { host.split(':').next().unwrap_or(host) };
    matches!(host, "localhost" | "127.0.0.1" | "::1")
}

fn token_ok(req: &HttpRequest, token: &str) -> bool {
    let given = req
        .header("authorization")
        .and_then(|a| a.strip_prefix("Bearer ").or_else(|| a.strip_prefix("bearer ")))
        .or_else(|| req.header("x-easycad-token"))
        .unwrap_or("")
        .trim();
    // constant time over the token length
    given.len() == token.len() && given.bytes().zip(token.bytes()).fold(0u8, |acc, (a, b)| acc | (a ^ b)) == 0
}

fn connection(conn: TcpStream, token: &str, handler: &Handler, stop: &AtomicBool) {
    let _ = conn.set_read_timeout(Some(Duration::from_secs(600)));
    let _ = conn.set_nodelay(true);
    let Ok(mut w) = conn.try_clone() else { return };
    let mut r = BufReader::new(conn);
    while !stop.load(Ordering::SeqCst) {
        let req = match read_request(&mut r) {
            Ok(Some(req)) => req,
            _ => return,
        };
        let close = req.header("connection").is_some_and(|c| c.eq_ignore_ascii_case("close"));
        let path = req.path.split('?').next().unwrap_or("");
        let res = if !origin_ok(req.header("origin")) {
            respond(&mut w, "403 Forbidden", "text/plain", b"cross-origin requests are not allowed", close)
        } else if path != "/mcp" && path != "/" {
            respond(&mut w, "404 Not Found", "text/plain", b"the MCP endpoint is /mcp", close)
        } else if !token_ok(&req, token) {
            respond(&mut w, "401 Unauthorized", "text/plain", b"missing or wrong bearer token (see the endpoint file written by easyCAD)", close)
        } else {
            match req.method.as_str() {
                "POST" => match serde_json::from_slice::<Value>(&req.body) {
                    Ok(msg) => match handle(&msg, handler) {
                        Some(out) => respond(&mut w, "200 OK", "application/json", out.to_string().as_bytes(), close),
                        None => respond(&mut w, "202 Accepted", "text/plain", b"", close),
                    },
                    Err(e) => {
                        let out = rpc_error(Value::Null, -32700, &format!("parse error: {e}"));
                        respond(&mut w, "400 Bad Request", "application/json", out.to_string().as_bytes(), close)
                    }
                },
                // no server-initiated stream; sessions are not tracked
                "GET" => respond(&mut w, "405 Method Not Allowed", "text/plain", b"", close),
                "DELETE" => respond(&mut w, "200 OK", "text/plain", b"", close),
                _ => respond(&mut w, "405 Method Not Allowed", "text/plain", b"", close),
            }
        };
        if res.is_err() || close {
            return;
        }
    }
}

// ------------------------------------------------------------------ endpoint file

/// easyCAD's config folder (the one holding `settings.json`).
pub fn config_dir() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("APPDATA").map(PathBuf::from))
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("easycad"))
}

/// Where the running app writes `{"url", "token", "pid"}` (`EASYCAD_MCP_ENDPOINT`
/// overrides it).
pub fn endpoint_file() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("EASYCAD_MCP_ENDPOINT") {
        return Some(PathBuf::from(p));
    }
    config_dir().map(|d| d.join("mcp-endpoint.json"))
}

/// Write the endpoint file (owner-only on Unix).
pub fn write_endpoint(url: &str, token: &str) -> std::io::Result<PathBuf> {
    let p = endpoint_file().ok_or_else(|| std::io::Error::other("no config folder (HOME unset)"))?;
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d)?;
    }
    let body = serde_json::to_string_pretty(&json!({"url": url, "token": token, "pid": std::process::id()}))?;
    let tmp = p.with_extension("json.tmp");
    {
        let mut o = std::fs::OpenOptions::new();
        o.write(true).create(true).truncate(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut o, 0o600);
        o.open(&tmp)?.write_all(body.as_bytes())?;
    }
    std::fs::rename(&tmp, &p)?;
    Ok(p)
}

/// Remove the endpoint file if this process wrote it.
pub fn remove_endpoint() {
    if let Some(p) = endpoint_file() {
        if read_endpoint().is_some_and(|e| e.pid == Some(std::process::id())) {
            let _ = std::fs::remove_file(p);
        }
    }
}

pub struct Endpoint {
    pub url: String,
    pub token: String,
    pub pid: Option<u32>,
}

pub fn read_endpoint() -> Option<Endpoint> {
    let v: Value = serde_json::from_str(&std::fs::read_to_string(endpoint_file()?).ok()?).ok()?;
    Some(Endpoint {
        url: v["url"].as_str()?.to_string(),
        token: v["token"].as_str()?.to_string(),
        pid: v["pid"].as_u64().map(|p| p as u32),
    })
}

/// A 128-bit random hex token (the standard library's per-process random hash keys).
pub fn new_token() -> String {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};
    let mut out = String::new();
    for i in 0..2u64 {
        let mut h = RandomState::new().build_hasher();
        h.write_u64(i);
        h.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0));
        h.write_u32(std::process::id());
        out.push_str(&format!("{:016x}", h.finish()));
    }
    out
}

// ------------------------------------------------------------------ client

/// POST `body` to `url` (`http://host:port/path`); returns (status, body).
pub fn post(url: &str, token: &str, body: &str, timeout: Duration) -> std::io::Result<(u16, String)> {
    let rest = url.strip_prefix("http://").ok_or_else(|| std::io::Error::other("only http:// URLs"))?;
    let (host, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let addr: SocketAddr = host
        .replace("localhost", "127.0.0.1")
        .parse()
        .map_err(|_| std::io::Error::other(format!("bad address {host}")))?;
    let mut s = TcpStream::connect_timeout(&addr, Duration::from_secs(2))?;
    s.set_read_timeout(Some(timeout))?;
    write!(
        s,
        "POST {path} HTTP/1.1\r\nHost: {host}\r\nAuthorization: Bearer {token}\r\nContent-Type: application/json\r\nAccept: application/json, text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )?;
    s.write_all(body.as_bytes())?;
    let mut r = BufReader::new(s);
    let mut line = String::new();
    r.read_line(&mut line)?;
    let status: u16 = line.split_whitespace().nth(1).and_then(|c| c.parse().ok()).ok_or_else(|| std::io::Error::other("bad status line"))?;
    let mut len = None;
    loop {
        line.clear();
        if r.read_line(&mut line)? == 0 {
            break;
        }
        let l = line.trim_end();
        if l.is_empty() {
            break;
        }
        if let Some((k, v)) = l.split_once(':') {
            if k.trim().eq_ignore_ascii_case("content-length") {
                len = v.trim().parse::<usize>().ok();
            }
        }
    }
    let mut buf = Vec::new();
    match len {
        Some(n) => {
            buf.resize(n, 0);
            r.read_exact(&mut buf)?;
        }
        None => {
            r.read_to_end(&mut buf)?;
        }
    }
    Ok((status, String::from_utf8_lossy(&buf).into_owned()))
}

// ------------------------------------------------------------------ base64

pub fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for c in data.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if c.len() > 1 { T[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if c.len() > 2 { T[n as usize & 63] as char } else { '=' });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn echo() -> Handler {
        Arc::new(|name: &str, args: Value| if name == "undo" { Err("nothing to undo".into()) } else { Ok(vec![json_text(&json!({"tool": name, "args": args}))]) })
    }

    #[test]
    fn schema_is_well_formed() {
        let t = tools();
        let names = tool_names();
        assert!(names.len() >= 16, "{names:?}");
        for tool in t.as_array().unwrap() {
            assert!(tool["description"].as_str().unwrap().len() > 40, "{tool}");
            assert_eq!(tool["inputSchema"]["type"], "object");
        }
    }

    #[test]
    fn base64_matches_rfc4648() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn json_text_keeps_vectors_on_one_line() {
        let t = json_text(&json!({"bbox": {"min": [0.0, -1.5, 2], "max": [1, 2, 3]}, "names": ["a", "b"], "sel": [{"x": 1}]}));
        let s = t["text"].as_str().unwrap();
        assert!(s.contains("\"min\": [0.0, -1.5, 2]"), "{s}");
        assert!(s.contains("\"names\": [\"a\", \"b\"]"), "{s}");
        let back: Value = serde_json::from_str(s).unwrap();
        assert_eq!(back["sel"][0]["x"], 1);
    }

    #[test]
    fn origins() {
        assert!(origin_ok(None));
        assert!(origin_ok(Some("http://localhost:3000")));
        assert!(origin_ok(Some("http://127.0.0.1")));
        assert!(origin_ok(Some("http://[::1]:80")));
        assert!(!origin_ok(Some("https://evil.example")));
        assert!(!origin_ok(Some("http://localhost.evil.example")));
    }

    #[test]
    fn jsonrpc_over_http() {
        let srv = serve(0, "secret".into(), echo()).unwrap();
        let url = srv.url();
        let call = |body: &str, tok: &str| post(&url, tok, body, Duration::from_secs(5)).unwrap();
        // wrong token
        assert_eq!(call(r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#, "nope").0, 401);
        let (s, b) = call(r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26"}}"#, "secret");
        assert_eq!(s, 200);
        let v: Value = serde_json::from_str(&b).unwrap();
        assert_eq!(v["result"]["protocolVersion"], "2025-03-26");
        assert_eq!(v["result"]["serverInfo"]["name"], "easycad");
        // notification: no body
        assert_eq!(call(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#, "secret").0, 202);
        let (_, b) = call(r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#, "secret");
        let v: Value = serde_json::from_str(&b).unwrap();
        assert!(v["result"]["tools"].as_array().unwrap().iter().any(|t| t["name"] == "add_feature"));
        let (_, b) = call(r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"set_param","arguments":{"name":"w","expr":"40"}}}"#, "secret");
        let v: Value = serde_json::from_str(&b).unwrap();
        assert_eq!(v["result"]["isError"], false);
        assert!(v["result"]["content"][0]["text"].as_str().unwrap().contains("\"w\""));
        let (_, b) = call(r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"undo"}}"#, "secret");
        let v: Value = serde_json::from_str(&b).unwrap();
        assert_eq!(v["result"]["isError"], true);
        assert_eq!(v["result"]["content"][0]["text"], "nothing to undo");
        let (_, b) = call(r#"{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"rm_rf"}}"#, "secret");
        assert!(b.contains("unknown tool"));
        // batch
        let (_, b) = call(r#"[{"jsonrpc":"2.0","id":6,"method":"ping"},{"jsonrpc":"2.0","method":"notifications/cancelled"}]"#, "secret");
        let v: Value = serde_json::from_str(&b).unwrap();
        assert_eq!(v.as_array().unwrap().len(), 1);
        srv.stop();
    }
}
