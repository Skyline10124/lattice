// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Localhost fake LLM for harness scenarios.
//!
//! A minimal HTTP/SSE server bound to a scenario-local port. The product
//! connects through the selected provider's formal base URL and credential
//! configuration (`OPENAI_COMPATIBLE_*`, `ANTHROPIC_BASE_URL`,
//! `GEMINI_BASE_URL`) — there is no test-only provider, hidden flag, or
//! product-side change. The request route selects the streaming protocol:
//! OpenAI Chat Completions, Anthropic Messages, or Gemini
//! `streamGenerateContent`.
//!
//! The server does two things (RFC #41, Codeberg #19, decision four):
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

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

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
    /// Protocol the request used: `openai`, `anthropic` or `gemini`.
    pub provider: String,
    /// The full decoded JSON body, for assertions on fields this struct does
    /// not surface (e.g. `temperature`, `max_tokens`).
    pub raw: Value,
}

/// The kind of SSE script the fake server returns for a request.
#[derive(Debug, Clone)]
pub enum SseScript {
    /// 手工编排的合法 OpenAI chunks，保持跨类型到达顺序；结尾自动添加 DONE。
    Chunks { chunks: Vec<Value> },
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
    /// Multiple ToolCalls in one assistant message, preserving binding order.
    ToolCalls {
        calls: Vec<(String, String, String)>,
    },
    /// Return a non-SSE HTTP response with a non-2xx status and a JSON error
    /// body (no `text/event-stream`). Simulates a provider startup failure
    /// (e.g. 429/500) so the product's retry/error path runs.
    HttpError { status: u16, body: String },
    /// Drop the connection mid-stream after emitting `chunks_before_drop`
    /// provider disconnect / aborted stream.
    Disconnect { chunks_before_drop: Vec<String> },
    /// Return a non-SSE HTTP 200 body that is not valid JSON per the OpenAI
    /// chunk schema (e.g. a plain string), exercising the parser's error
    /// path. The body still uses `text/event-stream` so the framer runs.
    IllegalResponse { body: String },
    /// Serve each inner script to exactly one request, in order. Requests
    /// beyond the sequence length repeat the last script. This is the
    /// multi-round foundation for Direct-Local tool scenarios (skeletons in
    /// #62 (Codeberg #40), expanded in #46 (Codeberg #24)): first request ->
    /// tool call, second
    /// request -> plain text, so a turn that uses a Tool still terminates
    /// cleanly.
    Sequence { scripts: Vec<SseScript> },
    /// 按请求轮转内层脚本，走完最后一个后回到第一个。`Sequence` 会
    /// 收敛到最后一个脚本，`Cycle` 则对后续每个请求重复整套模式，因此
    /// 同一个 TUI 会话里的每一轮 turn 都能重演相同的失败/成功形状。
    Cycle { scripts: Vec<SseScript> },
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

/// A bounded fake-provider protocol or lifecycle failure.
#[derive(Debug, Error)]
pub enum ServeError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("timed out after {timeout:?} waiting for {operation}")]
    Timeout {
        operation: &'static str,
        timeout: Duration,
    },
    #[error("not a model request: {0}")]
    UnexpectedRequest(String),
    #[error("script cannot be rendered in the {0} protocol")]
    UnsupportedScript(&'static str),
    #[error("fake-provider server thread failed: {0}")]
    ThreadFailure(String),
}

fn timeout_error(operation: &'static str, duration: Duration) -> ServeError {
    ServeError::Timeout {
        operation,
        timeout: duration,
    }
}

/// A running fake LLM server bound to a scenario-local port.
///
/// Explicit shutdown reports server failures and waits only to its deadline.
/// `Drop` uses a fixed bounded fallback, cancels any response gate, and closes
/// the listener. Captures remain readable until drop.
#[derive(Clone)]
pub struct ResponseGate {
    state: Arc<(Mutex<GateState>, Condvar)>,
    /// 分段交付共用一个场景期限；普通整份响应 gate 保持原生命周期。
    segment_deadline: Option<Instant>,
}

#[derive(Default)]
struct GateState {
    released_through: usize,
    cancelled: bool,
    stalled: usize,
    departed: usize,
}

impl ResponseGate {
    pub fn wait_until_stalled(&self, count: usize, timeout: Duration) -> Result<(), ServeError> {
        let deadline = Instant::now() + timeout;
        let (lock, wake) = &*self.state;
        let mut state = lock.lock().unwrap();
        while state.stalled < count {
            if state.cancelled {
                return Err(ServeError::ThreadFailure(
                    "response gate was cancelled before the requested stall".into(),
                ));
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(timeout_error("fake-provider response stall", timeout));
            }
            let (next, result) = wake.wait_timeout(state, remaining).unwrap();
            state = next;
            if result.timed_out() && state.stalled < count {
                return Err(timeout_error("fake-provider response stall", timeout));
            }
        }
        Ok(())
    }

    pub fn release(&self, timeout: Duration) -> Result<(), ServeError> {
        self.release_through(true, timeout)
    }

    /// 只放行下一帧 SSE，并等待处理线程越过该 gate；不会顺带放行终态。
    pub fn release_next(&self, timeout: Duration) -> Result<(), ServeError> {
        self.release_through(false, timeout)
    }

    fn release_through(&self, all: bool, timeout: Duration) -> Result<(), ServeError> {
        let deadline = Instant::now() + timeout;
        let (lock, wake) = &*self.state;
        let mut state = lock.lock().unwrap();
        let target = if all {
            state.stalled
        } else {
            state.departed + 1
        };
        state.released_through = if all { usize::MAX } else { target };
        wake.notify_all();
        while state.departed < target {
            if state.cancelled {
                return Err(ServeError::ThreadFailure("response gate cancelled".into()));
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(timeout_error("fake-provider response release", timeout));
            }
            let (next, result) = wake.wait_timeout(state, remaining).unwrap();
            state = next;
            if result.timed_out() && state.departed < target {
                return Err(timeout_error("fake-provider response release", timeout));
            }
        }
        Ok(())
    }

    fn cancel(&self) {
        let (lock, wake) = &*self.state;
        lock.lock().unwrap().cancelled = true;
        wake.notify_all();
    }

    fn wait(&self) -> Result<bool, ServeError> {
        let (lock, wake) = &*self.state;
        let mut state = lock.lock().unwrap();
        state.stalled += 1;
        let ticket = state.stalled;
        wake.notify_all();
        while state.released_through < ticket && !state.cancelled {
            state = if let Some(deadline) = self.segment_deadline {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    state.cancelled = true;
                    wake.notify_all();
                    return Err(ServeError::ThreadFailure(
                        "fake-provider segment delivery deadline elapsed".into(),
                    ));
                }
                wake.wait_timeout(state, remaining).unwrap().0
            } else {
                wake.wait(state).unwrap()
            };
        }
        let released = !state.cancelled;
        state.departed += 1;
        wake.notify_all();
        Ok(released)
    }
}

struct ServerState {
    script: SseScript,
    next: usize,
    requests: Vec<CapturedRequest>,
    shutdown_requested: bool,
    serve_exited: bool,
    active_handlers: usize,
    first_failure: Option<String>,
    expected_api_key: Option<String>,
}

impl ServerState {
    fn record_failure(&mut self, failure: impl Into<String>) {
        if self.first_failure.is_none() {
            self.first_failure = Some(failure.into());
        }
    }
}

pub struct FakeLlmServer {
    listener: Option<TcpListener>,
    addr: std::net::SocketAddr,
    state: Arc<(Mutex<ServerState>, Condvar)>,
    serve_handle: Option<JoinHandle<()>>,
    handlers: Arc<Mutex<Vec<JoinHandle<()>>>>,
    gate: Option<ResponseGate>,
}

impl FakeLlmServer {
    /// Bind a new server to `127.0.0.1:0` (OS-assigned port) and start
    /// serving `script`. The bind address is the value to put into
    /// `OPENAI_COMPATIBLE_BASE_URL`.
    ///
    /// Shutdown wakes the blocking listener with one loopback connection;
    /// request, gate, and thread completion waits all have explicit bounds.
    pub fn start(script: SseScript) -> Result<Self, ServeError> {
        Self::start_inner(script, None, None)
    }

    /// Start with a crash-surviving append-only request counter.
    pub fn start_logged(script: SseScript, path: &Path) -> Result<Self, ServeError> {
        Self::start_inner(script, None, Some(Arc::new(path.to_path_buf())))
    }

    pub fn start_gated(script: SseScript) -> Result<(Self, ResponseGate), ServeError> {
        Self::start_controlled(script, None)
    }

    /// 每帧 SSE 单独放行；全部交付受同一绝对期限约束，shutdown/Drop 仍取消 gate。
    pub fn start_segmented(
        script: SseScript,
        timeout: Duration,
    ) -> Result<(Self, ResponseGate), ServeError> {
        Self::start_controlled(script, Some(Instant::now() + timeout))
    }

    fn start_controlled(
        script: SseScript,
        segment_deadline: Option<Instant>,
    ) -> Result<(Self, ResponseGate), ServeError> {
        let gate = ResponseGate {
            state: Arc::new((Mutex::new(GateState::default()), Condvar::new())),
            segment_deadline,
        };
        let server = Self::start_inner(script, Some(gate.clone()), None)?;
        Ok((server, gate))
    }

    fn start_inner(
        script: SseScript,
        gate: Option<ResponseGate>,
        request_log: Option<Arc<PathBuf>>,
    ) -> Result<Self, ServeError> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let addr = listener.local_addr()?;
        let state = Arc::new((
            Mutex::new(ServerState {
                script,
                next: 0,
                requests: Vec::new(),
                shutdown_requested: false,
                serve_exited: false,
                active_handlers: 0,
                first_failure: None,
                expected_api_key: None,
            }),
            Condvar::new(),
        ));
        let handlers = Arc::new(Mutex::new(Vec::new()));
        let handle = {
            let listener_clone = listener.try_clone()?;
            let thread_state = Arc::clone(&state);
            let handlers = Arc::clone(&handlers);
            let handler_gate = gate.clone();
            let request_log_for_thread = request_log.clone();
            thread::Builder::new()
                .name("fake-llm-server".into())
                .spawn(move || {
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        serve_loop(
                            listener_clone,
                            Arc::clone(&thread_state),
                            handlers,
                            handler_gate,
                            request_log_for_thread,
                        )
                    }));
                    let (lock, wake) = &*thread_state;
                    let mut state = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                    if result.is_err() {
                        state.record_failure("serve thread panicked");
                    }
                    state.serve_exited = true;
                    wake.notify_all();
                })?
        };
        Ok(Self {
            listener: Some(listener),
            addr,
            state,
            serve_handle: Some(handle),
            handlers,
            gate,
        })
    }

    /// The base URL the product should connect to
    /// (`OPENAI_COMPATIBLE_BASE_URL`), without a trailing slash.
    pub fn base_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.local_addr().port())
    }

    /// 仅在首个请求前设置测试凭据；只比较内存字节，不写 capture 或错误正文。
    pub fn expect_api_key(&self, key: &str) {
        let mut state = self.state.0.lock().unwrap();
        assert!(state.requests.is_empty() && state.active_handlers == 0);
        state.expected_api_key = Some(key.to_string());
    }

    /// A snapshot of every request captured so far, in arrival order.
    pub fn captured_requests(&self) -> Vec<CapturedRequest> {
        self.state.0.lock().unwrap().requests.clone()
    }

    pub fn wait_for_requests(&self, count: usize, timeout: Duration) -> Result<(), ServeError> {
        let deadline = Instant::now() + timeout;
        let (lock, wake) = &*self.state;
        let mut state = lock.lock().unwrap();
        while state.requests.len() < count {
            if let Some(failure) = &state.first_failure {
                return Err(ServeError::ThreadFailure(failure.clone()));
            }
            if state.serve_exited {
                return Err(ServeError::ThreadFailure(
                    "serve thread exited before the requested capture".into(),
                ));
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(timeout_error("fake-provider request", timeout));
            }
            let (next, result) = wake.wait_timeout(state, remaining).unwrap();
            state = next;
            if result.timed_out() && state.requests.len() < count {
                return Err(timeout_error("fake-provider request", timeout));
            }
        }
        Ok(())
    }

    pub fn shutdown(&mut self, timeout: Duration) -> Result<(), ServeError> {
        if self.listener.is_none() {
            return match self.state.0.lock().unwrap().first_failure.clone() {
                Some(failure) => Err(ServeError::ThreadFailure(failure)),
                None => Ok(()),
            };
        }
        let deadline = Instant::now() + timeout;
        {
            let (lock, wake) = &*self.state;
            lock.lock().unwrap().shutdown_requested = true;
            wake.notify_all();
        }
        if let Some(gate) = &self.gate {
            gate.cancel();
        }
        if !timeout.is_zero() {
            let _ = TcpStream::connect_timeout(&self.addr, timeout.min(Duration::from_millis(100)));
        }

        {
            let (lock, wake) = &*self.state;
            let mut state = lock.lock().unwrap();
            while !state.serve_exited || state.active_handlers != 0 {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    return Err(timeout_error("fake-provider shutdown", timeout));
                }
                let (next, result) = wake.wait_timeout(state, remaining).unwrap();
                state = next;
                if result.timed_out() && (!state.serve_exited || state.active_handlers != 0) {
                    return Err(timeout_error("fake-provider shutdown", timeout));
                }
            }
        }

        if let Some(handle) = self.serve_handle.take()
            && handle.join().is_err()
        {
            self.state
                .0
                .lock()
                .unwrap()
                .record_failure("serve thread join failed");
        }
        for handle in self.handlers.lock().unwrap().drain(..) {
            if handle.join().is_err() {
                self.state
                    .0
                    .lock()
                    .unwrap()
                    .record_failure("handler thread join failed");
            }
        }
        self.listener.take();
        match self.state.0.lock().unwrap().first_failure.clone() {
            Some(failure) => Err(ServeError::ThreadFailure(failure)),
            None => Ok(()),
        }
    }

    fn local_addr(&self) -> std::net::SocketAddr {
        self.addr
    }
}

impl Drop for FakeLlmServer {
    fn drop(&mut self) {
        // Drop cannot return lifecycle errors. Explicit `shutdown` retains the
        // first fixture failure; this fallback only guarantees a bounded wait.
        let _ = self.shutdown(Duration::from_secs(3));
    }
}

fn serve_loop(
    listener: TcpListener,
    state: Arc<(Mutex<ServerState>, Condvar)>,
    handlers: Arc<Mutex<Vec<JoinHandle<()>>>>,
    gate: Option<ResponseGate>,
    request_log: Option<Arc<PathBuf>>,
) {
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                {
                    let (lock, _) = &*state;
                    let mut server = lock.lock().unwrap();
                    if server.shutdown_requested {
                        break;
                    }
                    server.active_handlers += 1;
                }
                let handler_state = Arc::clone(&state);
                let gate = gate.clone();
                let request_log = request_log.clone();
                let spawn = thread::Builder::new()
                    .name("fake-llm-handler".into())
                    .spawn(move || {
                        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            handle_request(
                                stream,
                                &handler_state,
                                gate.as_ref(),
                                request_log.as_deref().map(PathBuf::as_path),
                            )
                        }));
                        let (lock, wake) = &*handler_state;
                        let mut server =
                            lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                        match result {
                            Ok(Ok(())) => {}
                            Ok(Err(error)) => server.record_failure(error.to_string()),
                            Err(_) => server.record_failure("handler thread panicked"),
                        }
                        server.active_handlers -= 1;
                        wake.notify_all();
                    });
                match spawn {
                    Ok(handle) => handlers.lock().unwrap().push(handle),
                    Err(error) => {
                        let (lock, wake) = &*state;
                        let mut server = lock.lock().unwrap();
                        server.active_handlers -= 1;
                        server.record_failure(format!("failed to spawn handler thread: {error}"));
                        wake.notify_all();
                    }
                }
            }
            Err(error) => {
                let (lock, wake) = &*state;
                lock.lock()
                    .unwrap()
                    .record_failure(format!("accept failed: {error}"));
                wake.notify_all();
                break;
            }
        }
    }
}

fn append_fsynced(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

fn write_http_response(
    stream: &mut TcpStream,
    status: &str,
    content_type: &str,
    body: &[u8],
) -> std::io::Result<()> {
    write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )?;
    stream.write_all(body)?;
    stream.flush()?;
    stream.shutdown(Shutdown::Write)
}

/// One request from the product under test. The product is the fake provider's
/// only client, so parsing extracts just the route, credential and body; it
/// does not defend against malformed or hostile HTTP.
struct HttpRequest {
    method: String,
    path: String,
    /// `authorization`, `x-api-key` and `x-goog-api-key`, names lowercased.
    credentials: Vec<(String, String)>,
    body: String,
}

/// `Ok(None)` when the peer closed before sending a request line.
fn read_http_request(stream: &mut TcpStream) -> std::io::Result<Option<HttpRequest>> {
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    if reader.read_line(&mut line)? == 0 {
        return Ok(None);
    }
    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_string();
    let path = parts.next().unwrap_or_default().to_string();
    let (mut length, mut credentials) = (0, Vec::new());
    loop {
        line.clear();
        if reader.read_line(&mut line)? == 0 || line.trim().is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            let value = value.trim();
            if name.eq_ignore_ascii_case("content-length") {
                length = value.parse().unwrap_or(0);
            } else {
                let name = name.to_ascii_lowercase();
                if matches!(
                    name.as_str(),
                    "authorization" | "x-api-key" | "x-goog-api-key"
                ) {
                    credentials.push((name, value.to_string()));
                }
            }
        }
    }
    let mut body = vec![0; length];
    reader.read_exact(&mut body)?;
    Ok(Some(HttpRequest {
        method,
        path,
        credentials,
        body: String::from_utf8_lossy(&body).into_owned(),
    }))
}

/// Streaming protocol of one model request, chosen by its route.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Protocol {
    OpenAi,
    Anthropic,
    Gemini,
}

impl Protocol {
    /// The protocol and the model named in the path, for a model route.
    fn route(method: &str, path: &str) -> Option<(Self, Option<String>)> {
        if method != "POST" {
            return None;
        }
        match path {
            "/chat/completions" => Some((Self::OpenAi, None)),
            "/v1/messages" => Some((Self::Anthropic, None)),
            _ => {
                let model = path
                    .strip_prefix("/models/")?
                    .strip_suffix(":streamGenerateContent?alt=sse")?;
                Some((Self::Gemini, Some(model.to_string())))
            }
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::OpenAi => "openai",
            Self::Anthropic => "anthropic",
            Self::Gemini => "gemini",
        }
    }

    /// The API key in this protocol's credential header.
    fn api_key(self, request: &HttpRequest) -> Option<&str> {
        let (header, prefix) = match self {
            Self::OpenAi => ("authorization", "Bearer "),
            Self::Anthropic => ("x-api-key", ""),
            Self::Gemini => ("x-goog-api-key", ""),
        };
        request
            .credentials
            .iter()
            .find(|(name, _)| name == header)
            .and_then(|(_, value)| value.strip_prefix(prefix))
    }
}

fn handle_request(
    mut stream: TcpStream,
    shared: &(Mutex<ServerState>, Condvar),
    gate: Option<&ResponseGate>,
    request_log: Option<&Path>,
) -> Result<(), ServeError> {
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    let expected = shared.0.lock().unwrap().expected_api_key.clone();
    let Some(request) = read_http_request(&mut stream)? else {
        return Ok(());
    };
    let Some((protocol, path_model)) = Protocol::route(&request.method, &request.path) else {
        write_http_response(
            &mut stream,
            "404 Not Found",
            "text/plain",
            b"not a model request\n",
        )?;
        return Err(ServeError::UnexpectedRequest(format!(
            "{} {}",
            request.method, request.path
        )));
    };
    if expected
        .as_deref()
        .is_some_and(|expected| protocol.api_key(&request) != Some(expected))
    {
        return write_http_response(
            &mut stream,
            "401 Unauthorized",
            "text/plain",
            b"unauthorized\n",
        )
        .map_err(ServeError::Io);
    }

    let captured = parse_request_body(protocol, path_model, &request.body);
    // Script assignment and capture publication are one transaction. Capture
    // order is therefore the same order used to advance a Sequence, even if
    // gated handlers resume in a different order.
    let active = {
        let (lock, wake) = shared;
        let mut state = lock.lock().unwrap();
        let (active, advance_sequence) = match &state.script {
            SseScript::Sequence { scripts } => {
                if scripts.is_empty() {
                    (
                        SseScript::IllegalResponse {
                            body: "fake_llm: empty Sequence script".into(),
                        },
                        false,
                    )
                } else {
                    let idx = state.next.min(scripts.len() - 1);
                    (scripts[idx].clone(), true)
                }
            }
            SseScript::Cycle { scripts } => {
                if scripts.is_empty() {
                    (
                        SseScript::IllegalResponse {
                            body: "fake_llm: empty Cycle script".into(),
                        },
                        false,
                    )
                } else {
                    let idx = state.next % scripts.len();
                    (scripts[idx].clone(), true)
                }
            }
            other => (other.clone(), false),
        };
        state.next += usize::from(advance_sequence);
        state.requests.push(captured);
        wake.notify_all();
        active
    };
    if let Some(path) = request_log {
        append_fsynced(path, b"request\n")?;
    }
    let segmented = gate.is_some_and(|gate| gate.segment_deadline.is_some());
    if !segmented
        && let Some(gate) = gate
        && !gate.wait()?
    {
        return Ok(());
    }
    // HttpError short-circuits with a non-SSE HTTP error response.
    if let SseScript::HttpError { status, body } = &active {
        return write_http_response(
            &mut stream,
            &format!("{status} Error"),
            "application/json",
            body.as_bytes(),
        )
        .map_err(ServeError::Io);
    }
    let Some(sse) = render(protocol, &active) else {
        write_http_response(
            &mut stream,
            "500 Internal Server Error",
            "text/plain",
            b"script not supported for this protocol\n",
        )?;
        return Err(ServeError::UnsupportedScript(protocol.name()));
    };
    if !segmented {
        // 原整份响应路径保持一次写入；分段控制不改变其他场景的交付方式。
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
        return Ok(());
    }
    write!(
        stream,
        "HTTP/1.1 200 OK\r\n\
         Content-Type: text/event-stream\r\n\
         Cache-Control: no-cache\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\r\n",
        sse.len()
    )?;
    for frame in sse.split_inclusive("\n\n") {
        if let Some(gate) = gate
            && !gate.wait()?
        {
            return Ok(());
        }
        stream.write_all(frame.as_bytes())?;
        stream.flush()?;
    }
    Ok(())
}

/// Decode a model request body into a [`CapturedRequest`]. A malformed body
/// still records a capture with empty fields and the raw string stored under
/// `raw["__raw_body"]` so a scenario can see what the product sent even if it
/// was not valid JSON.
fn parse_request_body(
    protocol: Protocol,
    path_model: Option<String>,
    body: &str,
) -> CapturedRequest {
    let provider = protocol.name().to_string();
    let raw: Value = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(_) => {
            let mut map = serde_json::Map::new();
            map.insert("__raw_body".into(), Value::String(body.to_string()));
            return CapturedRequest {
                raw: Value::Object(map),
                provider,
                ..Default::default()
            };
        }
    };
    let model = path_model.unwrap_or_else(|| raw["model"].as_str().unwrap_or("").to_string());
    let messages = match protocol {
        Protocol::Gemini => &raw["contents"],
        _ => &raw["messages"],
    };
    let message_roles = messages
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|message| message["role"].as_str())
        .map(str::to_string)
        .collect();
    let tools = raw["tools"].as_array().into_iter().flatten();
    let tool_names = match protocol {
        Protocol::OpenAi => tools
            .filter_map(|tool| tool["function"]["name"].as_str())
            .map(str::to_string)
            .collect(),
        Protocol::Anthropic => tools
            .filter_map(|tool| tool["name"].as_str())
            .map(str::to_string)
            .collect(),
        Protocol::Gemini => tools
            .flat_map(|tool| {
                tool["functionDeclarations"]
                    .as_array()
                    .into_iter()
                    .flatten()
            })
            .filter_map(|declaration| declaration["name"].as_str())
            .map(str::to_string)
            .collect(),
    };
    CapturedRequest {
        model,
        message_roles,
        tool_names,
        provider,
        raw,
    }
}

/// `(call id, Tool name, arguments JSON)` of one scripted Tool call.
type ScriptedCall = (String, String, String);

/// Render `script` in `protocol`. Anthropic and Gemini render the text and
/// Tool call scripts; the other scripts exist only in the OpenAI protocol.
fn render(protocol: Protocol, script: &SseScript) -> Option<String> {
    if protocol == Protocol::OpenAi {
        return Some(render_script(script));
    }
    let (texts, calls, finish): (&[String], Vec<ScriptedCall>, &str) = match script {
        SseScript::PlainText {
            chunks,
            finish_reason,
        } => (chunks, Vec::new(), finish_reason),
        SseScript::ToolCall {
            call_id,
            tool_name,
            arguments_json,
        } => (
            &[],
            vec![(call_id.clone(), tool_name.clone(), arguments_json.clone())],
            "tool_calls",
        ),
        SseScript::ToolCalls { calls } => (&[], calls.clone(), "tool_calls"),
        _ => return None,
    };
    Some(match protocol {
        Protocol::Anthropic => render_anthropic(texts, &calls, finish),
        _ => render_gemini(texts, &calls, finish),
    })
}

/// Anthropic Messages stream: `message_start`, one text block, one
/// `tool_use` block per call, `message_delta` with the stop reason.
fn render_anthropic(texts: &[String], calls: &[ScriptedCall], finish: &str) -> String {
    fn event(out: &mut String, name: &str, data: Value) {
        out.push_str(&format!("event: {name}\ndata: {data}\n\n"));
    }
    let mut out = String::new();
    event(
        &mut out,
        "message_start",
        serde_json::json!({"type": "message_start", "message": {"usage": {"input_tokens": 1}}}),
    );
    let mut index = 0;
    if !texts.is_empty() {
        event(
            &mut out,
            "content_block_start",
            serde_json::json!({"type": "content_block_start", "index": index,
                "content_block": {"type": "text", "text": ""}}),
        );
        for text in texts {
            event(
                &mut out,
                "content_block_delta",
                serde_json::json!({"type": "content_block_delta", "index": index,
                    "delta": {"type": "text_delta", "text": text}}),
            );
        }
        event(
            &mut out,
            "content_block_stop",
            serde_json::json!({"type": "content_block_stop", "index": index}),
        );
        index += 1;
    }
    for (id, name, arguments) in calls {
        event(
            &mut out,
            "content_block_start",
            serde_json::json!({"type": "content_block_start", "index": index,
                "content_block": {"type": "tool_use", "id": id, "name": name, "input": {}}}),
        );
        event(
            &mut out,
            "content_block_delta",
            serde_json::json!({"type": "content_block_delta", "index": index,
                "delta": {"type": "input_json_delta", "partial_json": arguments}}),
        );
        event(
            &mut out,
            "content_block_stop",
            serde_json::json!({"type": "content_block_stop", "index": index}),
        );
        index += 1;
    }
    let stop_reason = match finish {
        "tool_calls" => "tool_use",
        "length" => "max_tokens",
        "stop" => "end_turn",
        other => other,
    };
    event(
        &mut out,
        "message_delta",
        serde_json::json!({"type": "message_delta", "delta": {"stop_reason": stop_reason},
            "usage": {"output_tokens": texts.len().max(calls.len())}}),
    );
    event(
        &mut out,
        "message_stop",
        serde_json::json!({"type": "message_stop"}),
    );
    out
}

/// Gemini `streamGenerateContent?alt=sse`: one chunk per text part or
/// `functionCall`, then a final chunk with `finishReason` and usage.
fn render_gemini(texts: &[String], calls: &[ScriptedCall], finish: &str) -> String {
    let chunk = |parts: Value| serde_json::json!({"candidates": [{"content": {"role": "model", "parts": parts}}]});
    let mut out = String::new();
    for text in texts {
        push_data(&mut out, &chunk(serde_json::json!([{"text": text}])));
    }
    for (id, name, arguments) in calls {
        let args: Value =
            serde_json::from_str(arguments).unwrap_or_else(|_| Value::String(arguments.clone()));
        push_data(
            &mut out,
            &chunk(serde_json::json!([{"functionCall": {"id": id, "name": name, "args": args}}])),
        );
    }
    let finish_reason = if finish == "length" {
        "MAX_TOKENS"
    } else {
        "STOP"
    };
    let produced = texts.len().max(calls.len());
    push_data(
        &mut out,
        &serde_json::json!({
            "candidates": [{"content": {"role": "model", "parts": []}, "finishReason": finish_reason}],
            "usageMetadata": {"promptTokenCount": 1, "candidatesTokenCount": produced,
                "totalTokenCount": 1 + produced}
        }),
    );
    out
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
        SseScript::Chunks { chunks } => {
            for chunk in chunks {
                out.push_str(&format!("data: {chunk}\n\n"));
            }
            out.push_str("data: [DONE]\n\n");
        }
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
            // 为工具调用响应提供使用量，以验证拒绝后的使用量保留，而非只覆盖缺失值。
            let done = serde_json::json!({
                "id": "chatcmpl-fake",
                "object": "chat.completion.chunk",
                "choices": [
                    {"index": 0, "delta": {}, "finish_reason": "tool_calls"}
                ],
                "usage": {
                    "prompt_tokens": 1,
                    "completion_tokens": 1,
                    "total_tokens": 2
                }
            });
            push_data(&mut out, &done);
            out.push_str("data: [DONE]\n\n");
        }
        SseScript::ToolCalls { calls } => {
            let starts: Vec<Value> = calls
                .iter()
                .enumerate()
                .map(|(index, (id, name, _))| {
                    serde_json::json!({
                        "index": index,
                        "id": id,
                        "type": "function",
                        "function": {"name": name, "arguments": ""}
                    })
                })
                .collect();
            push_data(
                &mut out,
                &serde_json::json!({
                    "id": "chatcmpl-fake",
                    "object": "chat.completion.chunk",
                    "choices": [{
                        "index": 0,
                        "delta": {"tool_calls": starts},
                        "finish_reason": null
                    }]
                }),
            );
            let arguments: Vec<Value> = calls
                .iter()
                .enumerate()
                .map(|(index, (_, _, args))| {
                    serde_json::json!({
                        "index": index,
                        "function": {"arguments": args}
                    })
                })
                .collect();
            push_data(
                &mut out,
                &serde_json::json!({
                    "id": "chatcmpl-fake",
                    "object": "chat.completion.chunk",
                    "choices": [{
                        "index": 0,
                        "delta": {"tool_calls": arguments},
                        "finish_reason": null
                    }]
                }),
            );
            push_data(
                &mut out,
                &serde_json::json!({
                    "id": "chatcmpl-fake",
                    "object": "chat.completion.chunk",
                    "choices": [
                        {"index": 0, "delta": {}, "finish_reason": "tool_calls"}
                    ],
                "usage": {
                    "prompt_tokens": 1,
                    "completion_tokens": calls.len() as u64,
                    "total_tokens": 1 + calls.len() as u64
                }
                }),
            );
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
        SseScript::HttpError { .. } => {
            // HttpError is short-circuited by `handle_request` before
            // rendering; reaching this arm is a harness bug.
            out.push_str("data: fake_llm: unresolved HttpError script\n\n");
            out.push_str("data: [DONE]\n\n");
        }
        SseScript::Sequence { .. } => {
            // rendering; reaching this arm is a harness bug. Emit a malformed
            // payload so the turn fails loudly instead of silently passing.
            out.push_str("data: fake_llm: unresolved Sequence script\n\n");
            out.push_str("data: [DONE]\n\n");
        }
        SseScript::Cycle { .. } => {
            // Cycle 与 Sequence 一样在 handle_request 里按请求解析；
            // 走到这里说明 harness 有 bug。发出畸形载荷，让 turn 大声
            // 失败而不是悄悄通过。
            out.push_str("data: fake_llm: unresolved Cycle script\n\n");
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
    use std::collections::HashMap;

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
        assert!(sse.contains("\"usage\""));
        assert!(sse.contains("\"total_tokens\":2"));
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
        let captured = parse_request_body(Protocol::OpenAi, None, &body);
        assert_eq!(captured.model, "compat:test");
        assert_eq!(captured.message_roles, vec!["system", "user"]);
        assert_eq!(
            captured.tool_names,
            vec!["observe__workspace", "session__todo_write"]
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
        let resp = std::str::from_utf8(&resp).expect("response must be valid UTF-8");

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

    fn exchange_raw(addr: std::net::SocketAddr, request: &[u8]) -> Vec<u8> {
        let mut stream = TcpStream::connect(addr).expect("connect");
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .expect("read timeout");
        stream.write_all(request).expect("write request");
        stream.flush().expect("flush request");
        let mut response = Vec::new();
        stream.read_to_end(&mut response).expect("read response");
        response
    }

    #[test]
    fn expected_api_key_rejects_foreign_credentials_without_capturing_them() {
        let mut server = FakeLlmServer::start(SseScript::plain("ok")).unwrap();
        server.expect_api_key("expected-fixture-key");
        for (key, status) in [
            ("foreign-fixture-key", "401"),
            ("expected-fixture-key", "200"),
        ] {
            let request = format!(
                "POST /chat/completions HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: 2\r\nAuthorization: Bearer {key}\r\n\r\n{{}}"
            );
            let response = exchange_raw(server.local_addr(), request.as_bytes());
            assert!(response.starts_with(format!("HTTP/1.1 {status}").as_bytes()));
            assert!(!String::from_utf8_lossy(&response).contains(key));
        }
        assert_eq!(server.captured_requests().len(), 1);
        server.shutdown(Duration::from_secs(2)).unwrap();
    }

    #[test]
    fn concurrent_sequence_keeps_capture_and_response_assignment_paired() {
        const REQUESTS: usize = 24;
        let scripts = (0..REQUESTS)
            .map(|index| SseScript::plain(format!("sequence-{index}")))
            .collect();
        let (server, gate) = FakeLlmServer::start_gated(SseScript::Sequence { scripts })
            .expect("gated sequence server");
        let mut clients = Vec::new();
        for index in 0..REQUESTS {
            let addr = server.local_addr();
            clients.push(thread::spawn(move || {
                let model = format!("request-{index}");
                let body = format!(r#"{{"model":"{model}","messages":[],"stream":true}}"#);
                let request = format!(
                    "POST /chat/completions HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
                    body.len()
                );
                (model, exchange_raw(addr, request.as_bytes()))
            }));
            server
                .wait_for_requests(index + 1, Duration::from_secs(2))
                .expect("capture in launch order");
        }

        let capture_order = server.captured_requests();
        gate.release(Duration::from_secs(2))
            .expect("bounded concurrent release");
        let responses: HashMap<_, _> = clients
            .into_iter()
            .map(|client| client.join().expect("client thread"))
            .collect();
        for (sequence_index, captured) in capture_order.iter().enumerate() {
            let response = responses.get(&captured.model).expect("response by model");
            let expected = format!(r#""content":"sequence-{sequence_index}""#);
            assert!(
                response
                    .windows(expected.len())
                    .any(|bytes| bytes == expected.as_bytes()),
                "capture position {sequence_index} ({}) received the wrong Sequence script",
                captured.model
            );
        }
    }

    fn request_in_thread(
        addr: std::net::SocketAddr,
    ) -> std::sync::mpsc::Receiver<std::io::Result<Vec<u8>>> {
        let (sent, received) = std::sync::mpsc::channel();
        thread::spawn(move || {
            let result = TcpStream::connect(addr).and_then(|mut stream| {
                let body = r#"{"model":"compat:test","messages":[],"stream":true}"#;
                write!(
                    stream,
                    "POST /chat/completions HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                    body.len(),
                    body
                )?;
                stream.flush()?;
                let mut response = Vec::new();
                stream.read_to_end(&mut response)?;
                Ok(response)
            });
            let _ = sent.send(result);
        });
        received
    }

    #[test]
    fn gated_response_waits_for_release_and_shutdown_joins_handlers() {
        let (server, gate) = FakeLlmServer::start_gated(SseScript::plain("gated")).expect("server");
        let response = request_in_thread(server.local_addr());
        server
            .wait_for_requests(1, Duration::from_secs(2))
            .expect("captured request");
        gate.wait_until_stalled(1, Duration::from_secs(2))
            .expect("handler reached gate");
        assert!(matches!(
            response.try_recv(),
            Err(std::sync::mpsc::TryRecvError::Empty)
        ));
        gate.release(Duration::from_secs(2))
            .expect("bounded release acknowledgement");
        let bytes = response
            .recv_timeout(Duration::from_secs(2))
            .expect("bounded response")
            .expect("response io");
        assert!(
            std::str::from_utf8(&bytes)
                .expect("response utf-8")
                .contains("gated")
        );

        let (mut server, gate) =
            FakeLlmServer::start_gated(SseScript::plain("cancelled")).expect("server");
        let response = request_in_thread(server.local_addr());
        server
            .wait_for_requests(1, Duration::from_secs(2))
            .expect("captured request");
        gate.wait_until_stalled(1, Duration::from_secs(2))
            .expect("handler reached cancellable gate");
        server
            .shutdown(Duration::from_secs(2))
            .expect("bounded shutdown");
        response
            .recv_timeout(Duration::from_secs(2))
            .expect("cancelled handler joined")
            .expect("client io");

        assert!(server.listener.is_none(), "listener was not removed");
        assert!(
            server.serve_handle.is_none(),
            "serve thread handle was not joined"
        );
        assert!(
            server.handlers.lock().unwrap().is_empty(),
            "handler thread handles were not joined"
        );
        let state = server.state.0.lock().unwrap();
        assert!(state.serve_exited, "serve thread did not publish exit");
        assert_eq!(state.active_handlers, 0, "handler remained active");
        assert!(
            state.first_failure.is_none(),
            "shutdown stored a thread failure: {:?}",
            state.first_failure
        );
    }

    #[test]
    fn segmented_response_delivers_only_released_frames_and_cancels_cleanly() {
        use std::io::{BufRead, BufReader};
        for cancel in [false, true] {
            let script = SseScript::PlainText {
                chunks: vec!["old-token".into(), "新-token".into()],
                finish_reason: "stop".into(),
            };
            let expected = render_script(&script);
            let (mut server, gate) =
                FakeLlmServer::start_segmented(script, Duration::from_secs(5)).unwrap();
            let mut stream = TcpStream::connect(server.local_addr()).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            stream.write_all(b"POST /chat/completions HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: 2\r\n\r\n{}").unwrap();
            let mut reader = BufReader::new(stream);
            let mut headers = String::new();
            loop {
                let mut line = String::new();
                assert_ne!(reader.read_line(&mut line).unwrap(), 0);
                headers.push_str(&line);
                if line == "\r\n" {
                    break;
                }
            }
            assert!(headers.contains(&format!("Content-Length: {}\r\n", expected.len())));
            for (index, token) in ["old-token", "新-token"].into_iter().enumerate() {
                gate.wait_until_stalled(index + 1, Duration::from_secs(2))
                    .unwrap();
                reader.get_mut().set_nonblocking(true).unwrap();
                let error = reader.read(&mut [0u8; 1]).unwrap_err();
                assert_eq!(error.kind(), std::io::ErrorKind::WouldBlock);
                reader.get_mut().set_nonblocking(false).unwrap();
                gate.release_next(Duration::from_secs(2)).unwrap();
                let mut frame = String::new();
                reader.read_line(&mut frame).unwrap();
                assert!(frame.contains(token) && !frame.contains("\"finish_reason\":\"stop\""));
                let mut separator = String::new();
                reader.read_line(&mut separator).unwrap();
                assert_eq!(separator, "\n");
            }
            gate.wait_until_stalled(3, Duration::from_secs(2)).unwrap();
            if cancel {
                server.shutdown(Duration::from_secs(2)).unwrap();
            } else {
                gate.release(Duration::from_secs(2)).unwrap();
            }
            let mut terminal = String::new();
            reader.read_to_string(&mut terminal).unwrap();
            if cancel {
                assert!(
                    terminal.is_empty(),
                    "shutdown must not deliver terminal frames"
                );
            } else {
                assert!(
                    terminal.contains("\"finish_reason\":\"stop\"")
                        && terminal.ends_with("data: [DONE]\n\n")
                );
                server.shutdown(Duration::from_secs(2)).unwrap();
            }
            assert_eq!(server.captured_requests().len(), 1);
            assert!(server.handlers.lock().unwrap().is_empty());
        }
    }

    #[test]
    fn segmented_delivery_deadline_fails_and_joins_without_release() {
        let (mut server, gate) =
            FakeLlmServer::start_segmented(SseScript::plain("withheld"), Duration::ZERO).unwrap();
        let response = request_in_thread(server.local_addr());
        let bytes = response
            .recv_timeout(Duration::from_secs(2))
            .unwrap()
            .unwrap();
        assert!(!String::from_utf8(bytes).unwrap().contains("withheld"));
        assert!(gate.release_next(Duration::from_secs(2)).is_err());
        let error = server.shutdown(Duration::from_secs(2)).unwrap_err();
        assert!(error.to_string().contains("segment"));
        assert!(server.handlers.lock().unwrap().is_empty());
    }

    /// 只有模型请求进入 capture；其他请求不能被静默接受，必须成为夹具失败。
    #[test]
    fn non_model_request_is_a_fixture_failure_and_not_captured() {
        let mut server = FakeLlmServer::start(SseScript::plain("unused")).unwrap();
        let response = exchange_raw(
            server.local_addr(),
            b"GET /models HTTP/1.1\r\nHost: localhost\r\n\r\n",
        );
        assert!(response.starts_with(b"HTTP/1.1 404"));
        assert!(server.captured_requests().is_empty());
        let error = server.shutdown(Duration::from_secs(2)).unwrap_err();
        assert!(
            matches!(error, ServeError::ThreadFailure(ref failure) if failure == "not a model request: GET /models"),
            "unexpected shutdown error: {error}"
        );
    }

    #[test]
    fn shutdown_returns_fixture_failure() {
        let log_directory = tempfile::tempdir().expect("log directory");
        let mut server =
            FakeLlmServer::start_logged(SseScript::plain("unused"), log_directory.path())
                .expect("server");
        let response = exchange_raw(
            server.local_addr(),
            b"POST /chat/completions HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: 2\r\n\r\n{}",
        );
        assert!(
            response.is_empty(),
            "failed handler must not send a response"
        );

        let error = server
            .shutdown(Duration::from_secs(2))
            .expect_err("shutdown must return the fixture failure");
        assert!(
            matches!(error, ServeError::ThreadFailure(ref failure) if failure.starts_with("io:")),
            "unexpected shutdown error: {error}"
        );
    }
}
