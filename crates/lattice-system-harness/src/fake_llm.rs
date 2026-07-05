// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Localhost OpenAI-compatible fake LLM for harness scenarios.
//!
//! A minimal HTTP/SSE server bound to a scenario-local port. The product
//! connects through the formal `OPENAI_COMPATIBLE_BASE_URL` /
//! `OPENAI_COMPATIBLE_MODEL` / `OPENAI_COMPATIBLE_API_KEY` configuration —
//! there is no test-only provider, hidden flag, or product-side change.
//!
//! The server does two things (RFC #19 decision four):
//!
//! 1. Captures the real Chat Completions request (model, messages, tool names)
//!    so scenarios can assert on what the product actually sent.
//! 2. Returns a deterministic SSE script chosen by the scenario: plain text,
//!    tool call, mid-stream disconnect, or illegal response.
//!
//! SSE framing is hand-written over `std::net::TcpListener`. The product's
//! SSE framer (`crates/lattice-llm/src/sse.rs`) accepts both `\n` and `\r\n`
//! line endings and treats `data: [DONE]\n\n` as a no-op data event, so the
//! fake server mirrors the exact shape the real OpenAI Chat Completions
//! stream produces: one `data: {chunk}\n\n` per chunk, terminated by
//! `data: [DONE]\n\n`.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

/// Default test API key. The product requires `OPENAI_COMPATIBLE_API_KEY` to
/// be set and non-empty (`lattice-llm/src/client.rs`), and the auth header is
/// `Bearer <key>`. Scenarios put this sentinel into the env allow-list; it is
/// also the value redaction scanners strip from artifacts.
pub const FAKE_API_KEY: &str = "harness-fake-key-do-not-ship";

/// A captured Chat Completions request body, decoded for scenario assertions.
///
/// Only the fields the harness asserts on are typed; the rest stay in the
/// generic `raw` JSON value so unknown provider extensions do not break
/// parsing.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct CapturedRequest {
    pub model: String,
    /// `role` → count of messages with that role, in arrival order.
    pub message_roles: Vec<String>,
    /// Tool function names as sent on the wire (already `.__`-encoded by the
    /// provider adapter, e.g. `observe__workspace`). Asserted verbatim so a
    /// catalog rename surfaces.
    pub tool_names: Vec<String>,
    /// The full decoded JSON body, for assertions on fields this struct does
    /// not surface (e.g. `temperature`, `max_tokens`).
    pub raw: Value,
}

/// The kind of SSE script the fake server returns for a request.
#[derive(Debug, Clone)]
pub enum SseScript {
    /// Plain assistant text. Emits one or more `delta.content` chunks then a
    /// terminal chunk with `finish_reason: "stop"` and `usage`, then
    /// `data: [DONE]`.
    PlainText {
        chunks: Vec<String>,
        finish_reason: String,
    },
    /// A single tool call. Emits a `tool_calls` start chunk (id + name), an
    /// arguments delta, then a terminal chunk with
    /// `finish_reason: "tool_calls"`.
    ToolCall {
        call_id: String,
        tool_name: String,
        arguments_json: String,
    },
    /// Drop the connection mid-stream after emitting `chunks_before_drop`
    /// content chunks, with no terminal chunk and no `[DONE]`. Simulates a
    /// provider disconnect / aborted stream.
    Disconnect { chunks_before_drop: Vec<String> },
    /// Return a non-SSE HTTP 200 body that is not valid JSON per the OpenAI
    /// chunk schema (e.g. a plain string), exercising the parser's error
    /// path. The body still uses `text/event-stream` so the framer runs.
    IllegalResponse { body: String },
}

impl SseScript {
    /// Convenience: a single-token plain-text turn that finishes with `stop`.
    pub fn plain(text: impl Into<String>) -> Self {
        SseScript::PlainText {
            chunks: vec![text.into()],
            finish_reason: "stop".to_string(),
        }
    }
}

/// Internal error surfaced when the server cannot serve a request. The server
/// thread logs these to stderr and closes the connection; they are not part
/// of the public harness result type because a serving error manifests to the
/// scenario as a product-side LLM failure (which is what the scenario asserts
/// on).
#[derive(Debug, Error)]
pub enum ServeError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("bad request: {0}")]
    BadRequest(String),
}

/// A running fake LLM server bound to a scenario-local port.
///
/// Drop stops accepting new connections; in-flight requests run to
/// completion. The captured requests are readable from any thread via
/// `captured_requests` until the server is dropped.
pub struct FakeLlmServer {
    listener: TcpListener,
    script: Arc<Mutex<SseScript>>,
    captures: Arc<Mutex<Vec<CapturedRequest>>>,
    _handle: Option<JoinHandle<()>>,
    shutdown: Arc<Mutex<bool>>,
}

impl FakeLlmServer {
    /// Bind a new server to `127.0.0.1:0` (OS-assigned port) and start
    /// serving `script`. The bind address is the value to put into
    /// `OPENAI_COMPATIBLE_BASE_URL`.
    pub fn start(script: SseScript) -> Result<Self, ServeError> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        listener.set_nonblocking(false)?;
        let script = Arc::new(Mutex::new(script));
        let captures: Arc<Mutex<Vec<CapturedRequest>>> = Arc::new(Mutex::new(Vec::new()));
        let shutdown = Arc::new(Mutex::new(false));
        let handle = {
            let listener_clone = listener.try_clone()?;
            let script = Arc::clone(&script);
            let captures = Arc::clone(&captures);
            let shutdown = Arc::clone(&shutdown);
            thread::spawn(move || serve_loop(listener_clone, script, captures, shutdown))
        };
        Ok(Self {
            listener,
            script,
            captures,
            _handle: Some(handle),
            shutdown,
        })
    }

    /// The base URL the product should connect to
    /// (`OPENAI_COMPATIBLE_BASE_URL`), without a trailing slash.
    pub fn base_url(&self) -> String {
        format!("http://{}", self.local_addr())
    }

    /// Replace the active script. In-flight requests continue with the script
    /// they started under; only subsequent requests see the new one.
    pub fn set_script(&self, script: SseScript) {
        *self.script.lock().unwrap() = script;
    }

    /// A snapshot of every request captured so far, in arrival order.
    pub fn captured_requests(&self) -> Vec<CapturedRequest> {
        self.captures.lock().unwrap().clone()
    }

    fn local_addr(&self) -> std::net::SocketAddr {
        self.listener.local_addr().expect("listener bound")
    }
}

impl Drop for FakeLlmServer {
    fn drop(&mut self) {
        *self.shutdown.lock().unwrap() = true;
        let _ = std::net::TcpListener::bind(("127.0.0.1", 0));
    }
}

fn serve_loop(
    listener: TcpListener,
    script: Arc<Mutex<SseScript>>,
    captures: Arc<Mutex<Vec<CapturedRequest>>>,
    shutdown: Arc<Mutex<bool>>,
) {
    while !*shutdown.lock().unwrap() {
        match listener.accept() {
            Ok((stream, _)) => {
                let script = Arc::clone(&script);
                let captures = Arc::clone(&captures);
                thread::spawn(move || {
                    if let Err(e) = handle_request(stream, &script, &captures) {
                        eprintln!("fake_llm: serve error: {e}");
                    }
                });
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            Err(e) => {
                eprintln!("fake_llm: accept error: {e}");
                break;
            }
        }
    }
}

fn handle_request(
    mut stream: TcpStream,
    script: &Mutex<SseScript>,
    captures: &Mutex<Vec<CapturedRequest>>,
) -> Result<(), ServeError> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut request_line = String::new();
    reader.read_line(&mut request_line)?;
    if request_line.is_empty() {
        return Err(ServeError::BadRequest("empty request line".into()));
    }
    let mut headers: HashMap<String, String> = HashMap::new();
    loop {
        let mut line = String::new();
        let n = reader.read_line(&mut line)?;
        if n == 0 || line.trim().is_empty() {
            break;
        }
        if let Some((k, v)) = line.split_once(':') {
            headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
        }
    }
    let body = if let Some(len) = headers
        .get("content-length")
        .and_then(|v| v.parse::<usize>().ok())
    {
        let mut buf = vec![0u8; len];
        reader.read_exact(&mut buf)?;
        String::from_utf8_lossy(&buf).to_string()
    } else {
        String::new()
    };

    let captured = parse_chat_completions_body(&body);
    captures.lock().unwrap().push(captured);

    let script = script.lock().unwrap().clone();
    let sse = render_script(&script);
    let response = format!(
        "HTTP/1.1 200 OK\r\n\
         Content-Type: text/event-stream\r\n\
         Cache-Control: no-cache\r\n\
         Connection: close\r\n\
         \r\n\
         {sse}"
    );
    stream.write_all(response.as_bytes())?;
    stream.flush()?;
    Ok(())
}

/// Decode a Chat Completions request body into a [`CapturedRequest`]. A
/// malformed body still records a capture with empty fields and the raw
/// string stored under `raw["__raw_body"]` so a scenario can see what the
/// product sent even if it was not valid JSON.
fn parse_chat_completions_body(body: &str) -> CapturedRequest {
    let raw: Value = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(_) => {
            let mut map = serde_json::Map::new();
            map.insert("__raw_body".into(), Value::String(body.to_string()));
            return CapturedRequest {
                raw: Value::Object(map),
                ..Default::default()
            };
        }
    };
    let model = raw
        .get("model")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let mut message_roles = Vec::new();
    if let Some(messages) = raw.get("messages").and_then(|m| m.as_array()) {
        for msg in messages {
            if let Some(role) = msg.get("role").and_then(|r| r.as_str()) {
                message_roles.push(role.to_string());
            }
        }
    }
    let mut tool_names = Vec::new();
    if let Some(tools) = raw.get("tools").and_then(|t| t.as_array()) {
        for tool in tools {
            if let Some(name) = tool
                .get("function")
                .and_then(|f| f.get("name"))
                .and_then(|n| n.as_str())
            {
                tool_names.push(name.to_string());
            }
        }
    }
    CapturedRequest {
        model,
        message_roles,
        tool_names,
        raw,
    }
}

/// Render an [`SseScript`] into the raw SSE text the server writes after the
/// HTTP headers. Each chunk is `data: {json}\n\n`; the stream ends with
/// `data: [DONE]\n\n` for the in-band terminal cases. The chunk JSON mirrors
/// the OpenAI Chat Completions streaming shape the product's
/// `OpenAiSseParser` expects (`choices[0].delta.content`,
/// `choices[0].delta.tool_calls`, `choices[0].finish_reason`, `usage`).
fn render_script(script: &SseScript) -> String {
    let mut out = String::new();
    match script {
        SseScript::PlainText {
            chunks,
            finish_reason,
        } => {
            for c in chunks {
                let chunk = serde_json::json!({
                    "id": "chatcmpl-fake",
                    "object": "chat.completion.chunk",
                    "choices": [
                        {"index": 0, "delta": {"content": c}, "finish_reason": null}
                    ]
                });
                push_data(&mut out, &chunk);
            }
            let done = serde_json::json!({
                "id": "chatcmpl-fake",
                "object": "chat.completion.chunk",
                "choices": [
                    {"index": 0, "delta": {}, "finish_reason": finish_reason}
                ],
                "usage": {
                    "prompt_tokens": 1,
                    "completion_tokens": chunks.len() as u64,
                    "total_tokens": 1 + chunks.len() as u64
                }
            });
            push_data(&mut out, &done);
            out.push_str("data: [DONE]\n\n");
        }
        SseScript::ToolCall {
            call_id,
            tool_name,
            arguments_json,
        } => {
            let start = serde_json::json!({
                "id": "chatcmpl-fake",
                "object": "chat.completion.chunk",
                "choices": [{
                    "index": 0,
                    "delta": {
                        "tool_calls": [{
                            "index": 0,
                            "id": call_id,
                            "type": "function",
                            "function": {"name": tool_name, "arguments": ""}
                        }]
                    },
                    "finish_reason": null
                }]
            });
            push_data(&mut out, &start);
            let args = serde_json::json!({
                "id": "chatcmpl-fake",
                "object": "chat.completion.chunk",
                "choices": [{
                    "index": 0,
                    "delta": {
                        "tool_calls": [{
                            "index": 0,
                            "function": {"arguments": arguments_json}
                        }]
                    },
                    "finish_reason": null
                }]
            });
            push_data(&mut out, &args);
            let done = serde_json::json!({
                "id": "chatcmpl-fake",
                "object": "chat.completion.chunk",
                "choices": [
                    {"index": 0, "delta": {}, "finish_reason": "tool_calls"}
                ]
            });
            push_data(&mut out, &done);
            out.push_str("data: [DONE]\n\n");
        }
        SseScript::Disconnect { chunks_before_drop } => {
            for c in chunks_before_drop {
                let chunk = serde_json::json!({
                    "id": "chatcmpl-fake",
                    "object": "chat.completion.chunk",
                    "choices": [
                        {"index": 0, "delta": {"content": c}, "finish_reason": null}
                    ]
                });
                push_data(&mut out, &chunk);
            }
            // No terminal chunk, no [DONE] — the connection is simply closed
            // after the in-flight chunks. The product should treat this as a
            // truncated stream.
        }
        SseScript::IllegalResponse { body } => {
            // `data: <body>\n\n` — the framer accepts it as one data event,
            // and the chunk parser rejects it as malformed JSON.
            out.push_str("data: ");
            out.push_str(body);
            out.push_str("\n\n");
            out.push_str("data: [DONE]\n\n");
        }
    }
    out
}

fn push_data(out: &mut String, value: &Value) {
    out.push_str("data: ");
    out.push_str(&value.to_string());
    out.push_str("\n\n");
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Render a plain-text script and verify the exact SSE shape the product
    /// framer expects: one `data: {chunk}\n\n` per content chunk, a terminal
    /// chunk with `finish_reason` and `usage`, then `data: [DONE]\n\n`.
    #[test]
    fn render_plain_text_script_matches_openai_shape() {
        let script = SseScript::PlainText {
            chunks: vec!["Hello".into(), " world".into()],
            finish_reason: "stop".into(),
        };
        let sse = render_script(&script);
        assert!(sse.contains("data: {"));
        assert!(sse.contains("\"finish_reason\":\"stop\""));
        assert!(sse.contains("\"usage\""));
        assert!(sse.contains("\"total_tokens\""));
        assert!(sse.ends_with("data: [DONE]\n\n"));
    }

    /// A tool-call script emits the start chunk (id + name), an arguments
    /// delta, and a terminal chunk with `finish_reason: "tool_calls"`.
    #[test]
    fn render_tool_call_script_emits_start_args_and_tool_calls_finish() {
        let script = SseScript::ToolCall {
            call_id: "call_abc".into(),
            tool_name: "observe__workspace".into(),
            arguments_json: "{}".into(),
        };
        let sse = render_script(&script);
        assert!(sse.contains("\"id\":\"call_abc\""));
        assert!(sse.contains("\"name\":\"observe__workspace\""));
        assert!(sse.contains("\"arguments\":\"{}\""));
        assert!(sse.contains("\"finish_reason\":\"tool_calls\""));
        assert!(sse.ends_with("data: [DONE]\n\n"));
    }

    /// A disconnect script emits the in-flight chunks with no terminal chunk
    /// and no `[DONE]` marker — the connection is simply truncated.
    #[test]
    fn render_disconnect_script_omits_terminal_chunk_and_done() {
        let script = SseScript::Disconnect {
            chunks_before_drop: vec!["partial".into()],
        };
        let sse = render_script(&script);
        assert!(sse.contains("\"content\":\"partial\""));
        assert!(!sse.contains("\"finish_reason\":\""));
        assert!(!sse.contains("[DONE]"));
    }

    /// An illegal-response script emits a non-JSON `data:` payload so the
    /// product chunk parser hits its error path.
    #[test]
    fn render_illegal_response_script_emits_non_json_data() {
        let script = SseScript::IllegalResponse {
            body: "not json at all".into(),
        };
        let sse = render_script(&script);
        assert!(sse.contains("data: not json at all\n\n"));
    }

    /// Decoding a Chat Completions body surfaces model, message roles (in
    /// arrival order), and wire tool names.
    #[test]
    fn parse_body_surfaces_model_roles_and_tool_names() {
        let body = serde_json::json!({
            "model": "compat:test",
            "messages": [
                {"role": "system", "content": "s"},
                {"role": "user", "content": "u"}
            ],
            "tools": [
                {"type": "function", "function": {"name": "observe__workspace"}},
                {"type": "function", "function": {"name": "session__todo_write"}}
            ]
        })
        .to_string();
        let captured = parse_chat_completions_body(&body);
        assert_eq!(captured.model, "compat:test");
        assert_eq!(captured.message_roles, vec!["system", "user"]);
        assert_eq!(
            captured.tool_names,
            vec!["observe__workspace", "session__todo_write"]
        );
    }

    /// A malformed body is still recorded, with the raw text preserved so a
    /// scenario can see what the product sent.
    #[test]
    fn parse_body_records_raw_text_when_not_json() {
        let captured = parse_chat_completions_body("not json");
        assert_eq!(captured.model, "");
        assert!(captured.message_roles.is_empty());
        assert_eq!(
            captured.raw.get("__raw_body").and_then(|v| v.as_str()),
            Some("not json")
        );
    }

    /// The fake server accepts a real HTTP Chat Completions request, captures
    /// it, and returns the script's SSE. This is the end-to-end server
    /// tracer bullet: it proves the bind/accept/parse/serve path works and
    /// that the product's framer-compatible framing is on the wire.
    #[test]
    fn server_captures_request_and_returns_plain_text_sse() {
        let server = FakeLlmServer::start(SseScript::plain("hi")).expect("start server");
        let url = format!("{}/chat/completions", server.base_url());

        let resp = std::net::TcpStream::connect(server.local_addr())
            .and_then(|mut s| {
                let body = serde_json::json!({
                    "model": "compat:test",
                    "messages": [{"role": "user", "content": "ping"}],
                    "stream": true
                })
                .to_string();
                let req = format!(
                    "POST /chat/completions HTTP/1.1\r\n\
                     Host: 127.0.0.1\r\n\
                     Content-Type: application/json\r\n\
                     Content-Length: {}\r\n\
                     \r\n\
                     {body}",
                    body.len()
                );
                s.write_all(req.as_bytes())?;
                s.flush()?;
                let mut buf = Vec::new();
                s.read_to_end(&mut buf)?;
                Ok(buf)
            })
            .expect("connect + read");
        let resp = String::from_utf8_lossy(&resp);

        assert!(resp.starts_with("HTTP/1.1 200 OK"), "got: {resp}");
        assert!(resp.contains("text/event-stream"));
        assert!(resp.contains("data: {"));
        assert!(resp.contains("data: [DONE]"));
        let captured = server.captured_requests();
        assert_eq!(captured.len(), 1);
        assert_eq!(captured[0].model, "compat:test");
        assert_eq!(captured[0].message_roles, vec!["user"]);

        let _ = url;
    }
}
