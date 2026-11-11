//! `opstrellis mcp`: a tools-only Model Context Protocol server over stdio (newline-delimited
//! JSON-RPC 2.0), started by an agent's client. The tools are in `agent.rs`.
//!
//! Dual-era, per the 2026-07-28 specification: legacy clients open with `initialize` (2025-11-25 or
//! 2025-06-18); modern ones send the protocol version in each request's `_meta` and may probe with
//! `server/discover`. Each style has its own versions: `server/discover` and the unsupported-version
//! error list the per-request ones only, and a legacy version in `_meta` is refused like any other
//! unsupported one (legacy versions are negotiated by `initialize`). stdout carries protocol
//! messages only.

use crate::agent::{self, Store, Tool};
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex};

/// Newest first; an `initialize` asking for another version gets the first.
pub const LEGACY_VERSIONS: [&str; 2] = ["2025-11-25", "2025-06-18"];
pub const MODERN_VERSIONS: [&str; 1] = ["2026-07-28"];
const VERSION_KEY: &str = "io.modelcontextprotocol/protocolVersion";
const CAPABILITIES_KEY: &str = "io.modelcontextprotocol/clientCapabilities";
const SERVER_INFO_KEY: &str = "io.modelcontextprotocol/serverInfo";
/// Far above any request this server takes; a longer line is answered with an error and skipped.
pub const MAX_MESSAGE_BYTES: usize = 1 << 20;

const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;
const INTERNAL_ERROR: i64 = -32603;
const UNSUPPORTED_VERSION: i64 = -32022;

struct RpcError {
    code: i64,
    message: String,
    data: Option<Value>,
}

fn rpc(code: i64, message: impl Into<String>) -> RpcError {
    RpcError { code, message: message.into(), data: None }
}

fn invalid_params(message: impl Into<String>) -> RpcError {
    rpc(INVALID_PARAMS, message)
}

fn error_response(id: Value, e: RpcError) -> Value {
    let mut error = json!({ "code": e.code, "message": e.message });
    if let Some(data) = e.data {
        error["data"] = data;
    }
    json!({ "jsonrpc": "2.0", "id": id, "error": error })
}

/// Client-sent text, shortened before it is echoed back.
fn shown(s: &str) -> String {
    s.chars().take(64).collect()
}

fn server_info() -> Value {
    json!({ "name": "opstrellis", "title": "OpsTrellis", "version": env!("CARGO_PKG_VERSION") })
}

/// What to do with one line from the client.
#[derive(Debug, PartialEq)]
pub enum Handled {
    Reply(String),
    /// An `ask_user` call: its reply is written when the user answers (see `ask_reply`).
    Ask { id: Value, question: String, modern: bool },
    /// `notifications/cancelled` for this request id.
    Cancel(Value),
    Silent,
}

pub fn handle(store: &Store, line: &[u8]) -> Handled {
    if line.iter().all(u8::is_ascii_whitespace) {
        return Handled::Silent;
    }
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| respond(store, line)))
        .unwrap_or_else(|_| Handled::Reply(error_response(Value::Null, rpc(INTERNAL_ERROR, "Internal error")).to_string()))
}

/// The reply to one line, waiting for an `ask_user` answer in place (no cancellation): for tests.
#[cfg(test)]
pub fn handle_line(store: &Store, line: &[u8]) -> Option<String> {
    match handle(store, line) {
        Handled::Reply(r) => Some(r),
        Handled::Ask { id, question, modern } => Some(ask_reply(&id, modern, agent::ask(store, &question, |_| {}))),
        Handled::Cancel(_) | Handled::Silent => None,
    }
}

/// The `tools/call` reply for an `ask_user` call that ended.
pub fn ask_reply(id: &Value, modern: bool, result: Result<Value, String>) -> String {
    let result = tool_result(result);
    let result = if modern { stamped(result) } else { result };
    json!({ "jsonrpc": "2.0", "id": id, "result": result }).to_string()
}

fn tool_result(result: Result<Value, String>) -> Value {
    match result {
        Ok(v) => json!({ "content": [{ "type": "text", "text": v.to_string() }], "isError": false }),
        Err(why) => json!({ "content": [{ "type": "text", "text": why }], "isError": true }),
    }
}

fn respond(store: &Store, line: &[u8]) -> Handled {
    let reply = |v: Value| Handled::Reply(v.to_string());
    let msg = match serde_json::from_slice::<Value>(line) {
        Ok(Value::Object(m)) => m,
        Ok(Value::Array(_)) => return reply(error_response(Value::Null, rpc(INVALID_REQUEST, "Batches are not supported: send one message per line"))),
        Ok(_) => return reply(error_response(Value::Null, rpc(INVALID_REQUEST, "Invalid Request: a message is a JSON object"))),
        Err(_) => return reply(error_response(Value::Null, rpc(PARSE_ERROR, "Parse error"))),
    };
    // A notification (no id) is never answered; nor is a response, as this server sends no requests.
    let Some(id) = msg.get("id") else {
        let cancelled = msg.get("method").and_then(Value::as_str) == Some("notifications/cancelled");
        return match msg.get("params").and_then(|p| p.get("requestId")) {
            Some(request) if cancelled => Handled::Cancel(request.clone()),
            _ => Handled::Silent,
        };
    };
    if !msg.contains_key("method") && (msg.contains_key("result") || msg.contains_key("error")) {
        return Handled::Silent;
    }
    let id = match id {
        Value::String(_) => id.clone(),
        Value::Number(n) if n.is_i64() || n.is_u64() => id.clone(),
        _ => return reply(error_response(Value::Null, rpc(INVALID_REQUEST, "Invalid Request: id must be a string or an integer"))),
    };
    if msg.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return reply(error_response(id, rpc(INVALID_REQUEST, "Invalid Request: jsonrpc must be \"2.0\"")));
    }
    let Some(method) = msg.get("method").and_then(Value::as_str) else {
        return reply(error_response(id, rpc(INVALID_REQUEST, "Invalid Request: method must be a string")));
    };
    let empty = Map::new();
    let params = match msg.get("params") {
        None => &empty,
        Some(Value::Object(p)) => p,
        Some(_) => return reply(error_response(id, invalid_params("params must be an object"))),
    };
    match dispatch(store, method, params) {
        Ok(Call::Done(result)) => reply(json!({ "jsonrpc": "2.0", "id": id, "result": result })),
        Ok(Call::Ask { question, modern }) => Handled::Ask { id, question, modern },
        Err(e) => reply(error_response(id, e)),
    }
}

/// A request's result, or an `ask_user` call that waits for the user.
enum Call {
    Done(Value),
    Ask { question: String, modern: bool },
}

/// Whether the request is a modern one (protocol version in `_meta`), checking that version.
fn modern(params: &Map<String, Value>) -> Result<bool, RpcError> {
    let Some(meta) = params.get("_meta") else { return Ok(false) };
    let Some(meta) = meta.as_object() else { return Err(invalid_params("_meta must be an object")) };
    let Some(version) = meta.get(VERSION_KEY) else { return Ok(false) };
    let Some(version) = version.as_str() else { return Err(invalid_params(format!("{VERSION_KEY} must be a string"))) };
    if !MODERN_VERSIONS.contains(&version) {
        return Err(RpcError {
            code: UNSUPPORTED_VERSION,
            message: "Unsupported protocol version (earlier versions start with initialize)".into(),
            data: Some(json!({ "supported": MODERN_VERSIONS, "requested": shown(version) })),
        });
    }
    if !meta.get(CAPABILITIES_KEY).is_some_and(Value::is_object) {
        return Err(invalid_params(format!("_meta needs {CAPABILITIES_KEY}")));
    }
    Ok(true)
}

fn dispatch(store: &Store, method: &str, params: &Map<String, Value>) -> Result<Call, RpcError> {
    if method == "initialize" {
        return initialize(store, params).map(Call::Done);
    }
    let modern = modern(params)?;
    let result = match method {
        "ping" => json!({}),
        "server/discover" if modern => json!({
            "supportedVersions": MODERN_VERSIONS,
            "capabilities": { "tools": {} },
            "instructions": agent::instructions(&store.settings()),
        }),
        "server/discover" => return Err(invalid_params(format!("server/discover needs _meta with {VERSION_KEY}"))),
        "tools/list" => tools_list(store, params)?,
        "tools/call" => match tools_call(store, params)? {
            Ok(result) => result,
            Err(question) => return Ok(Call::Ask { question, modern }),
        },
        _ => return Err(rpc(METHOD_NOT_FOUND, format!("Method not found: {}", shown(method)))),
    };
    Ok(Call::Done(if modern { stamped(result) } else { result }))
}

/// What a modern result carries besides its content.
fn stamped(mut result: Value) -> Value {
    if let Value::Object(m) = &mut result {
        m.insert("resultType".into(), json!("complete"));
        m.insert("_meta".into(), json!({ SERVER_INFO_KEY: server_info() }));
    }
    result
}

fn initialize(store: &Store, params: &Map<String, Value>) -> Result<Value, RpcError> {
    let Some(requested) = params.get("protocolVersion").and_then(Value::as_str) else {
        return Err(invalid_params("protocolVersion must be a string"));
    };
    let version = LEGACY_VERSIONS.into_iter().find(|v| *v == requested).unwrap_or(LEGACY_VERSIONS[0]);
    Ok(json!({
        "protocolVersion": version,
        "capabilities": { "tools": {} },
        "serverInfo": server_info(),
        "instructions": agent::instructions(&store.settings()),
    }))
}

fn tools_list(store: &Store, params: &Map<String, Value>) -> Result<Value, RpcError> {
    match params.get("cursor") {
        None | Some(Value::Null) => Ok(json!({ "tools": agent::listed_tools(&store.settings()) })),
        Some(_) => Err(invalid_params("Invalid cursor: the tool list has one page")),
    }
}

/// Ok(Err(question)): an `ask_user` call to hand to the app.
fn tools_call(store: &Store, params: &Map<String, Value>) -> Result<Result<Value, String>, RpcError> {
    let Some(name) = params.get("name").and_then(Value::as_str) else { return Err(invalid_params("name must be a string")) };
    let empty = Map::new();
    let args = match params.get("arguments") {
        None | Some(Value::Null) => &empty,
        Some(Value::Object(a)) => a,
        Some(_) => return Err(invalid_params("arguments must be an object")),
    };
    let tool = Tool::from_name(name).ok_or_else(|| invalid_params(format!("Unknown tool: {}", shown(name))))?;
    if tool == Tool::AskUser {
        return Ok(match agent::question(store, args) {
            Ok(question) => Err(question),
            Err(why) => Ok(tool_result(Err(why))),
        });
    }
    Ok(Ok(tool_result(agent::call(store, tool, args))))
}

#[derive(Debug, PartialEq)]
pub enum Message {
    Line(Vec<u8>),
    TooLong,
}

/// One newline-terminated message (a trailing `\r` is dropped), or None at the end of input. A line
/// past `cap` bytes is read through and dropped, never held in memory.
pub fn read_message(r: &mut impl BufRead, cap: usize) -> std::io::Result<Option<Message>> {
    let (mut line, mut too_long, mut started) = (Vec::new(), false, false);
    loop {
        let buf = match r.fill_buf() {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        if buf.is_empty() {
            return Ok(started.then(|| finish(line, too_long)));
        }
        started = true;
        let newline = buf.iter().position(|&b| b == b'\n');
        let chunk = buf.get(..newline.unwrap_or(buf.len())).unwrap_or_default();
        if !too_long {
            if line.len() + chunk.len() > cap {
                too_long = true;
                line = Vec::new();
            } else {
                line.extend_from_slice(chunk);
            }
        }
        let used = newline.map_or(buf.len(), |i| i + 1);
        r.consume(used);
        if newline.is_some() {
            return Ok(Some(finish(line, too_long)));
        }
    }
}

fn finish(mut line: Vec<u8>, too_long: bool) -> Message {
    if too_long {
        return Message::TooLong;
    }
    if line.last() == Some(&b'\r') {
        line.pop();
    }
    Message::Line(line)
}

/// Serves until stdin closes. Diagnostics go to stderr and never hold what was read.
pub fn serve() -> i32 {
    serve_on(Store::locate(), std::io::stdin().lock(), Arc::new(Mutex::new(std::io::stdout())))
}

/// `ask_user` calls in flight, by request id, with their connection to the app once made.
/// `ask_user` calls in flight, by request id: a ticket that tells this call from a later one
/// reusing its id, and the connection to the app once made.
type Asks = Arc<Mutex<HashMap<String, (u64, Option<UnixStream>)>>>;

/// Records call `key` as in flight; false while a call with that id still is.
fn track_call(asks: &Asks, key: &str, ticket: u64) -> bool {
    let mut in_flight = asks.lock().unwrap_or_else(|e| e.into_inner());
    if in_flight.contains_key(key) {
        return false;
    }
    in_flight.insert(key.to_string(), (ticket, None));
    true
}

/// Takes call `key` out, whichever call holds the id now; returns its connection.
fn cancel_call(asks: &Asks, key: &str) -> Option<UnixStream> {
    asks.lock().unwrap_or_else(|e| e.into_inner()).remove(key).and_then(|(_, conn)| conn)
}

/// The call with `ticket` ended: true (respond) if it was still in flight. A later call that
/// reuses the id after a cancel keeps its entry.
fn end_call(asks: &Asks, key: &str, ticket: u64) -> bool {
    let mut in_flight = asks.lock().unwrap_or_else(|e| e.into_inner());
    let mine = in_flight.get(key).is_some_and(|(t, _)| *t == ticket);
    if mine {
        in_flight.remove(key);
    }
    mine
}

fn write_line<W: Write>(output: &Mutex<W>, line: &str) -> bool {
    let mut out = output.lock().unwrap_or_else(|e| e.into_inner());
    writeln!(out, "{line}").and_then(|()| out.flush()).is_ok()
}

/// Reads requests one at a time, but an `ask_user` call waits on its own thread, so `ping`, other
/// calls and `notifications/cancelled` (which withdraws the question) are handled meanwhile.
pub fn serve_on<W: Write + Send + 'static>(store: Store, mut input: impl BufRead, output: Arc<Mutex<W>>) -> i32 {
    let store = Arc::new(store);
    let asks: Asks = Arc::default();
    let mut tickets = 0u64;
    loop {
        let handled = match read_message(&mut input, MAX_MESSAGE_BYTES) {
            Ok(None) => return 0,
            Ok(Some(Message::Line(line))) => handle(&store, &line),
            Ok(Some(Message::TooLong)) => Handled::Reply(error_response(Value::Null, rpc(INVALID_REQUEST, format!("Message over {MAX_MESSAGE_BYTES} bytes"))).to_string()),
            Err(e) => {
                eprintln!("opstrellis mcp: reading stdin failed: {e}");
                return 1;
            }
        };
        match handled {
            Handled::Reply(reply) => {
                if !write_line(&output, &reply) {
                    return 0;
                }
            }
            Handled::Silent => {}
            Handled::Cancel(request) => {
                if let Some(conn) = cancel_call(&asks, &request.to_string()) {
                    let _ = conn.shutdown(std::net::Shutdown::Both);
                }
            }
            Handled::Ask { id, question, modern } => {
                let key = id.to_string();
                tickets += 1;
                let ticket = tickets;
                if !track_call(&asks, &key, ticket) {
                    write_line(&output, &error_response(id, rpc(INVALID_REQUEST, "Invalid Request: this id is still in use")).to_string());
                    continue;
                }
                let (store, asks, output) = (store.clone(), asks.clone(), output.clone());
                std::thread::spawn(move || {
                    let result = agent::ask(&store, &question, |conn| {
                        let mut in_flight = asks.lock().unwrap_or_else(|e| e.into_inner());
                        match in_flight.get_mut(&key).filter(|(t, _)| *t == ticket) {
                            Some((_, slot)) => *slot = conn.try_clone().ok(),
                            // Cancelled while connecting.
                            None => {
                                let _ = conn.shutdown(std::net::Shutdown::Both);
                            }
                        }
                    });
                    // A cancelled call gets no response.
                    if end_call(&asks, &key, ticket) {
                        write_line(&output, &ask_reply(&id, modern, result));
                    }
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::tests::Fixture;

    fn reply(store: &Store, line: &str) -> Option<Value> {
        let out = handle_line(store, line.as_bytes())?;
        assert!(!out.contains('\n'), "one line per message");
        Some(serde_json::from_str(&out).unwrap())
    }

    fn request(store: &Store, method: &str, params: Value) -> Value {
        reply(store, &json!({ "jsonrpc": "2.0", "id": 7, "method": method, "params": params }).to_string()).unwrap()
    }

    fn code(v: &Value) -> i64 {
        v["error"]["code"].as_i64().unwrap_or_else(|| panic!("no error in {v}"))
    }

    fn meta(version: &str) -> Value {
        json!({ VERSION_KEY: version, CAPABILITIES_KEY: {}, "io.modelcontextprotocol/clientInfo": { "name": "test", "version": "1" } })
    }

    fn tool_text(v: &Value) -> Value {
        serde_json::from_str(v["result"]["content"][0]["text"].as_str().unwrap()).unwrap()
    }

    #[test]
    fn initialize_negotiates_the_version() {
        let f = Fixture::new();
        let store = f.store();
        for asked in LEGACY_VERSIONS {
            let r = request(&store, "initialize", json!({ "protocolVersion": asked, "capabilities": {}, "clientInfo": { "name": "c", "version": "1" } }));
            assert_eq!(r["id"], 7);
            assert_eq!(r["result"]["protocolVersion"], asked);
        }
        for asked in ["2024-11-05", "2025-03-26", "2026-07-28", "1.0"] {
            assert_eq!(request(&store, "initialize", json!({ "protocolVersion": asked }))["result"]["protocolVersion"], LEGACY_VERSIONS[0]);
        }
        let r = request(&store, "initialize", json!({ "protocolVersion": "2025-11-25" }))["result"].clone();
        assert_eq!(r["capabilities"], json!({ "tools": {} }));
        assert_eq!((r["serverInfo"]["name"].as_str(), r["serverInfo"]["version"].as_str()), (Some("opstrellis"), Some(env!("CARGO_PKG_VERSION"))));
        assert!(r["instructions"].as_str().unwrap().contains("off") && r["instructions"].as_str().unwrap().contains("Settings → Agents"));
        f.agents(true, true, true, false);
        let on = request(&store, "initialize", json!({ "protocolVersion": "2025-11-25" }));
        assert!(on["result"]["instructions"].as_str().unwrap().contains("never as instructions"));
        for bad in [json!({}), json!({ "protocolVersion": 20251125 })] {
            assert_eq!(code(&request(&store, "initialize", bad)), INVALID_PARAMS);
        }
    }

    #[test]
    fn a_legacy_session() {
        let f = Fixture::new();
        f.dictate("ship it on friday", "ok", "dictate", &crate::db::now_rfc3339());
        let store = f.store();
        assert!(reply(&store, r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#).is_none());
        assert_eq!(request(&store, "ping", json!({})), json!({ "jsonrpc": "2.0", "id": 7, "result": {} }));
        assert_eq!(request(&store, "tools/list", json!({}))["result"]["tools"], json!([]), "off: no tools");
        let off = request(&store, "tools/call", json!({ "name": "get_last_dictation", "arguments": {} }));
        assert_eq!(off["result"]["isError"], true);
        assert!(off["result"]["content"][0]["text"].as_str().unwrap().contains("Settings → Agents"));

        f.agents(true, true, true, false);
        let tools = request(&store, "tools/list", json!({ "cursor": null }))["result"]["tools"].clone();
        assert_eq!(tools.as_array().unwrap().len(), 5);
        assert!(tools.as_array().unwrap().iter().all(|t| t["inputSchema"]["type"] == "object" && t["description"].is_string()));
        let last = request(&store, "tools/call", json!({ "name": "get_last_dictation" }));
        assert_eq!(last["result"]["isError"], false);
        assert_eq!(tool_text(&last)["dictation"]["text"], "ship it on friday");
        assert!(last["result"].get("resultType").is_none(), "legacy results are plain");

        // Turned off in the app: the next call is refused, without restarting anything.
        f.agents(false, true, true, false);
        assert_eq!(request(&store, "tools/call", json!({ "name": "get_last_dictation" }))["result"]["isError"], true);
        assert_eq!(request(&store, "tools/list", json!({}))["result"]["tools"], json!([]));
    }

    #[test]
    fn tool_call_errors() {
        let f = Fixture::new();
        f.agents(true, true, true, false);
        let store = f.store();
        assert_eq!(code(&request(&store, "tools/call", json!({}))), INVALID_PARAMS);
        assert_eq!(code(&request(&store, "tools/call", json!({ "name": 5 }))), INVALID_PARAMS);
        assert_eq!(code(&request(&store, "tools/call", json!({ "name": "rm_rf" }))), INVALID_PARAMS);
        assert_eq!(code(&request(&store, "tools/call", json!({ "name": "get_note", "arguments": [1] }))), INVALID_PARAMS);
        assert_eq!(code(&request(&store, "tools/list", json!({ "cursor": "next" }))), INVALID_PARAMS);
        let bad = request(&store, "tools/call", json!({ "name": "search_dictations", "arguments": { "limit": 500 } }));
        assert_eq!(bad["result"]["isError"], true, "argument errors are tool errors the model can fix");
        let missing = request(&store, "tools/call", json!({ "name": "get_note", "arguments": { "id": 3 } }));
        assert_eq!(missing["result"]["isError"], true);
    }

    #[test]
    fn json_rpc_errors() {
        let f = Fixture::new();
        let store = f.store();
        let r = |line: &str| reply(&store, line).unwrap();
        assert_eq!((code(&r("{not json")), r("{not json")["id"].clone()), (PARSE_ERROR, Value::Null));
        assert_eq!(code(&r("[]")), INVALID_REQUEST);
        assert_eq!(code(&r(r#"[{"jsonrpc":"2.0","id":1,"method":"ping"}]"#)), INVALID_REQUEST);
        for line in ["1", "\"ping\"", "null", "true"] {
            assert_eq!(code(&r(line)), INVALID_REQUEST, "{line}");
        }
        for id in ["null", "1.5", "{}", "[1]", "true"] {
            let v = r(&format!(r#"{{"jsonrpc":"2.0","id":{id},"method":"ping"}}"#));
            assert_eq!((code(&v), &v["id"]), (INVALID_REQUEST, &Value::Null), "{id}");
        }
        assert_eq!(r(r#"{"jsonrpc":"2.0","id":"abc","method":"ping"}"#)["id"], "abc");
        assert_eq!(code(&r(r#"{"id":1,"method":"ping"}"#)), INVALID_REQUEST);
        assert_eq!(code(&r(r#"{"jsonrpc":"1.0","id":1,"method":"ping"}"#)), INVALID_REQUEST);
        assert_eq!(code(&r(r#"{"jsonrpc":"2.0","id":1}"#)), INVALID_REQUEST);
        assert_eq!(code(&r(r#"{"jsonrpc":"2.0","id":1,"method":42}"#)), INVALID_REQUEST);
        assert_eq!(code(&r(r#"{"jsonrpc":"2.0","id":1,"method":"ping","params":[1]}"#)), INVALID_PARAMS);
        let unknown = r(r#"{"jsonrpc":"2.0","id":9,"method":"resources/list"}"#);
        assert_eq!((code(&unknown), unknown["id"].as_i64()), (METHOD_NOT_FOUND, Some(9)));
        for silent in [
            r#"{"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":1}}"#,
            r#"{"jsonrpc":"2.0","method":"tools/call","params":"garbage"}"#,
            r#"{"jsonrpc":"2.0","method":"no/such/method"}"#,
            r#"{"jsonrpc":"2.0","id":1,"result":{}}"#,
            r#"{"jsonrpc":"2.0","id":1,"error":{"code":1,"message":"x"}}"#,
            "", "   ", "\t",
        ] {
            assert!(reply(&store, silent).is_none(), "{silent}");
        }
    }

    #[test]
    fn a_modern_session() {
        let f = Fixture::new();
        f.agents(true, true, true, false);
        let store = f.store();
        let d = request(&store, "server/discover", json!({ "_meta": meta("2026-07-28") }))["result"].clone();
        assert_eq!((d["resultType"].as_str(), d["supportedVersions"].clone()), (Some("complete"), json!(["2026-07-28"])));
        assert_eq!(d["capabilities"], json!({ "tools": {} }));
        assert_eq!(d["_meta"][SERVER_INFO_KEY]["name"], "opstrellis");
        assert!(d["instructions"].is_string());
        let list = request(&store, "tools/list", json!({ "_meta": meta("2026-07-28") }))["result"].clone();
        assert_eq!((list["resultType"].as_str(), list["tools"].as_array().map(Vec::len)), (Some("complete"), Some(5)));
        let call = request(&store, "tools/call", json!({ "name": "list_notes", "arguments": {}, "_meta": meta("2026-07-28") }));
        assert_eq!((call["result"]["resultType"].as_str(), call["result"]["isError"].as_bool()), (Some("complete"), Some(false)));

        let unsupported = request(&store, "tools/list", json!({ "_meta": meta("2099-01-01") }));
        assert_eq!(code(&unsupported), UNSUPPORTED_VERSION);
        assert_eq!(unsupported["error"]["data"], json!({ "supported": ["2026-07-28"], "requested": "2099-01-01" }), "the same list server/discover gives");
        let legacy = request(&store, "tools/list", json!({ "_meta": meta("2025-11-25") }));
        assert_eq!((code(&legacy), legacy["error"]["data"]["supported"].clone()), (UNSUPPORTED_VERSION, json!(["2026-07-28"])), "legacy versions go through initialize");
        assert_eq!(code(&request(&store, "server/discover", json!({}))), INVALID_PARAMS, "a legacy probe fails, as the client expects");
        assert_eq!(code(&request(&store, "tools/list", json!({ "_meta": { VERSION_KEY: "2026-07-28" } }))), INVALID_PARAMS, "capabilities are required");
        assert_eq!(code(&request(&store, "tools/list", json!({ "_meta": { VERSION_KEY: 1 } }))), INVALID_PARAMS);
        assert_eq!(code(&request(&store, "tools/list", json!({ "_meta": "x" }))), INVALID_PARAMS);
        assert!(request(&store, "tools/list", json!({ "_meta": { "progressToken": 1 } }))["result"].get("resultType").is_none(), "other _meta keys stay legacy");
    }

    #[test]
    fn ask_user_waits_without_blocking_other_requests_and_can_be_cancelled() {
        use std::time::Duration;
        let f = Fixture::empty();
        f.settings(json!({ "agents": { "enabled": true, "voice": true } }));
        let board = crate::ask::tests::leak();
        let listener = crate::ask::bind(&f.dir.join(crate::ask::SOCKET)).unwrap();
        std::thread::spawn(move || crate::ask::accept_loop(listener, board, Arc::new(|| Ok(())), crate::ask::QUESTION_TIMEOUT));
        let (client, server) = UnixStream::pair().unwrap();
        let output = Arc::new(Mutex::new(Vec::<u8>::new()));
        let (store, out) = (f.store(), output.clone());
        let served = std::thread::spawn(move || serve_on(store, std::io::BufReader::new(server), out));
        let send = |v: Value| writeln!(&mut &client, "{v}").unwrap();
        let call = |id: i64, question: &str| json!({ "jsonrpc": "2.0", "id": id, "method": "tools/call", "params": { "name": "ask_user", "arguments": { "question": question } } });
        let lines = || String::from_utf8(output.lock().unwrap().clone()).unwrap().lines().map(|l| serde_json::from_str::<Value>(l).unwrap()).collect::<Vec<_>>();
        let reply_to = |id: i64| lines().into_iter().find(|l| l["id"] == id);
        let wait = |what: &dyn Fn() -> bool| {
            for _ in 0..500 {
                if what() {
                    return;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            panic!("timed out; output so far: {:?}", lines());
        };

        send(call(5, "Deploy now?"));
        wait(&|| board.is_pending());
        send(json!({ "jsonrpc": "2.0", "id": 6, "method": "ping" }));
        wait(&|| reply_to(6).is_some());
        assert!(reply_to(5).is_none(), "still waiting for the user");
        let question = crate::ask::tests::show_and_wait(board);
        assert!(board.answer(question, "Yes, go ahead."));
        wait(&|| reply_to(5).is_some());
        let answered = reply_to(5).unwrap();
        assert_eq!(answered["result"]["isError"], false);
        assert_eq!(tool_text(&answered), json!({ "answer": "Yes, go ahead." }));

        send(call(7, "Sure?"));
        wait(&|| board.is_pending());
        send(json!({ "jsonrpc": "2.0", "method": "notifications/cancelled", "params": { "requestId": 7, "reason": "user stopped" } }));
        wait(&|| !board.is_pending());
        std::thread::sleep(Duration::from_millis(300));
        assert!(reply_to(7).is_none(), "a cancelled call gets no response");

        f.settings(json!({ "agents": { "enabled": true, "voice": false } }));
        send(call(8, "Allowed?"));
        wait(&|| reply_to(8).is_some());
        assert_eq!(reply_to(8).unwrap()["result"]["isError"], true);
        assert!(!board.is_pending());

        client.shutdown(std::net::Shutdown::Write).unwrap();
        assert_eq!(served.join().unwrap(), 0);
    }

    #[test]
    fn a_reused_id_after_a_cancel_keeps_its_own_call() {
        let asks: Asks = Arc::default();
        assert!(track_call(&asks, "7", 1));
        assert!(!track_call(&asks, "7", 2), "still in flight");
        assert!(cancel_call(&asks, "7").is_none(), "cancelled before connecting");
        assert!(track_call(&asks, "7", 3), "the id is free again");
        assert!(!end_call(&asks, "7", 1), "the cancelled call ends: no response, and the new call stays");
        assert!(asks.lock().unwrap().contains_key("7"));
        assert!(end_call(&asks, "7", 3));
        assert!(!end_call(&asks, "7", 3), "answered once");
    }

    #[test]
    fn ask_user_is_handed_off_not_answered_inline() {
        let f = Fixture::empty();
        f.settings(json!({ "agents": { "enabled": true, "voice": true } }));
        let store = f.store();
        let line = json!({ "jsonrpc": "2.0", "id": "a", "method": "tools/call", "params": { "name": "ask_user", "arguments": { "question": " Ready? " }, "_meta": meta("2026-07-28") } }).to_string();
        assert_eq!(handle(&store, line.as_bytes()), Handled::Ask { id: json!("a"), question: "Ready?".into(), modern: true });
        let cancel = r#"{"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":"a"}}"#;
        assert_eq!(handle(&store, cancel.as_bytes()), Handled::Cancel(json!("a")));
        assert_eq!(handle(&store, br#"{"jsonrpc":"2.0","method":"notifications/cancelled","params":{}}"#), Handled::Silent);
        let reply: Value = serde_json::from_str(&ask_reply(&json!("a"), true, Err("The user didn't answer.".into()))).unwrap();
        assert_eq!((reply["result"]["isError"].as_bool(), reply["result"]["resultType"].as_str()), (Some(true), Some("complete")));
        let gone = reply_line(&store, &line);
        assert!(gone["result"]["content"][0]["text"].as_str().unwrap().contains("Open OpsTrellis"), "no app: {gone}");
    }

    fn reply_line(store: &Store, line: &str) -> Value {
        serde_json::from_str(&handle_line(store, line.as_bytes()).unwrap()).unwrap()
    }

    #[test]
    fn reads_one_message_per_line() {
        let mut input: &[u8] = b"{\"a\":1}\r\n\n{\"b\":2}";
        assert_eq!(read_message(&mut input, 100).unwrap(), Some(Message::Line(b"{\"a\":1}".to_vec())));
        assert_eq!(read_message(&mut input, 100).unwrap(), Some(Message::Line(Vec::new())));
        assert_eq!(read_message(&mut input, 100).unwrap(), Some(Message::Line(b"{\"b\":2}".to_vec())), "the last line needs no newline");
        assert_eq!(read_message(&mut input, 100).unwrap(), None);

        let huge = [vec![b'x'; 5000], b"\nnext\n".to_vec()].concat();
        let mut input = std::io::BufReader::with_capacity(64, huge.as_slice());
        assert_eq!(read_message(&mut input, 1000).unwrap(), Some(Message::TooLong));
        assert_eq!(read_message(&mut input, 1000).unwrap(), Some(Message::Line(b"next".to_vec())), "the next message still works");
        assert_eq!(read_message(&mut input, 1000).unwrap(), None);
        let mut cut: &[u8] = &[b'y'; 2000];
        assert_eq!(read_message(&mut cut, 1000).unwrap(), Some(Message::TooLong));
    }

    #[test]
    fn garbage_never_panics() {
        let f = Fixture::new();
        f.agents(true, true, true, true);
        let store = f.store();
        let mut lines: Vec<Vec<u8>> = [
            "{", "}", "{}", "[", "[[]]", "[{}]", "\"\"", "-0", "1e999", "{\"jsonrpc\":\"2.0\"}", "{\"id\":1}", "{\"method\":\"ping\"}",
            "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{\"protocolVersion\":null}}",
            "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/call\",\"params\":{\"name\":\"get_note\",\"arguments\":{\"id\":-9223372036854775808}}}",
            "{\"jsonrpc\":\"2.0\",\"id\":18446744073709551615,\"method\":\"tools/call\",\"params\":{\"name\":\"search_dictations\",\"arguments\":{\"query\":\"%_\\u0000\"}}}",
            "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/call\",\"params\":{\"name\":\"add_dictionary_term\",\"arguments\":{\"phrase\":\"\\u202e\\ud83d\\ude00\"}}}",
            "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/call\",\"params\":{\"name\":\"list_notes\",\"arguments\":{\"kind\":{\"a\":[]},\"limit\":1e308}}}",
            "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/list\",\"params\":{\"_meta\":{\"io.modelcontextprotocol/protocolVersion\":[]}}}",
        ]
        .iter()
        .map(|s| s.as_bytes().to_vec())
        .collect();
        lines.push(vec![0xff, 0xfe, b'{', 0x80]);
        lines.push(format!("{}{}", "[".repeat(10_000), "]".repeat(10_000)).into_bytes());
        lines.push(format!("{{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"{}\"}}", "m".repeat(200_000)).into_bytes());
        let alphabet = b"{}[]\":,0123456789.-eE truefalsnul\\uidjsonrpcmethodparams_meta\xff";
        let mut seed: u64 = 0x9e37_79b9_7f4a_7c15;
        for n in 0..300 {
            let mut line = Vec::new();
            for _ in 0..(n % 97) {
                seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                line.push(alphabet[(seed >> 33) as usize % alphabet.len()]);
            }
            lines.push(line);
        }
        for line in lines {
            if let Some(out) = handle_line(&store, &line) {
                let v: Value = serde_json::from_str(&out).unwrap();
                assert_eq!(v["jsonrpc"], "2.0");
                assert!(v.get("result").is_some() != v.get("error").is_some());
                assert!(!out.contains('\n') && out.len() < 1_000, "errors don't echo whole requests");
            }
        }
    }
}
