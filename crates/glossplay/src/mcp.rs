//! The Model Context Protocol over stdio: one JSON-RPC 2.0 message per line.
//!
//! I serve both eras of the protocol. Clients up to revision 2025-11-25 open with an
//! `initialize` handshake; from 2026-07-28 on there is no handshake and every request carries
//! its protocol version and client capabilities in `params._meta`. Which one a request is,
//! is decided per request, so either kind of client works without configuration.
//!
//! This file knows nothing about the player: tool calls leave here as `ToolCall`s and their
//! answers come back through `Outbox::finish`.

use serde_json::{Map, Value, json};
use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// The stateless revision, with per-request metadata.
pub const MODERN: &str = "2026-07-28";
/// Handshake revisions, newest first; an unknown version asked in `initialize` gets the first.
pub const LEGACY: [&str; 4] = ["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];

const META_VERSION: &str = "io.modelcontextprotocol/protocolVersion";
const META_CAPABILITIES: &str = "io.modelcontextprotocol/clientCapabilities";
const META_SERVER: &str = "io.modelcontextprotocol/serverInfo";

pub const PARSE_ERROR: i64 = -32700;
pub const INVALID_REQUEST: i64 = -32600;
pub const METHOD_NOT_FOUND: i64 = -32601;
pub const INVALID_PARAMS: i64 = -32602;
pub const UNSUPPORTED_VERSION: i64 = -32022;

const INSTRUCTIONS: &str = "Glossplay is a video player for language learners. Its window \
appears on the user's desktop with the first command that needs it (open, play, screenshot \
and the like); `status` and other questions do not open it. If the user closes the window, \
playback pauses and the window hides until the next such command. Open a file or link with \
`open`, then `wait_for` tracks_ready before choosing subtitles. Every tool answers once its action has taken effect \
(a seek has landed, a word card is open). Coordinates are physical pixels: window-relative \
ones start at the top-left of the drawing area, screen ones are for a screen recorder.";

pub fn server_info() -> Value {
    json!({
        "name": "glossplay",
        "title": "Glossplay",
        "version": env!("CARGO_PKG_VERSION"),
    })
}

pub fn supported_versions() -> Vec<&'static str> {
    std::iter::once(MODERN).chain(LEGACY).collect()
}

#[derive(Debug, Clone, PartialEq)]
pub struct ToolCall {
    pub id: Value,
    pub name: String,
    pub args: Value,
    /// Asked with 2026-07-28 metadata: the answer carries `resultType` and `serverInfo`.
    pub modern: bool,
}

#[derive(Debug, PartialEq)]
pub enum Action {
    /// A message to write to stdout as it is.
    Send(Value),
    /// A tool to run in the player; the answer is sent when it is done.
    Call(ToolCall),
    /// The client gave up on a request; nothing more may be sent for it.
    Cancel(Value),
}

/// What the protocol needs to know about the tools.
pub struct Protocol {
    tools: Value,
    names: Vec<String>,
    /// The version agreed in `initialize`, when a handshake client opened the session.
    pub legacy_version: Option<String>,
}

pub fn error(id: Value, code: i64, message: impl Into<String>) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message.into()}})
}

fn error_with(id: Value, code: i64, message: &str, data: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message, "data": data}})
}

/// A successful response; a modern request gets the fields every 2026-07-28 result has.
pub fn result(id: Value, mut body: Map<String, Value>, modern: bool) -> Value {
    if modern {
        body.insert("resultType".into(), json!("complete"));
        if let Some(meta) = body
            .entry("_meta")
            .or_insert_with(|| json!({}))
            .as_object_mut()
        {
            meta.insert(META_SERVER.into(), server_info());
        }
    }
    json!({"jsonrpc": "2.0", "id": id, "result": Value::Object(body)})
}

/// The answer to `tools/call`: the tool's JSON as text, or its error message with `isError`.
pub fn tool_result(id: Value, modern: bool, outcome: Result<Value, String>) -> Value {
    let (text, is_error) = match outcome {
        Ok(Value::String(s)) => (s, false),
        Ok(v) => (v.to_string(), false),
        Err(e) => (e, true),
    };
    let mut body = Map::new();
    body.insert("content".into(), json!([{"type": "text", "text": text}]));
    body.insert("isError".into(), json!(is_error));
    result(id, body, modern)
}

fn object(v: Value) -> Map<String, Value> {
    match v {
        Value::Object(m) => m,
        _ => Map::new(),
    }
}

impl Protocol {
    /// `tools` is the array `tools/list` returns; each entry has a `name`.
    pub fn new(tools: Value) -> Protocol {
        let names = tools
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|t| t["name"].as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        Protocol {
            tools,
            names,
            legacy_version: None,
        }
    }

    /// Reacts to one line read from the client.
    pub fn handle(&mut self, line: &str) -> Vec<Action> {
        let msg: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(e) => {
                return vec![Action::Send(error(
                    Value::Null,
                    PARSE_ERROR,
                    format!("Parse error: {e}"),
                ))];
            }
        };
        let Value::Object(obj) = msg else {
            let text = if msg.is_array() {
                "Invalid Request: batches are not supported"
            } else {
                "Invalid Request: a message must be a JSON object"
            };
            return vec![Action::Send(error(Value::Null, INVALID_REQUEST, text))];
        };
        let id = obj.get("id").cloned();
        let valid_id = id
            .as_ref()
            .is_some_and(|i| i.is_string() || i.is_i64() || i.is_u64());
        if obj.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
            let id = id.filter(|_| valid_id).unwrap_or(Value::Null);
            return vec![Action::Send(error(
                id,
                INVALID_REQUEST,
                "Invalid Request: jsonrpc must be \"2.0\"",
            ))];
        }
        let Some(method) = obj.get("method").and_then(Value::as_str) else {
            // A response from the client: I never send requests, so there is nothing to match.
            if obj.contains_key("result") || obj.contains_key("error") {
                return Vec::new();
            }
            let id = id.filter(|_| valid_id).unwrap_or(Value::Null);
            return vec![Action::Send(error(
                id,
                INVALID_REQUEST,
                "Invalid Request: no method",
            ))];
        };
        let params = obj.get("params").cloned().unwrap_or_else(|| json!({}));
        let Some(id) = id else {
            return match method {
                "notifications/cancelled" => params
                    .get("requestId")
                    .cloned()
                    .map(Action::Cancel)
                    .into_iter()
                    .collect(),
                _ => Vec::new(),
            };
        };
        if !valid_id {
            return vec![Action::Send(error(
                Value::Null,
                INVALID_REQUEST,
                "Invalid Request: id must be a string or an integer",
            ))];
        }
        if !params.is_object() {
            return vec![Action::Send(error(
                id,
                INVALID_PARAMS,
                "Invalid params: params must be an object",
            ))];
        }
        self.request(id, method, params)
    }

    fn request(&mut self, id: Value, method: &str, params: Value) -> Vec<Action> {
        let meta = params.get("_meta");
        let modern = match meta.and_then(|m| m.get(META_VERSION)) {
            None => false,
            Some(Value::String(v)) if v == MODERN => {
                if meta.and_then(|m| m.get(META_CAPABILITIES)).is_none() {
                    return vec![Action::Send(error(
                        id,
                        INVALID_PARAMS,
                        format!("Invalid params: _meta lacks {META_CAPABILITIES}"),
                    ))];
                }
                true
            }
            Some(v) => {
                return vec![Action::Send(error_with(
                    id,
                    UNSUPPORTED_VERSION,
                    "Unsupported protocol version",
                    json!({"supported": supported_versions(), "requested": v}),
                ))];
            }
        };
        let send = |v: Value| vec![Action::Send(v)];
        match method {
            "initialize" => {
                let asked = params["protocolVersion"].as_str().unwrap_or_default();
                let version = LEGACY
                    .iter()
                    .find(|v| **v == asked)
                    .copied()
                    .unwrap_or(LEGACY[0]);
                self.legacy_version = Some(version.to_owned());
                send(result(
                    id,
                    object(json!({
                        "protocolVersion": version,
                        "capabilities": {"tools": {"listChanged": false}},
                        "serverInfo": server_info(),
                        "instructions": INSTRUCTIONS,
                    })),
                    false,
                ))
            }
            // Gone from 2026-07-28, but harmless to answer whoever still asks.
            "ping" => send(result(id, Map::new(), modern)),
            "server/discover" if !modern => send(error(
                id,
                INVALID_PARAMS,
                format!(
                    "Invalid params: server/discover needs _meta.{META_VERSION} = \"{MODERN}\" \
                     and _meta.{META_CAPABILITIES}"
                ),
            )),
            "server/discover" => send(result(
                id,
                object(json!({
                    "supportedVersions": supported_versions(),
                    "capabilities": {"tools": {}},
                    "instructions": INSTRUCTIONS,
                    "ttlMs": 3_600_000,
                    "cacheScope": "private",
                })),
                true,
            )),
            "tools/list" => {
                let mut body = object(json!({"tools": self.tools}));
                if modern {
                    body.insert("ttlMs".into(), json!(3_600_000));
                    body.insert("cacheScope".into(), json!("private"));
                }
                send(result(id, body, modern))
            }
            "tools/call" => {
                let Some(name) = params["name"].as_str() else {
                    return send(error(
                        id,
                        INVALID_PARAMS,
                        "Invalid params: tools/call needs a tool name",
                    ));
                };
                if !self.names.iter().any(|n| n == name) {
                    return send(error(id, INVALID_PARAMS, format!("Unknown tool: {name}")));
                }
                let args = match params.get("arguments") {
                    None | Some(Value::Null) => json!({}),
                    Some(a @ Value::Object(_)) => a.clone(),
                    Some(_) => {
                        return send(error(
                            id,
                            INVALID_PARAMS,
                            "Invalid params: arguments must be an object",
                        ));
                    }
                };
                vec![Action::Call(ToolCall {
                    id,
                    name: name.to_owned(),
                    args,
                    modern,
                })]
            }
            _ => send(error(
                id,
                METHOD_NOT_FOUND,
                format!("Method not found: {method}"),
            )),
        }
    }
}

/// Writes messages to the client and keeps track of calls still owed an answer, so that a
/// call is answered exactly once: by the player, by the watchdog when the player is stuck,
/// or never when the client cancelled it.
pub struct Outbox {
    out: Mutex<Box<dyn Write + Send>>,
    owed: Mutex<HashMap<String, Owed>>,
}

struct Owed {
    id: Value,
    modern: bool,
    tool: String,
    deadline: Instant,
}

fn key(id: &Value) -> String {
    id.to_string()
}

impl Outbox {
    pub fn new(out: Box<dyn Write + Send>) -> Outbox {
        Outbox {
            out: Mutex::new(out),
            owed: Mutex::default(),
        }
    }

    pub fn send(&self, msg: &Value) {
        // serde_json never puts a raw newline into a string, so one message is one line.
        let mut line = msg.to_string();
        line.push('\n');
        let mut out = self.out.lock().unwrap_or_else(|e| e.into_inner());
        let _ = out.write_all(line.as_bytes()).and_then(|()| out.flush());
    }

    /// A call has gone to the player; past `limit` the watchdog answers it instead.
    pub fn owe(&self, call: &ToolCall, limit: Duration) {
        self.owed.lock().unwrap().insert(
            key(&call.id),
            Owed {
                id: call.id.clone(),
                modern: call.modern,
                tool: call.name.clone(),
                deadline: Instant::now() + limit,
            },
        );
    }

    /// Sends the answer to a call, unless it was answered or cancelled already.
    pub fn finish(&self, id: &Value, modern: bool, outcome: Result<Value, String>) -> bool {
        if self.owed.lock().unwrap().remove(&key(id)).is_none() {
            return false;
        }
        self.send(&tool_result(id.clone(), modern, outcome));
        true
    }

    pub fn cancel(&self, id: &Value) {
        self.owed.lock().unwrap().remove(&key(id));
    }

    /// Answers with an error every call past its deadline.
    pub fn expire(&self, now: Instant, why: &str) {
        let late: Vec<Owed> = {
            let mut owed = self.owed.lock().unwrap();
            let keys: Vec<String> = owed
                .iter()
                .filter(|(_, o)| o.deadline <= now)
                .map(|(k, _)| k.clone())
                .collect();
            keys.iter().filter_map(|k| owed.remove(k)).collect()
        };
        for o in late {
            let text = format!("{}: {why}", o.tool);
            self.send(&tool_result(o.id, o.modern, Err(text)));
        }
    }

    /// Answers every call still open with an error, on the way out.
    pub fn fail_all(&self, why: &str) {
        self.expire(Instant::now() + Duration::from_secs(1 << 30), why);
    }

    #[cfg(test)]
    pub fn owed_count(&self) -> usize {
        self.owed.lock().unwrap().len()
    }
}

/// Reads requests line by line until the client closes stdin. Protocol messages are answered
/// here; tool calls go to `dispatch`.
pub fn serve(
    input: impl BufRead,
    protocol: &mut Protocol,
    outbox: &Outbox,
    mut dispatch: impl FnMut(Action),
) {
    const MAX_LINE: usize = 16 << 20;
    let mut input = input;
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match std::io::Read::take(&mut input, MAX_LINE as u64).read_until(b'\n', &mut buf) {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
        if buf.len() >= MAX_LINE && buf.last() != Some(&b'\n') {
            // Skip the rest of an oversized line rather than read it into memory.
            let mut rest = Vec::new();
            loop {
                rest.clear();
                match std::io::Read::take(&mut input, 1 << 20).read_until(b'\n', &mut rest) {
                    Ok(0) | Err(_) => return,
                    Ok(_) if rest.last() == Some(&b'\n') => break,
                    Ok(_) => {}
                }
            }
            outbox.send(&error(
                Value::Null,
                INVALID_REQUEST,
                "Invalid Request: message too long",
            ));
            continue;
        }
        let line = String::from_utf8_lossy(&buf);
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        for action in protocol.handle(line) {
            match action {
                Action::Send(v) => outbox.send(&v),
                other => dispatch(other),
            }
        }
    }
}

/// Takes stdin and stdout over for the protocol. Anything else in the process that prints, or
/// any helper that inherits the standard handles, must not end up in the protocol stream:
/// stdout is pointed at stderr and stdin at NUL from here on.
#[cfg(windows)]
pub fn take_stdio() -> std::io::Result<(std::fs::File, std::fs::File)> {
    use std::ffi::c_void;
    use std::fs::File;
    use std::os::windows::io::{AsHandle, AsRawHandle, IntoRawHandle};
    use windows_sys::Win32::Foundation::{HANDLE_FLAG_INHERIT, SetHandleInformation};

    const STD_INPUT_HANDLE: u32 = -10i32 as u32;
    const STD_OUTPUT_HANDLE: u32 = -11i32 as u32;
    const STD_ERROR_HANDLE: u32 = -12i32 as u32;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetStdHandle(which: u32) -> *mut c_void;
        fn SetStdHandle(which: u32, handle: *mut c_void) -> i32;
    }

    let missing = || {
        std::io::Error::other(
            "--mcp needs stdin and stdout connected to an MCP client (pipes); none were given",
        )
    };
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    if stdin.as_raw_handle().is_null() || stdout.as_raw_handle().is_null() {
        return Err(missing());
    }
    // Duplicates are not inheritable, so no child ever holds the protocol pipes open.
    let input = File::from(stdin.as_handle().try_clone_to_owned()?);
    let output = File::from(stdout.as_handle().try_clone_to_owned()?);
    let nul = File::open("NUL")?.into_raw_handle();
    unsafe {
        SetHandleInformation(stdin.as_raw_handle(), HANDLE_FLAG_INHERIT, 0);
        SetHandleInformation(stdout.as_raw_handle(), HANDLE_FLAG_INHERIT, 0);
        SetStdHandle(STD_INPUT_HANDLE, nul);
        SetStdHandle(STD_OUTPUT_HANDLE, GetStdHandle(STD_ERROR_HANDLE));
    }
    Ok((input, output))
}

#[cfg(not(windows))]
pub fn take_stdio() -> std::io::Result<(std::fs::File, std::fs::File)> {
    use std::os::fd::AsFd;
    let input = std::fs::File::from(std::io::stdin().as_fd().try_clone_to_owned()?);
    let output = std::fs::File::from(std::io::stdout().as_fd().try_clone_to_owned()?);
    Ok((input, output))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn tools() -> Value {
        json!([
            {"name": "play", "description": "Play", "inputSchema": {"type": "object"}},
            {"name": "seek", "description": "Seek", "inputSchema": {"type": "object"}},
        ])
    }

    fn one(p: &mut Protocol, line: &str) -> Value {
        match p.handle(line).pop() {
            Some(Action::Send(v)) => v,
            other => panic!("expected a message, got {other:?}"),
        }
    }

    fn modern_meta() -> Value {
        json!({
            "io.modelcontextprotocol/protocolVersion": MODERN,
            "io.modelcontextprotocol/clientCapabilities": {},
            "io.modelcontextprotocol/clientInfo": {"name": "t", "version": "1"},
        })
    }

    #[test]
    fn initialize_answers_the_version_asked_or_the_newest_handshake_one() {
        let mut p = Protocol::new(tools());
        let r = one(
            &mut p,
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"c","version":"1"}}}"#,
        );
        assert_eq!(r["id"], 1);
        assert_eq!(r["result"]["protocolVersion"], "2025-06-18");
        assert_eq!(
            r["result"]["capabilities"]["tools"],
            json!({"listChanged": false})
        );
        assert_eq!(r["result"]["serverInfo"]["name"], "glossplay");
        assert!(r["result"].get("resultType").is_none(), "{r}");
        assert_eq!(p.legacy_version.as_deref(), Some("2025-06-18"));

        let r = one(
            &mut p,
            r#"{"jsonrpc":"2.0","id":"x","method":"initialize","params":{"protocolVersion":"1999-01-01"}}"#,
        );
        assert_eq!(r["id"], "x");
        assert_eq!(r["result"]["protocolVersion"], LEGACY[0]);
    }

    #[test]
    fn notifications_get_no_answer_and_cancel_is_passed_on() {
        let mut p = Protocol::new(tools());
        assert!(
            p.handle(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#)
                .is_empty()
        );
        assert_eq!(
            p.handle(
                r#"{"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":7}}"#
            ),
            vec![Action::Cancel(json!(7))]
        );
        // A response from the client is not answered either.
        assert!(
            p.handle(r#"{"jsonrpc":"2.0","id":3,"result":{}}"#)
                .is_empty()
        );
    }

    #[test]
    fn ping_and_tools_list() {
        let mut p = Protocol::new(tools());
        let r = one(&mut p, r#"{"jsonrpc":"2.0","id":2,"method":"ping"}"#);
        assert_eq!(r, json!({"jsonrpc": "2.0", "id": 2, "result": {}}));
        let r = one(
            &mut p,
            r#"{"jsonrpc":"2.0","id":3,"method":"tools/list","params":{}}"#,
        );
        assert_eq!(r["result"]["tools"], tools());
        assert!(r["result"].get("ttlMs").is_none());
    }

    #[test]
    fn tool_calls_go_to_the_player_and_unknown_ones_are_rejected() {
        let mut p = Protocol::new(tools());
        let got = p.handle(
            r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"seek","arguments":{"seconds":3}}}"#,
        );
        assert_eq!(
            got,
            vec![Action::Call(ToolCall {
                id: json!(4),
                name: "seek".into(),
                args: json!({"seconds": 3}),
                modern: false,
            })]
        );
        let got =
            p.handle(r#"{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"play"}}"#);
        assert!(matches!(&got[0], Action::Call(c) if c.args == json!({})));

        let r = one(
            &mut p,
            r#"{"jsonrpc":"2.0","id":6,"method":"tools/call","params":{"name":"fly"}}"#,
        );
        assert_eq!(r["error"]["code"], INVALID_PARAMS);
        assert_eq!(r["error"]["message"], "Unknown tool: fly");
        let r = one(
            &mut p,
            r#"{"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"play","arguments":[1]}}"#,
        );
        assert_eq!(r["error"]["code"], INVALID_PARAMS);
    }

    #[test]
    fn broken_input_gets_json_rpc_errors() {
        let mut p = Protocol::new(tools());
        let r = one(&mut p, r#"{"jsonrpc":"2.0","id":1,"method":"#);
        assert_eq!(r["error"]["code"], PARSE_ERROR);
        assert_eq!(r["id"], Value::Null);
        let r = one(&mut p, r#"[{"jsonrpc":"2.0","id":1,"method":"ping"}]"#);
        assert_eq!(r["error"]["code"], INVALID_REQUEST);
        let r = one(&mut p, r#"{"jsonrpc":"1.0","id":9,"method":"ping"}"#);
        assert_eq!(
            (r["id"].clone(), r["error"]["code"].clone()),
            (json!(9), json!(INVALID_REQUEST))
        );
        let r = one(&mut p, r#"{"jsonrpc":"2.0","id":null,"method":"ping"}"#);
        assert_eq!(r["error"]["code"], INVALID_REQUEST);
        let r = one(
            &mut p,
            r#"{"jsonrpc":"2.0","id":2,"method":"resources/list"}"#,
        );
        assert_eq!(r["error"]["code"], METHOD_NOT_FOUND);
        assert_eq!(r["id"], 2);
        let r = one(
            &mut p,
            r#"{"jsonrpc":"2.0","id":3,"method":"ping","params":5}"#,
        );
        assert_eq!(r["error"]["code"], INVALID_PARAMS);
    }

    #[test]
    fn modern_requests_carry_their_version_and_get_result_type() {
        let mut p = Protocol::new(tools());
        let req = json!({"jsonrpc": "2.0", "id": "d", "method": "server/discover",
            "params": {"_meta": modern_meta()}});
        let r = one(&mut p, &req.to_string());
        assert_eq!(r["result"]["resultType"], "complete");
        assert_eq!(r["result"]["supportedVersions"][0], MODERN);
        assert!(
            r["result"]["supportedVersions"]
                .as_array()
                .unwrap()
                .contains(&json!("2025-11-25"))
        );
        assert_eq!(r["result"]["capabilities"], json!({"tools": {}}));
        assert_eq!(
            r["result"]["_meta"]["io.modelcontextprotocol/serverInfo"]["name"],
            "glossplay"
        );

        let req = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list",
            "params": {"_meta": modern_meta()}});
        let r = one(&mut p, &req.to_string());
        assert_eq!(r["result"]["resultType"], "complete");
        assert_eq!(r["result"]["cacheScope"], "private");
        assert!(r["result"]["ttlMs"].is_u64());

        let req = json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call",
            "params": {"name": "play", "_meta": modern_meta()}});
        assert!(matches!(&p.handle(&req.to_string())[0], Action::Call(c) if c.modern));

        // Without metadata, discovery is not a modern request and cannot be answered.
        let r = one(
            &mut p,
            r#"{"jsonrpc":"2.0","id":3,"method":"server/discover"}"#,
        );
        assert_eq!(r["error"]["code"], INVALID_PARAMS);
    }

    #[test]
    fn an_unknown_version_or_missing_capabilities_is_refused() {
        let mut p = Protocol::new(tools());
        let req = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list",
            "params": {"_meta": {"io.modelcontextprotocol/protocolVersion": "1900-01-01",
                "io.modelcontextprotocol/clientCapabilities": {}}}});
        let r = one(&mut p, &req.to_string());
        assert_eq!(r["error"]["code"], UNSUPPORTED_VERSION);
        assert_eq!(r["error"]["data"]["requested"], "1900-01-01");
        assert_eq!(r["error"]["data"]["supported"][0], MODERN);

        let req = json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list",
            "params": {"_meta": {"io.modelcontextprotocol/protocolVersion": MODERN}}});
        let r = one(&mut p, &req.to_string());
        assert_eq!(r["error"]["code"], INVALID_PARAMS);
    }

    #[test]
    fn tool_results_and_errors() {
        let r = tool_result(json!(1), false, Ok(json!({"pos": 1.5})));
        assert_eq!(r["result"]["content"][0]["type"], "text");
        assert_eq!(r["result"]["content"][0]["text"], r#"{"pos":1.5}"#);
        assert_eq!(r["result"]["isError"], false);
        let r = tool_result(json!(2), true, Err("no file is open".into()));
        assert_eq!(r["result"]["isError"], true);
        assert_eq!(r["result"]["content"][0]["text"], "no file is open");
        assert_eq!(r["result"]["resultType"], "complete");
    }

    #[derive(Clone, Default)]
    struct Sink(Arc<Mutex<Vec<u8>>>);

    impl Write for Sink {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl Sink {
        fn lines(&self) -> Vec<Value> {
            String::from_utf8(self.0.lock().unwrap().clone())
                .unwrap()
                .lines()
                .map(|l| serde_json::from_str(l).unwrap())
                .collect()
        }
    }

    #[test]
    fn a_call_is_answered_once_and_late_ones_by_the_watchdog() {
        let sink = Sink::default();
        let outbox = Outbox::new(Box::new(sink.clone()));
        let call = |id: i64| ToolCall {
            id: json!(id),
            name: "seek".into(),
            args: json!({}),
            modern: false,
        };
        outbox.owe(&call(1), Duration::from_secs(60));
        outbox.owe(&call(2), Duration::ZERO);
        outbox.owe(&call(3), Duration::from_secs(60));
        outbox.cancel(&json!(3));
        assert!(!outbox.finish(&json!(3), false, Ok(json!("late"))));
        outbox.expire(Instant::now(), "the player did not answer");
        assert!(outbox.finish(&json!(1), false, Ok(json!("ok"))));
        assert!(!outbox.finish(&json!(1), false, Ok(json!("again"))));
        assert!(!outbox.finish(&json!(2), false, Ok(json!("too late"))));
        let lines = sink.lines();
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert_eq!(lines[0]["id"], 2);
        assert_eq!(lines[0]["result"]["isError"], true);
        assert_eq!(
            lines[0]["result"]["content"][0]["text"],
            "seek: the player did not answer"
        );
        assert_eq!(lines[1]["id"], 1);
        assert_eq!(outbox.owed_count(), 0);
    }

    #[test]
    fn serve_reads_lines_until_the_end_of_input() {
        let sink = Sink::default();
        let outbox = Outbox::new(Box::new(sink.clone()));
        let mut p = Protocol::new(tools());
        let input = concat!(
            "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{\"protocolVersion\":\"2025-11-25\"}}\r\n",
            "\n",
            "{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n",
            "not json\n",
            "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/call\",\"params\":{\"name\":\"play\"}}",
        );
        let mut calls = Vec::new();
        serve(input.as_bytes(), &mut p, &outbox, |a| calls.push(a));
        let lines = sink.lines();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0]["result"]["protocolVersion"], "2025-11-25");
        assert_eq!(lines[1]["error"]["code"], PARSE_ERROR);
        assert_eq!(calls.len(), 1, "the last line counts without a newline");
    }
}
