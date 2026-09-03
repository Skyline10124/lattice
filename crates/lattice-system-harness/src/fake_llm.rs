// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Localhost OpenAI-compatible fake LLM for harness scenarios.
//!
//! A minimal HTTP/SSE server bound to a scenario-local port. The product
//! connects through the formal `OPENAI_COMPATIBLE_BASE_URL` /
//! `OPENAI_COMPATIBLE_MODEL` / `OPENAI_COMPATIBLE_API_KEY` configuration —
//! there is no test-only provider, hidden flag, or product-side change.
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

use std::io::{Read, Write};
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
}

#[derive(Default)]
struct GateState {
    released: bool,
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
        let deadline = Instant::now() + timeout;
        let (lock, wake) = &*self.state;
        let mut state = lock.lock().unwrap();
        let target = state.stalled;
        state.released = true;
        wake.notify_all();
        while state.departed < target {
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

    fn wait(&self) -> bool {
        let (lock, wake) = &*self.state;
        let mut state = lock.lock().unwrap();
        state.stalled += 1;
        wake.notify_all();
        while !state.released && !state.cancelled {
            state = wake.wait(state).unwrap();
        }
        let released = state.released;
        state.departed += 1;
        wake.notify_all();
        released
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
        let gate = ResponseGate {
            state: Arc::new((Mutex::new(GateState::default()), Condvar::new())),
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

const MAX_REQUEST_HEADER_BYTES: usize = 16 * 1024;
const MAX_REQUEST_BODY_BYTES: usize = 1024 * 1024;

#[derive(Clone, Copy)]
struct HttpRejection {
    status: &'static str,
    body: &'static [u8],
}

impl HttpRejection {
    const fn new(status: &'static str, body: &'static [u8]) -> Self {
        Self { status, body }
    }
}

enum RequestReadError {
    Io(std::io::Error),
    Reject(HttpRejection),
}

impl From<std::io::Error> for RequestReadError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

fn reject(status: &'static str, body: &'static [u8]) -> RequestReadError {
    RequestReadError::Reject(HttpRejection::new(status, body))
}

fn read_request_header(stream: &mut TcpStream) -> Result<Vec<u8>, RequestReadError> {
    let mut header = Vec::with_capacity(MAX_REQUEST_HEADER_BYTES);
    let mut byte = [0_u8; 1];
    loop {
        if header.len() == MAX_REQUEST_HEADER_BYTES {
            return Err(reject(
                "431 Request Header Fields Too Large",
                b"request headers too large\n",
            ));
        }
        match stream.read(&mut byte) {
            Ok(0) => {
                return Err(reject("400 Bad Request", b"malformed http framing\n"));
            }
            Ok(_) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                ) =>
            {
                return Err(reject("400 Bad Request", b"malformed http framing\n"));
            }
            Err(error) => return Err(RequestReadError::Io(error)),
        }
        if byte[0] == b'\n' && header.last() != Some(&b'\r') {
            return Err(reject("400 Bad Request", b"malformed http framing\n"));
        }
        if header.last() == Some(&b'\r') && byte[0] != b'\n' {
            return Err(reject("400 Bad Request", b"malformed http framing\n"));
        }
        header.push(byte[0]);
        if header.ends_with(b"\r\n\r\n") {
            return Ok(header);
        }
    }
}

fn is_header_name_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric()
        || matches!(
            byte,
            b'!' | b'#'
                | b'$'
                | b'%'
                | b'&'
                | b'\''
                | b'*'
                | b'+'
                | b'-'
                | b'.'
                | b'^'
                | b'_'
                | b'`'
                | b'|'
                | b'~'
        )
}

fn trim_ascii_spaces(mut bytes: &[u8]) -> &[u8] {
    while bytes.first() == Some(&b' ') {
        bytes = &bytes[1..];
    }
    while bytes.last() == Some(&b' ') {
        bytes = &bytes[..bytes.len() - 1];
    }
    bytes
}

fn parse_decimal_length(bytes: &[u8]) -> Option<usize> {
    if bytes.is_empty() || !bytes.iter().all(u8::is_ascii_digit) {
        return None;
    }
    bytes.iter().try_fold(0_usize, |value, byte| {
        value.checked_mul(10)?.checked_add(usize::from(byte - b'0'))
    })
}

fn read_http_request(stream: &mut TcpStream) -> Result<String, RequestReadError> {
    let header = read_request_header(stream)?;
    let mut lines = header[..header.len() - 2].split(|byte| *byte == b'\n');
    let request_line = lines
        .next()
        .and_then(|line| line.strip_suffix(b"\r"))
        .ok_or_else(|| reject("400 Bad Request", b"malformed request line\n"))?;
    let mut request_parts = request_line.split(|byte| *byte == b' ');
    let (Some(method), Some(target), Some(version), None) = (
        request_parts.next(),
        request_parts.next(),
        request_parts.next(),
        request_parts.next(),
    ) else {
        return Err(reject("400 Bad Request", b"malformed request line\n"));
    };
    if method != b"POST" {
        return Err(reject("405 Method Not Allowed", b"method not allowed\n"));
    }
    if target != b"/chat/completions" {
        return Err(reject("404 Not Found", b"route not found\n"));
    }
    if version != b"HTTP/1.1" {
        return Err(reject(
            "505 HTTP Version Not Supported",
            b"http version not supported\n",
        ));
    }

    let mut host: Option<&[u8]> = None;
    let mut content_type: Option<&[u8]> = None;
    let mut content_length: Option<&[u8]> = None;
    let mut transfer_encoding = false;
    for line in lines {
        if line.is_empty() {
            continue;
        }
        let Some(line) = line.strip_suffix(b"\r") else {
            return Err(reject("400 Bad Request", b"malformed headers\n"));
        };
        let Some(colon) = line.iter().position(|byte| *byte == b':') else {
            return Err(reject("400 Bad Request", b"malformed headers\n"));
        };
        let (name, value) = (&line[..colon], trim_ascii_spaces(&line[colon + 1..]));
        if name.is_empty()
            || !name.iter().copied().all(is_header_name_byte)
            || !value.iter().all(|byte| matches!(byte, 0x20..=0x7e))
        {
            return Err(reject("400 Bad Request", b"malformed headers\n"));
        }
        if name.eq_ignore_ascii_case(b"host") {
            if host.replace(value).is_some() {
                return Err(reject("400 Bad Request", b"invalid host header\n"));
            }
        } else if name.eq_ignore_ascii_case(b"content-type") {
            if content_type.replace(value).is_some() {
                return Err(reject("400 Bad Request", b"invalid content-type\n"));
            }
        } else if name.eq_ignore_ascii_case(b"content-length") {
            if content_length.replace(value).is_some() {
                return Err(reject("400 Bad Request", b"invalid content-length\n"));
            }
        } else if name.eq_ignore_ascii_case(b"transfer-encoding") {
            transfer_encoding = true;
        }
    }

    if transfer_encoding {
        return Err(reject("400 Bad Request", b"unsupported framing\n"));
    }
    if host.is_none_or(<[u8]>::is_empty) {
        return Err(reject("400 Bad Request", b"missing host header\n"));
    }
    if !content_type.is_some_and(|value| value.eq_ignore_ascii_case(b"application/json")) {
        return Err(reject("400 Bad Request", b"invalid content-type\n"));
    }
    let Some(content_length) = content_length else {
        return Err(reject("411 Length Required", b"content-length required\n"));
    };
    let Some(body_len) = parse_decimal_length(content_length) else {
        return Err(reject("400 Bad Request", b"invalid content-length\n"));
    };
    if body_len > MAX_REQUEST_BODY_BYTES {
        return Err(reject("413 Content Too Large", b"request body too large\n"));
    }

    let mut body = vec![0_u8; body_len];
    if let Err(error) = stream.read_exact(&mut body) {
        return match error.kind() {
            std::io::ErrorKind::UnexpectedEof
            | std::io::ErrorKind::TimedOut
            | std::io::ErrorKind::WouldBlock => {
                Err(reject("400 Bad Request", b"incomplete request body\n"))
            }
            _ => Err(RequestReadError::Io(error)),
        };
    }
    String::from_utf8(body).map_err(|_| reject("400 Bad Request", b"invalid utf-8 body\n"))
}

fn handle_request(
    mut stream: TcpStream,
    shared: &(Mutex<ServerState>, Condvar),
    gate: Option<&ResponseGate>,
    request_log: Option<&Path>,
) -> Result<(), ServeError> {
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    let body = match read_http_request(&mut stream) {
        Ok(body) => body,
        Err(RequestReadError::Io(error)) => return Err(ServeError::Io(error)),
        Err(RequestReadError::Reject(rejection)) => {
            write_http_response(&mut stream, rejection.status, "text/plain", rejection.body)?;
            return Ok(());
        }
    };

    let captured = parse_chat_completions_body(&body);
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
    if let Some(gate) = gate
        && !gate.wait()
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
    let sse = render_script(&active);
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
                    "choices": [{"index": 0, "delta": {}, "finish_reason": "tool_calls"}]
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
    fn request_contract_accepts_only_post_chat_completions_http_1_1() {
        let server = FakeLlmServer::start(SseScript::plain("unused")).expect("server");
        let cases: &[(&[u8], &[u8])] = &[
            (
                b"GET /chat/completions HTTP/1.1",
                b"HTTP/1.1 405 Method Not Allowed\r\nContent-Type: text/plain\r\nContent-Length: 19\r\nConnection: close\r\n\r\nmethod not allowed\n",
            ),
            (
                b"POST /v1/chat/completions HTTP/1.1",
                b"HTTP/1.1 404 Not Found\r\nContent-Type: text/plain\r\nContent-Length: 16\r\nConnection: close\r\n\r\nroute not found\n",
            ),
            (
                b"POST /chat/completions HTTP/1.0",
                b"HTTP/1.1 505 HTTP Version Not Supported\r\nContent-Type: text/plain\r\nContent-Length: 27\r\nConnection: close\r\n\r\nhttp version not supported\n",
            ),
            (
                b"POST  /chat/completions HTTP/1.1",
                b"HTTP/1.1 400 Bad Request\r\nContent-Type: text/plain\r\nContent-Length: 23\r\nConnection: close\r\n\r\nmalformed request line\n",
            ),
            (
                b"POST\xff/chat/completions HTTP/1.1",
                b"HTTP/1.1 400 Bad Request\r\nContent-Type: text/plain\r\nContent-Length: 23\r\nConnection: close\r\n\r\nmalformed request line\n",
            ),
            (
                b"POST\t/chat/completions HTTP/1.1",
                b"HTTP/1.1 400 Bad Request\r\nContent-Type: text/plain\r\nContent-Length: 23\r\nConnection: close\r\n\r\nmalformed request line\n",
            ),
        ];

        for (request_line, expected) in cases {
            let mut request = request_line.to_vec();
            request.extend_from_slice(
                b"\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: 2\r\n\r\n{}",
            );
            assert_eq!(exchange_raw(server.local_addr(), &request), *expected);
        }
        assert!(server.captured_requests().is_empty());
    }

    #[test]
    fn request_contract_requires_single_bounded_content_length() {
        let server = FakeLlmServer::start(SseScript::plain("unused")).expect("server");
        let bad_length = b"HTTP/1.1 400 Bad Request\r\nContent-Type: text/plain\r\nContent-Length: 23\r\nConnection: close\r\n\r\ninvalid content-length\n";
        let cases: &[(&[u8], &[u8])] = &[
            (
                b"POST /chat/completions HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\n\r\n",
                b"HTTP/1.1 411 Length Required\r\nContent-Type: text/plain\r\nContent-Length: 24\r\nConnection: close\r\n\r\ncontent-length required\n",
            ),
            (
                b"POST /chat/completions HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: -1\r\n\r\n",
                bad_length,
            ),
            (
                b"POST /chat/completions HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: 2\r\nContent-Length: 2\r\n\r\n{}",
                bad_length,
            ),
            (
                b"POST /chat/completions HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: 2\r\nContent-Length: 3\r\n\r\n{}",
                bad_length,
            ),
            (
                b"POST /chat/completions HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: 1048577\r\n\r\n",
                b"HTTP/1.1 413 Content Too Large\r\nContent-Type: text/plain\r\nContent-Length: 23\r\nConnection: close\r\n\r\nrequest body too large\n",
            ),
        ];

        for (request, expected) in cases {
            assert_eq!(exchange_raw(server.local_addr(), request), *expected);
        }

        let mut stream = TcpStream::connect(server.local_addr()).expect("connect");
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .expect("read timeout");
        stream
            .write_all(
                b"POST /chat/completions HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: 4\r\n\r\n{}",
            )
            .expect("write short body");
        stream
            .shutdown(Shutdown::Write)
            .expect("half-close request write side");
        let mut response = Vec::new();
        stream
            .read_to_end(&mut response)
            .expect("read incomplete-body response");
        assert_eq!(
            response,
            b"HTTP/1.1 400 Bad Request\r\nContent-Type: text/plain\r\nContent-Length: 24\r\nConnection: close\r\n\r\nincomplete request body\n"
        );
        assert!(server.captured_requests().is_empty());
    }

    #[test]
    fn request_contract_rejects_unsupported_framing_and_invalid_utf8() {
        let server = FakeLlmServer::start(SseScript::plain("unused")).expect("server");
        let bad_framing = b"HTTP/1.1 400 Bad Request\r\nContent-Type: text/plain\r\nContent-Length: 23\r\nConnection: close\r\n\r\nmalformed http framing\n";
        let bad_headers = b"HTTP/1.1 400 Bad Request\r\nContent-Type: text/plain\r\nContent-Length: 18\r\nConnection: close\r\n\r\nmalformed headers\n";
        let cases: &[(&[u8], &[u8])] = &[
            (
                b"POST /chat/completions HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nTransfer-Encoding: chunked\r\nContent-Length: 2\r\n\r\n{}",
                b"HTTP/1.1 400 Bad Request\r\nContent-Type: text/plain\r\nContent-Length: 20\r\nConnection: close\r\n\r\nunsupported framing\n",
            ),
            (
                b"POST /chat/completions HTTP/1.1\nHost: localhost\nContent-Type: application/json\nContent-Length: 2\n\n{}",
                bad_framing,
            ),
            (
                b"POST /chat/completions HTTP/1.1\r\nHost: local\xffhost\r\nContent-Type: application/json\r\nContent-Length: 2\r\n\r\n{}",
                bad_headers,
            ),
            (
                b"POST /chat/completions HTTP/1.1\r\nContent-Type: application/json\r\nContent-Length: 2\r\n\r\n{}",
                b"HTTP/1.1 400 Bad Request\r\nContent-Type: text/plain\r\nContent-Length: 20\r\nConnection: close\r\n\r\nmissing host header\n",
            ),
            (
                b"POST /chat/completions HTTP/1.1\r\nHost: localhost\r\nContent-Length: 2\r\n\r\n{}",
                b"HTTP/1.1 400 Bad Request\r\nContent-Type: text/plain\r\nContent-Length: 21\r\nConnection: close\r\n\r\ninvalid content-type\n",
            ),
            (
                b"POST /chat/completions HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: 1\r\n\r\n\xff",
                b"HTTP/1.1 400 Bad Request\r\nContent-Type: text/plain\r\nContent-Length: 19\r\nConnection: close\r\n\r\ninvalid utf-8 body\n",
            ),
        ];

        for (request, expected) in cases {
            assert_eq!(exchange_raw(server.local_addr(), request), *expected);
        }
        assert!(server.captured_requests().is_empty());
    }

    #[test]
    fn request_contract_rejects_oversize_headers_before_terminator() {
        let server = FakeLlmServer::start(SseScript::plain("unused")).expect("server");
        let mut request = b"POST /chat/completions HTTP/1.1\r\nX-Fill: ".to_vec();
        request.extend(std::iter::repeat_n(b'a', MAX_REQUEST_HEADER_BYTES));
        assert_eq!(
            exchange_raw(server.local_addr(), &request),
            b"HTTP/1.1 431 Request Header Fields Too Large\r\nContent-Type: text/plain\r\nContent-Length: 26\r\nConnection: close\r\n\r\nrequest headers too large\n"
        );
        assert!(server.captured_requests().is_empty());
    }

    #[test]
    fn http_error_responses_have_byte_exact_crlf_and_content_length() {
        let body = r#"{"error":"请求失败"}"#;
        let server = FakeLlmServer::start(SseScript::HttpError {
            status: 429,
            body: body.into(),
        })
        .expect("server");
        let response = exchange_raw(
            server.local_addr(),
            b"POST /chat/completions HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: 2\r\n\r\n{}",
        );
        assert_eq!(
            response,
            concat!(
                "HTTP/1.1 429 Error\r\n",
                "Content-Type: application/json\r\n",
                "Content-Length: 24\r\n",
                "Connection: close\r\n",
                "\r\n",
                "{\"error\":\"请求失败\"}"
            )
            .as_bytes()
        );
    }

    #[test]
    fn cycle_serves_scripts_round_robin_across_requests() {
        let server = FakeLlmServer::start(SseScript::Cycle {
            scripts: vec![SseScript::plain("cycle-a"), SseScript::plain("cycle-b")],
        })
        .expect("cycle server");
        let addr = server.local_addr();
        let body = r#"{"model":"m","messages":[],"stream":true}"#;
        let request = format!(
            "POST /chat/completions HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );
        for expected in ["cycle-a", "cycle-b", "cycle-a", "cycle-b"] {
            let response = exchange_raw(addr, request.as_bytes());
            let needle = format!(r#""content":"{expected}""#);
            assert!(
                response
                    .windows(needle.len())
                    .any(|bytes| bytes == needle.as_bytes()),
                "Cycle 必须按轮转重复脚本；期望 {expected}"
            );
        }
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
