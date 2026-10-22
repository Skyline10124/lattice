// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Localhost fake LLM for harness scenarios.
//!
//! The product connects through the selected provider's formal base URL and
//! credential configuration (`OPENAI_COMPATIBLE_*`, `ANTHROPIC_BASE_URL`,
//! `GEMINI_BASE_URL`) — there is no test-only provider, hidden flag, or
//! product-side change. The request route selects the streaming protocol:
//! OpenAI Chat Completions, Anthropic Messages, or Gemini
//! `streamGenerateContent`.
//!
//! 传输与官方 provider 一致：TLS + HTTP/2，ALPN 只提供 `h2`，证书来自
//! [`TlsIdentity`]，场景经 `SSL_CERT_FILE` 信任它（见 `ScenarioWorld`）。
//! [`FakeLlmServer::start_plain_http1`] 另起明文 HTTP/1.1，只服务本地 provider
//! （Ollama 一类）场景，只支持完整响应。
//!
//! The server does two things (RFC #41, Codeberg #19, decision four):
//!
//! 1. Captures each model request (model, messages, tool names, identifying
//!    headers, connection) so scenarios can assert on what the product sent.
//! 2. Answers with the script chosen by the scenario: plain text, tool calls,
//!    HTTP errors, or a transport failure (stream cut, reset, GOAWAY, refused
//!    stream).
//!
//! SSE bodies mirror the real streams: one `data: {chunk}\n\n` per chunk,
//! terminated by `data: [DONE]\n\n` in the OpenAI protocol.

use std::collections::BTreeMap;
use std::future::Future;
use std::io::Write;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Condvar, Mutex};
use std::task::{Context, Poll, ready};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use bytes::Bytes;
use h2::Reason;
use h2::server::SendResponse;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;
use tokio::io::{
    AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader, ReadBuf,
};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, watch};
use tokio_rustls::TlsAcceptor;

use crate::tls::TlsIdentity;

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
    /// 标识客户端的请求头（`user-agent`、`accept`、`x-client-request-id`），名称小写。
    pub headers: BTreeMap<String, String>,
    /// 承载本请求的 TCP 连接序号，按服务端接受顺序从 0 起；同号表示复用了同一条连接。
    pub connection: usize,
    /// 服务端启动到收到本请求的毫秒数，用于断言两次请求之间的等待。
    pub received_ms: u64,
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
    /// 带指定 Content-Type 与额外响应头的非 2xx 响应，用于 `retry-after`、HTML 网关错误页等形状。
    HttpResponse {
        status: u16,
        content_type: String,
        headers: Vec<(String, String)>,
        body: String,
    },
    /// 交付内层脚本的事件后先发 `GOAWAY(NO_ERROR)` 优雅关闭连接，再结束流，模拟 provider
    /// 回收连接。只用于 HTTP/2。
    CloseAfter { script: Box<SseScript> },
    /// 发出 `chunks_before_drop` 后正常结束流（END_STREAM），没有终止 chunk 与 `[DONE]`：
    /// 产品看到的是流在结束事件之前结束。只用于 HTTP/2。
    Disconnect { chunks_before_drop: Vec<String> },
    /// 与 `Disconnect` 相同，但以 `RST_STREAM(INTERNAL_ERROR)` 结束：HTTP 层看到的是
    /// 响应体读到一半出错，而不是正常结束。只用于 HTTP/2。
    Truncated { chunks_before_drop: Vec<String> },
    /// 收到请求后发 `GOAWAY(NO_ERROR)` 并关闭连接，GOAWAY 的最后流号小于本请求：
    /// 按 RFC 9113 §6.8，客户端可确知本请求未被处理。只用于 HTTP/2。
    GoawayUnprocessed,
    /// 以 `RST_STREAM(REFUSED_STREAM)` 拒绝本请求（RFC 9113 §8.7：未被处理）。只用于 HTTP/2。
    RefusedStream,
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
    #[error("provider would reject the tool definitions: {0}")]
    RejectedToolSchema(String),
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

    /// 在阻塞线程上等待 gate，不占用异步运行时的工作线程。
    async fn pass(&self) -> Result<bool, ServeError> {
        let gate = self.clone();
        tokio::task::spawn_blocking(move || gate.wait())
            .await
            .map_err(|_| ServeError::ThreadFailure("gate wait task failed".into()))?
    }
}

struct ServerState {
    script: SseScript,
    next: usize,
    requests: Vec<CapturedRequest>,
    shutdown_requested: bool,
    serve_exited: bool,
    /// 在途的连接与请求处理任务数。
    active_handlers: usize,
    first_failure: Option<String>,
    expected_api_key: Option<String>,
    /// 已接受的 TCP 连接数，也是下一条连接的序号。
    connections: usize,
    /// 客户端在 TLS 握手中拒绝了服务端证书的次数。
    tls_rejections: usize,
    started: Instant,
}

impl ServerState {
    fn record_failure(&mut self, failure: impl Into<String>) {
        if self.first_failure.is_none() {
            self.first_failure = Some(failure.into());
        }
    }
}

/// 服务线程与各处理任务共享的上下文。
struct Shared {
    state: Arc<(Mutex<ServerState>, Condvar)>,
    gate: Option<ResponseGate>,
    request_log: Option<PathBuf>,
    stop: watch::Receiver<bool>,
}

impl Shared {
    fn segmented(&self) -> bool {
        self.gate
            .as_ref()
            .is_some_and(|gate| gate.segment_deadline.is_some())
    }

    /// 整份响应 gate：返回 `false` 表示 gate 已取消，不再响应。
    async fn pass_whole_response(&self) -> Result<bool, ServeError> {
        match &self.gate {
            Some(gate) if !self.segmented() => gate.pass().await,
            _ => Ok(true),
        }
    }

    /// 分段 gate：每帧 SSE 前等待一次。
    async fn pass_frame(&self) -> Result<bool, ServeError> {
        match &self.gate {
            Some(gate) if self.segmented() => gate.pass().await,
            _ => Ok(true),
        }
    }
}

pub struct FakeLlmServer {
    addr: SocketAddr,
    plain_http1: bool,
    state: Arc<(Mutex<ServerState>, Condvar)>,
    stop: watch::Sender<bool>,
    serve_handle: Option<JoinHandle<()>>,
    gate: Option<ResponseGate>,
}

impl FakeLlmServer {
    /// Bind a new TLS + HTTP/2 server to `127.0.0.1:0` (OS-assigned port) and
    /// start serving `script` with the shared harness [`TlsIdentity`].
    pub fn start(script: SseScript) -> Result<Self, ServeError> {
        Self::start_inner(script, None, None, Some(TlsIdentity::shared()?))
    }

    /// 明文 HTTP/1.1，只用于本地 provider 场景；只支持完整的流式响应与 HTTP 错误响应。
    pub fn start_plain_http1(script: SseScript) -> Result<Self, ServeError> {
        Self::start_inner(script, None, None, None)
    }

    /// Start with a crash-surviving append-only request counter.
    pub fn start_logged(script: SseScript, path: &Path) -> Result<Self, ServeError> {
        Self::start_inner(
            script,
            None,
            Some(path.to_path_buf()),
            Some(TlsIdentity::shared()?),
        )
    }

    pub fn start_gated(script: SseScript) -> Result<(Self, ResponseGate), ServeError> {
        Self::start_controlled(script, None, TlsIdentity::shared()?)
    }

    /// 用指定的 TLS 身份启动带 gate 的服务端，供需要不同信任根的场景使用。
    pub fn start_gated_with(
        script: SseScript,
        identity: &TlsIdentity,
    ) -> Result<(Self, ResponseGate), ServeError> {
        Self::start_controlled(script, None, identity)
    }

    /// 每帧 SSE 单独放行；全部交付受同一绝对期限约束，shutdown/Drop 仍取消 gate。
    pub fn start_segmented(
        script: SseScript,
        timeout: Duration,
    ) -> Result<(Self, ResponseGate), ServeError> {
        Self::start_controlled(
            script,
            Some(Instant::now() + timeout),
            TlsIdentity::shared()?,
        )
    }

    fn start_controlled(
        script: SseScript,
        segment_deadline: Option<Instant>,
        identity: &TlsIdentity,
    ) -> Result<(Self, ResponseGate), ServeError> {
        let gate = ResponseGate {
            state: Arc::new((Mutex::new(GateState::default()), Condvar::new())),
            segment_deadline,
        };
        let server = Self::start_inner(script, Some(gate.clone()), None, Some(identity))?;
        Ok((server, gate))
    }

    fn start_inner(
        script: SseScript,
        gate: Option<ResponseGate>,
        request_log: Option<PathBuf>,
        identity: Option<&TlsIdentity>,
    ) -> Result<Self, ServeError> {
        let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
        listener.set_nonblocking(true)?;
        let addr = listener.local_addr()?;
        let acceptor = identity
            .map(|identity| identity.server_config(&[b"h2"]))
            .transpose()?
            .map(TlsAcceptor::from);
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
                connections: 0,
                tls_rejections: 0,
                started: Instant::now(),
            }),
            Condvar::new(),
        ));
        let (stop, stop_rx) = watch::channel(false);
        let shared = Arc::new(Shared {
            state: Arc::clone(&state),
            gate: gate.clone(),
            request_log,
            stop: stop_rx,
        });
        let handle = thread::Builder::new()
            .name("fake-llm-server".into())
            .spawn(move || {
                let thread_state = Arc::clone(&shared.state);
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    run_server(listener, acceptor, shared)
                }));
                let (lock, wake) = &*thread_state;
                let mut state = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                match result {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) => state.record_failure(error.to_string()),
                    Err(_) => state.record_failure("serve thread panicked"),
                }
                state.serve_exited = true;
                wake.notify_all();
            })?;
        Ok(Self {
            addr,
            plain_http1: identity.is_none(),
            state,
            stop,
            serve_handle: Some(handle),
            gate,
        })
    }

    /// The base URL the product should connect to, without a trailing slash.
    /// HTTP/2 用 `https://127.0.0.1`（证书含该 IP）；明文 HTTP/1.1 用本地 provider
    /// 常见的 `http://localhost`。
    pub fn base_url(&self) -> String {
        if self.plain_http1 {
            format!("http://localhost:{}", self.addr.port())
        } else {
            format!("https://127.0.0.1:{}", self.addr.port())
        }
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

    /// 客户端在 TLS 握手中拒绝服务端证书的次数。
    pub fn rejected_handshakes(&self) -> usize {
        self.state.0.lock().unwrap().tls_rejections
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

    /// 发出停止信号并有界等待服务线程收尾。超时返回错误但保留线程句柄，
    /// 再次调用（或 Drop）会继续等待，直到线程真正 join。
    pub fn shutdown(&mut self, timeout: Duration) -> Result<(), ServeError> {
        if self.serve_handle.is_none() {
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
        let _ = self.stop.send(true);

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
        match self.state.0.lock().unwrap().first_failure.clone() {
            Some(failure) => Err(ServeError::ThreadFailure(failure)),
            None => Ok(()),
        }
    }

    #[cfg(test)]
    fn local_addr(&self) -> SocketAddr {
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

/// 服务线程：在自己的运行时里接受连接，关停后等在途任务收尾。
fn run_server(
    listener: std::net::TcpListener,
    acceptor: Option<TlsAcceptor>,
    shared: Arc<Shared>,
) -> Result<(), ServeError> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    let result = runtime.block_on(accept_loop(listener, acceptor, Arc::clone(&shared)));
    // 被 gate 卡住的任务已由 shutdown 取消 gate 放行，其余任务观察到 stop 后退出。
    let (lock, wake) = &*shared.state;
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut state = lock.lock().unwrap();
    while state.active_handlers != 0 && Instant::now() < deadline {
        state = wake
            .wait_timeout(state, deadline.saturating_duration_since(Instant::now()))
            .unwrap()
            .0;
    }
    drop(state);
    runtime.shutdown_timeout(Duration::from_secs(1));
    let mut state = lock.lock().unwrap();
    if state.active_handlers != 0 {
        state.record_failure("handler task did not finish before shutdown");
        state.active_handlers = 0;
    }
    result
}

async fn accept_loop(
    listener: std::net::TcpListener,
    acceptor: Option<TlsAcceptor>,
    shared: Arc<Shared>,
) -> Result<(), ServeError> {
    let listener = tokio::net::TcpListener::from_std(listener)?;
    let mut stop = shared.stop.clone();
    loop {
        let tcp = tokio::select! {
            accepted = listener.accept() => accepted?.0,
            _ = stopped(&mut stop) => return Ok(()),
        };
        let connection = {
            let mut state = shared.state.0.lock().unwrap();
            if state.shutdown_requested {
                return Ok(());
            }
            state.connections += 1;
            state.connections - 1
        };
        match &acceptor {
            Some(acceptor) => spawn_tracked(
                &shared,
                serve_h2(tcp, acceptor.clone(), connection, Arc::clone(&shared)),
            ),
            None => spawn_tracked(&shared, serve_h1(tcp, connection, Arc::clone(&shared))),
        }
    }
}

/// 计入在途任务；任务失败或 panic 记为夹具失败。shutdown 发出停止信号时中止任务，
/// 所以 TLS 握手、读请求等对端驱动的等待都不会拖住关停。
fn spawn_tracked<F>(shared: &Arc<Shared>, task: F)
where
    F: Future<Output = Result<(), ServeError>> + Send + 'static,
{
    shared.state.0.lock().unwrap().active_handlers += 1;
    let shared = Arc::clone(shared);
    tokio::spawn(async move {
        let mut stop = shared.stop.clone();
        let mut handle = tokio::spawn(task);
        let outcome = tokio::select! {
            outcome = &mut handle => outcome,
            _ = stopped(&mut stop) => {
                handle.abort();
                handle.await
            }
        };
        let (lock, wake) = &*shared.state;
        let mut state = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        match outcome {
            Ok(Ok(())) => {}
            Ok(Err(error)) => state.record_failure(error.to_string()),
            Err(error) if error.is_panic() => state.record_failure("handler task panicked"),
            Err(_) => {}
        }
        state.active_handlers -= 1;
        wake.notify_all();
    });
}

/// 等到 shutdown 发出停止信号。包一层是为了不把 watch 的读锁守卫留在 `select!` 的输出里。
async fn stopped(stop: &mut watch::Receiver<bool>) {
    let _ = stop.wait_for(|stop| *stop).await;
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

/// One request from the product under test. The product is the fake provider's
/// only client, so parsing extracts just the route, credential, identifying
/// headers and body; it does not defend against malformed or hostile HTTP.
struct HttpRequest {
    method: String,
    path: String,
    /// `authorization`, `x-api-key` and `x-goog-api-key`, names lowercased.
    credentials: Vec<(String, String)>,
    /// [`CapturedRequest::headers`] 收录的请求头。
    headers: BTreeMap<String, String>,
    body: String,
}

impl HttpRequest {
    fn new(
        method: &str,
        path: String,
        header_pairs: impl IntoIterator<Item = (String, String)>,
        body: &[u8],
    ) -> Self {
        let (mut credentials, mut headers) = (Vec::new(), BTreeMap::new());
        for (name, value) in header_pairs {
            match name.as_str() {
                "authorization" | "x-api-key" | "x-goog-api-key" => credentials.push((name, value)),
                "user-agent" | "accept" | "x-client-request-id" => {
                    headers.insert(name, value);
                }
                _ => {}
            }
        }
        Self {
            method: method.to_string(),
            path,
            credentials,
            headers,
            body: String::from_utf8_lossy(body).into_owned(),
        }
    }
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

/// 对一条请求的回应，与传输无关。
enum Reply {
    Status {
        status: u16,
        content_type: String,
        headers: Vec<(String, String)>,
        body: String,
    },
    Sse {
        frames: Vec<String>,
        end: SseEnd,
    },
    GoawayUnprocessed,
    RefusedStream,
}

/// SSE 帧发完之后怎样结束。
#[derive(Clone, Copy, PartialEq, Eq)]
enum SseEnd {
    /// 正常结束流。
    Complete,
    /// 先优雅关闭连接，再正常结束流。
    CloseThenComplete,
    /// 正常结束流，但帧里没有结束事件。
    CleanEof,
    /// 以 `RST_STREAM(INTERNAL_ERROR)` 结束。
    Reset,
}

impl Reply {
    fn text(status: u16, body: &str) -> Self {
        Reply::Status {
            status,
            content_type: "text/plain".into(),
            headers: Vec::new(),
            body: body.into(),
        }
    }
}

/// 路由、核对凭据、登记 capture 并按脚本决定回应。第二项是回应之后要报告的夹具失败。
/// 会写请求日志并 fsync，调用方放在阻塞线程上执行。
fn decide(
    shared: &Shared,
    request: HttpRequest,
    connection: usize,
) -> Result<(Reply, Option<ServeError>), ServeError> {
    let Some((protocol, path_model)) = Protocol::route(&request.method, &request.path) else {
        let failure = ServeError::UnexpectedRequest(format!("{} {}", request.method, request.path));
        return Ok((Reply::text(404, "not a model request\n"), Some(failure)));
    };
    let expected = shared.state.0.lock().unwrap().expected_api_key.clone();
    if expected
        .as_deref()
        .is_some_and(|expected| protocol.api_key(&request) != Some(expected))
    {
        return Ok((Reply::text(401, "unauthorized\n"), None));
    }
    if let Some(rejection) = tool_schema_rejection(protocol, &request.body) {
        let body = serde_json::json!({"error": {
            "message": rejection,
            "type": "invalid_request_error",
            "code": "invalid_function_parameters",
        }});
        let reply = Reply::Status {
            status: 400,
            content_type: "application/json".into(),
            headers: Vec::new(),
            body: body.to_string(),
        };
        return Ok((reply, Some(ServeError::RejectedToolSchema(rejection))));
    }
    let mut captured = parse_request_body(protocol, path_model, &request.body);
    captured.headers = request.headers;
    captured.connection = connection;
    // Script assignment and capture publication are one transaction. Capture
    // order is therefore the same order used to advance a Sequence, even if
    // gated handlers resume in a different order.
    let active = {
        let (lock, wake) = &*shared.state;
        let mut state = lock.lock().unwrap();
        let (active, advance_sequence) = match &state.script {
            SseScript::Sequence { scripts } | SseScript::Cycle { scripts }
                if scripts.is_empty() =>
            {
                (
                    SseScript::IllegalResponse {
                        body: "fake_llm: empty Sequence/Cycle script".into(),
                    },
                    false,
                )
            }
            SseScript::Sequence { scripts } => {
                (scripts[state.next.min(scripts.len() - 1)].clone(), true)
            }
            SseScript::Cycle { scripts } => (scripts[state.next % scripts.len()].clone(), true),
            other => (other.clone(), false),
        };
        state.next += usize::from(advance_sequence);
        captured.received_ms = state.started.elapsed().as_millis() as u64;
        state.requests.push(captured);
        wake.notify_all();
        active
    };
    if let Some(path) = &shared.request_log {
        append_fsynced(path, b"request\n")?;
    }
    let (active, end) = match active {
        SseScript::CloseAfter { script } => (*script, Some(SseEnd::CloseThenComplete)),
        SseScript::Truncated { chunks_before_drop } => (
            SseScript::Disconnect { chunks_before_drop },
            Some(SseEnd::Reset),
        ),
        other => (other, None),
    };
    let reply = match active {
        SseScript::HttpError { status, body } => Reply::Status {
            status,
            content_type: "application/json".into(),
            headers: Vec::new(),
            body,
        },
        SseScript::HttpResponse {
            status,
            content_type,
            headers,
            body,
        } => Reply::Status {
            status,
            content_type,
            headers,
            body,
        },
        SseScript::GoawayUnprocessed => Reply::GoawayUnprocessed,
        SseScript::RefusedStream => Reply::RefusedStream,
        script => {
            let Some(sse) = render(protocol, &script) else {
                let failure = ServeError::UnsupportedScript(protocol.name());
                return Ok((
                    Reply::text(500, "script not supported for this protocol\n"),
                    Some(failure),
                ));
            };
            let end = end.unwrap_or(if matches!(script, SseScript::Disconnect { .. }) {
                SseEnd::CleanEof
            } else {
                SseEnd::Complete
            });
            Reply::Sse {
                frames: sse.split_inclusive("\n\n").map(str::to_owned).collect(),
                end,
            }
        }
    };
    Ok((reply, None))
}

async fn decide_blocking(
    shared: &Arc<Shared>,
    request: HttpRequest,
    connection: usize,
) -> Result<(Reply, Option<ServeError>), ServeError> {
    let shared = Arc::clone(shared);
    tokio::task::spawn_blocking(move || decide(&shared, request, connection))
        .await
        .map_err(|_| ServeError::ThreadFailure("request decision task failed".into()))?
}

/// h2 连接任务交给连接循环执行的动作；只有连接循环在驱动连接，帧的顺序由它保证。
enum Control {
    /// 发 `GOAWAY(NO_ERROR)` 优雅关闭，再结束该流。h2 每次轮询连接都先写出待发的 GOAWAY，
    /// 再编码各流排队的帧，所以客户端先处理 GOAWAY、后读到流结束，此后不会在这条连接上
    /// 开新流。若结束流在前，客户端可能在 GOAWAY 到达前就在这条连接上发出下一个请求。
    GracefulThenEnd(h2::SendStream<Bytes>),
    /// 先把已排队的帧交给传输，再以 `RST_STREAM(INTERNAL_ERROR)` 重置该流。h2 的重置会
    /// 丢弃该流尚未编码的 HEADERS/DATA，直接重置就可能让客户端只看到请求被重置。
    ResetAfterFlush(h2::SendStream<Bytes>),
    /// 在 h2 库发出任何别的帧之前，插入最后流号小于该请求的 GOAWAY，然后断开连接。
    GoawayUnprocessed {
        last_stream_id: u32,
        respond: SendResponse<Bytes>,
    },
}

async fn serve_h2(
    tcp: TcpStream,
    acceptor: TlsAcceptor,
    connection: usize,
    shared: Arc<Shared>,
) -> Result<(), ServeError> {
    let Ok(tls) = acceptor.accept(tcp).await else {
        shared.state.0.lock().unwrap().tls_rejections += 1;
        return Ok(());
    };
    if tls.get_ref().1.alpn_protocol() != Some(b"h2") {
        return Err(ServeError::ThreadFailure(
            "client did not negotiate h2 over ALPN".into(),
        ));
    }
    let tap = Tap::new(tls);
    // 产品关闭连接、重置或握手中途放弃都是对端行为，不是夹具失败。
    let Ok(mut h2) = h2::server::handshake(tap.clone()).await else {
        return Ok(());
    };
    let (control, mut controls) = mpsc::unbounded_channel();
    let spawn_request = |next: Option<Result<_, h2::Error>>| match next {
        Some(Ok((request, respond))) => {
            spawn_tracked(
                &shared,
                h2_request(
                    request,
                    respond,
                    connection,
                    Arc::clone(&shared),
                    control.clone(),
                ),
            );
            true
        }
        Some(Err(_)) | None => false,
    };
    loop {
        tokio::select! {
            next = h2.accept() => if !spawn_request(next) {
                return Ok(());
            },
            Some(action) = controls.recv() => match action {
                Control::GracefulThenEnd(mut send) => {
                    h2.graceful_shutdown();
                    let _ = send.send_data(Bytes::new(), true);
                }
                Control::ResetAfterFlush(mut send) => {
                    // 只有本任务驱动连接：轮询一次，让 h2 把该流已排队的帧编码进传输
                    // （`Tap` 总是整块接收），之后的重置只能作用于流本身。
                    let next = std::future::poll_fn(|cx| match h2.poll_accept(cx) {
                        Poll::Pending => Poll::Ready(None),
                        Poll::Ready(next) => Poll::Ready(Some(next)),
                    })
                    .await;
                    send.send_reset(Reason::INTERNAL_ERROR);
                    if let Some(next) = next
                        && !spawn_request(next)
                    {
                        return Ok(());
                    }
                }
                Control::GoawayUnprocessed { last_stream_id, respond } => {
                    tap.inject(&goaway_frame(last_stream_id)).await?;
                    drop(h2);
                    drop(respond);
                    return Ok(());
                }
            },
        }
    }
}

/// `GOAWAY(last_stream_id, NO_ERROR)`，RFC 9113 §6.8。
fn goaway_frame(last_stream_id: u32) -> [u8; 17] {
    let mut frame = [0u8; 17];
    frame[2] = 8; // 负载长度
    frame[3] = 0x7; // GOAWAY
    frame[9..13].copy_from_slice(&(last_stream_id & 0x7fff_ffff).to_be_bytes());
    frame
}

async fn h2_request(
    request: http::Request<h2::RecvStream>,
    mut respond: SendResponse<Bytes>,
    connection: usize,
    shared: Arc<Shared>,
    control: mpsc::UnboundedSender<Control>,
) -> Result<(), ServeError> {
    let (parts, mut body) = request.into_parts();
    let mut bytes = Vec::new();
    while let Some(chunk) = body.data().await {
        let Ok(chunk) = chunk else {
            return Ok(());
        };
        let _ = body.flow_control().release_capacity(chunk.len());
        bytes.extend_from_slice(&chunk);
    }
    let path = parts
        .uri
        .path_and_query()
        .map_or("/", |path| path.as_str())
        .to_string();
    let headers = parts.headers.iter().filter_map(|(name, value)| {
        Some((name.as_str().to_string(), value.to_str().ok()?.to_string()))
    });
    let request = HttpRequest::new(parts.method.as_str(), path, headers, &bytes);
    let (reply, failure) = match decide_blocking(&shared, request, connection).await {
        Ok(decision) => decision,
        Err(error) => {
            respond.send_reset(Reason::INTERNAL_ERROR);
            return Err(error);
        }
    };
    if !shared.pass_whole_response().await? {
        respond.send_reset(Reason::CANCEL);
        return Ok(());
    }
    // 发送失败都来自对端已放弃本流或连接，不是夹具失败。
    match reply {
        Reply::Status {
            status,
            content_type,
            headers,
            body,
        } => {
            let mut response = http::Response::builder()
                .status(status)
                .header("content-type", content_type);
            for (name, value) in headers {
                response = response.header(name, value);
            }
            let response = response.body(()).expect("valid fixture response");
            if let Ok(mut send) = respond.send_response(response, body.is_empty())
                && !body.is_empty()
            {
                let _ = send.send_data(Bytes::from(body), true);
            }
        }
        Reply::Sse { frames, end } => {
            let response = http::Response::builder()
                .status(200)
                .header("content-type", "text/event-stream")
                .header("cache-control", "no-cache")
                .body(())
                .expect("valid fixture response");
            let Ok(mut send) = respond.send_response(response, false) else {
                return Ok(());
            };
            for frame in frames {
                if !shared.pass_frame().await? {
                    send.send_reset(Reason::CANCEL);
                    return Ok(());
                }
                if send.send_data(Bytes::from(frame), false).is_err() {
                    return Ok(());
                }
            }
            match end {
                SseEnd::Complete | SseEnd::CleanEof => {
                    let _ = send.send_data(Bytes::new(), true);
                }
                SseEnd::CloseThenComplete => {
                    let _ = control.send(Control::GracefulThenEnd(send));
                }
                SseEnd::Reset => {
                    let _ = control.send(Control::ResetAfterFlush(send));
                }
            }
        }
        Reply::GoawayUnprocessed => {
            let last_stream_id = u32::from(respond.stream_id()).saturating_sub(2);
            let _ = control.send(Control::GoawayUnprocessed {
                last_stream_id,
                respond,
            });
        }
        Reply::RefusedStream => respond.send_reset(Reason::REFUSED_STREAM),
    }
    failure.map_or(Ok(()), Err)
}

/// h2 连接的传输。写入先进本地缓冲、flush 时才发出，所以缓冲里总是 h2 库写完的整帧；
/// [`Tap::inject`] 借此在帧之间插入 h2 库不会发的帧（最后流号小于在途请求的 GOAWAY）。
#[derive(Clone)]
struct Tap(Arc<Mutex<TapInner>>);

struct TapInner {
    io: tokio_rustls::server::TlsStream<TcpStream>,
    pending: Vec<u8>,
}

impl Tap {
    fn new(io: tokio_rustls::server::TlsStream<TcpStream>) -> Self {
        Self(Arc::new(Mutex::new(TapInner {
            io,
            pending: Vec::new(),
        })))
    }

    /// 只能由驱动 h2 连接的任务调用，此时 h2 库不在写帧。
    async fn inject(&self, frame: &[u8]) -> std::io::Result<()> {
        self.0.lock().unwrap().pending.extend_from_slice(frame);
        self.clone().flush().await
    }
}

impl AsyncRead for Tap {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.0.lock().unwrap().io).poll_read(cx, buf)
    }
}

impl AsyncWrite for Tap {
    fn poll_write(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        self.0.lock().unwrap().pending.extend_from_slice(buf);
        Poll::Ready(Ok(buf.len()))
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        let mut guard = self.0.lock().unwrap();
        let inner = &mut *guard;
        while !inner.pending.is_empty() {
            let written = ready!(Pin::new(&mut inner.io).poll_write(cx, &inner.pending))?;
            if written == 0 {
                return Poll::Ready(Err(std::io::ErrorKind::WriteZero.into()));
            }
            inner.pending.drain(..written);
        }
        Pin::new(&mut inner.io).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        ready!(self.as_mut().poll_flush(cx))?;
        Pin::new(&mut self.0.lock().unwrap().io).poll_shutdown(cx)
    }
}

/// 明文 HTTP/1.1：同一条连接上依次处理请求，完整响应后保持连接供产品复用。
async fn serve_h1(
    tcp: TcpStream,
    connection: usize,
    shared: Arc<Shared>,
) -> Result<(), ServeError> {
    const IDLE_LIMIT: Duration = Duration::from_secs(5);
    let mut stream = BufReader::new(tcp);
    loop {
        // 等下一条请求；对端关闭、服务关停或空闲过久时安静地结束连接。
        tokio::select! {
            ready = stream.fill_buf() => match ready {
                Ok(buffer) if !buffer.is_empty() => {}
                _ => return Ok(()),
            },
            _ = tokio::time::sleep(IDLE_LIMIT) => return Ok(()),
        }
        let Some((request, client_close)) = read_h1_request(&mut stream).await? else {
            return Ok(());
        };
        let (reply, failure) = decide_blocking(&shared, request, connection).await?;
        if !shared.pass_whole_response().await? {
            return Ok(());
        }
        let keep = match reply {
            Reply::Status {
                status,
                content_type,
                headers,
                body,
            } => {
                let extra: String = headers
                    .iter()
                    .map(|(name, value)| format!("{name}: {value}\r\n"))
                    .collect();
                let head = format!(
                    "HTTP/1.1 {status} {}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\n{extra}\r\n",
                    if status == 200 { "OK" } else { "Error" },
                    body.len()
                );
                write_h1(&mut stream, &[head, body]).await
            }
            Reply::Sse {
                frames,
                end: SseEnd::Complete,
            } => {
                // 与本地推理服务一样用分块编码流式输出。结尾的零长度块稍后单独发出：
                // 客户端读到结束事件时响应体往往还没结束，要读到它连接才能回到连接池。
                let head = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nTransfer-Encoding: chunked\r\n\r\n".to_string();
                if !write_h1(&mut stream, &[head]).await {
                    return Ok(());
                }
                for frame in frames {
                    let chunk = format!("{:x}\r\n{frame}\r\n", frame.len());
                    if !shared.pass_frame().await? || !write_h1(&mut stream, &[chunk]).await {
                        return Ok(());
                    }
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
                write_h1(&mut stream, &["0\r\n\r\n".to_string()]).await
            }
            _ => {
                let head = "HTTP/1.1 500 Error\r\nContent-Length: 0\r\n\r\n".to_string();
                write_h1(&mut stream, &[head]).await;
                return Err(ServeError::UnsupportedScript("http/1.1"));
            }
        };
        if let Some(failure) = failure {
            return Err(failure);
        }
        if !keep || client_close {
            return Ok(());
        }
    }
}

/// 写出并 flush；失败只说明对端已关闭连接。
async fn write_h1(stream: &mut BufReader<TcpStream>, parts: &[String]) -> bool {
    for part in parts {
        if stream.get_mut().write_all(part.as_bytes()).await.is_err() {
            return false;
        }
    }
    stream.get_mut().flush().await.is_ok()
}

/// 第二项表示客户端要求响应后关闭连接；`None` 表示对端在请求行之前关闭。
async fn read_h1_request(
    stream: &mut BufReader<TcpStream>,
) -> Result<Option<(HttpRequest, bool)>, ServeError> {
    let mut line = String::new();
    if stream.read_line(&mut line).await? == 0 {
        return Ok(None);
    }
    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_string();
    let path = parts.next().unwrap_or_default().to_string();
    let (mut length, mut close, mut headers) = (0, false, Vec::new());
    loop {
        line.clear();
        if stream.read_line(&mut line).await? == 0 || line.trim().is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            let (name, value) = (name.to_ascii_lowercase(), value.trim().to_string());
            match name.as_str() {
                "content-length" => length = value.parse().unwrap_or(0),
                "connection" => close = value.eq_ignore_ascii_case("close"),
                _ => {}
            }
            headers.push((name, value));
        }
    }
    let mut body = vec![0; length];
    stream.read_exact(&mut body).await?;
    Ok(Some((
        HttpRequest::new(&method, path, headers, &body),
        close,
    )))
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
        ..Default::default()
    }
}

/// 按真实 provider 的工具定义校验拒绝请求；违反者返回 400，整个 Turn 无法开始。
///
/// - OpenAI、Anthropic：参数 schema 顶层必须是 `type: object`，且不能出现
///   `oneOf`、`anyOf`、`allOf`、`enum`、`not`。
/// - Gemini：`functionDeclarations[].parameters` 只接受 OpenAPI 子集，任意层级出现
///   `additionalProperties`、`const`、`contains`、`minContains` 都会被拒（2026-10-05 对
///   AI Studio 与 Vertex 实测）；完整 JSON Schema 须放在 `parametersJsonSchema`。
fn tool_schema_rejection(protocol: Protocol, body: &str) -> Option<String> {
    let raw: Value = serde_json::from_str(body).ok()?;
    let tools = raw["tools"].as_array().into_iter().flatten();
    if protocol == Protocol::Gemini {
        return tools
            .flat_map(|tool| {
                tool["functionDeclarations"]
                    .as_array()
                    .into_iter()
                    .flatten()
            })
            .find_map(|declaration| {
                let keyword = ["additionalProperties", "const", "contains", "minContains"]
                    .into_iter()
                    .find(|key| has_key_anywhere(&declaration["parameters"], key))?;
                Some(format!(
                    "Invalid JSON payload received. Unknown name \"{keyword}\" in parameters of \
                     function {}: Cannot find field.",
                    declaration["name"]
                ))
            });
    }
    tools.into_iter().find_map(|tool| {
        let (name, schema) = match protocol {
            Protocol::OpenAi => (&tool["function"]["name"], &tool["function"]["parameters"]),
            _ => (&tool["name"], &tool["input_schema"]),
        };
        let combinator = ["oneOf", "anyOf", "allOf", "enum", "not"]
            .into_iter()
            .find(|key| schema.get(key).is_some());
        (schema["type"] != "object" || combinator.is_some()).then(|| {
            format!(
                "invalid schema for function {name}: schema must have type 'object' and not have \
                 'oneOf'/'anyOf'/'allOf'/'enum'/'not' at the top level"
            )
        })
    })
}

fn has_key_anywhere(value: &Value, key: &str) -> bool {
    match value {
        Value::Object(map) => {
            map.contains_key(key) || map.values().any(|nested| has_key_anywhere(nested, key))
        }
        Value::Array(items) => items.iter().any(|nested| has_key_anywhere(nested, key)),
        _ => false,
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
            // No terminal chunk, no [DONE] — the stream simply ends
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
        SseScript::HttpError { .. }
        | SseScript::HttpResponse { .. }
        | SseScript::CloseAfter { .. }
        | SseScript::Truncated { .. }
        | SseScript::GoawayUnprocessed
        | SseScript::RefusedStream => {
            // 这些脚本都在 `decide` 中先行处理；
            // reaching this arm is a harness bug.
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
            // Cycle 与 Sequence 一样在 `decide` 里按请求解析；
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

    #[test]
    fn render_scripts_preserve_expected_sse_shape() {
        let rows: [(_, _, &[&str], &[&str], Option<&str>); 4] = [
            // The product framer expects content chunks, finish_reason and usage,
            // then the [DONE] marker for a plain-text response.
            (
                "plain_text",
                SseScript::PlainText {
                    chunks: vec!["Hello".into(), " world".into()],
                    finish_reason: "stop".into(),
                },
                &[
                    "data: {",
                    "\"finish_reason\":\"stop\"",
                    "\"usage\"",
                    "\"total_tokens\"",
                ],
                &[],
                Some("data: [DONE]\n\n"),
            ),
            // Tool calls emit id + name, an arguments delta, then tool_calls finish.
            (
                "tool_call",
                SseScript::ToolCall {
                    call_id: "call_abc".into(),
                    tool_name: "observe__workspace".into(),
                    arguments_json: "{}".into(),
                },
                &[
                    "\"id\":\"call_abc\"",
                    "\"name\":\"observe__workspace\"",
                    "\"arguments\":\"{}\"",
                    "\"finish_reason\":\"tool_calls\"",
                    "\"usage\"",
                    "\"total_tokens\":2",
                ],
                &[],
                Some("data: [DONE]\n\n"),
            ),
            // Disconnect truncates the stream without a terminal chunk or [DONE].
            (
                "disconnect",
                SseScript::Disconnect {
                    chunks_before_drop: vec!["partial".into()],
                },
                &["\"content\":\"partial\""],
                &["\"finish_reason\":\"", "[DONE]"],
                None,
            ),
            // The non-JSON data payload exercises the product chunk parser's error path.
            (
                "illegal_response",
                SseScript::IllegalResponse {
                    body: "not json at all".into(),
                },
                &["data: not json at all\n\n"],
                &[],
                None,
            ),
        ];
        for (name, script, required_substrings, forbidden_substrings, required_suffix) in rows {
            let sse = render_script(&script);
            for substring in required_substrings {
                assert!(sse.contains(substring), "{name}: missing {substring:?}");
            }
            for substring in forbidden_substrings {
                assert!(!sse.contains(substring), "{name}: unexpected {substring:?}");
            }
            if let Some(suffix) = required_suffix {
                assert!(sse.ends_with(suffix), "{name}: missing suffix {suffix:?}");
            }
        }
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

    /// 自测中每次等待夹具的上限；夹具回归为不响应时测试失败而不是挂住。
    async fn bounded<T>(future: impl Future<Output = T>) -> T {
        tokio::time::timeout(Duration::from_secs(5), future)
            .await
            .expect("fake provider did not respond within the fixture bound")
    }
    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap()
    }

    /// 像产品一样经 TLS + ALPN `h2` 连接，只信任 harness CA。
    async fn connect(addr: SocketAddr) -> h2::client::SendRequest<Bytes> {
        use tokio_rustls::rustls;
        let mut roots = rustls::RootCertStore::empty();
        roots
            .add(rustls::pki_types::CertificateDer::from(
                TlsIdentity::shared().unwrap().ca_der().to_vec(),
            ))
            .unwrap();
        let mut config = rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::aws_lc_rs::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_root_certificates(roots)
        .with_no_client_auth();
        config.alpn_protocols = vec![b"h2".to_vec()];
        let tcp = bounded(TcpStream::connect(addr)).await.unwrap();
        let name = rustls::pki_types::ServerName::try_from("127.0.0.1").unwrap();
        let tls = bounded(tokio_rustls::TlsConnector::from(Arc::new(config)).connect(name, tcp))
            .await
            .unwrap();
        let (client, connection) = bounded(h2::client::handshake(tls)).await.unwrap();
        tokio::spawn(async move {
            let _ = connection.await;
        });
        client
    }

    /// 发一条请求；`Err` 表示服务端重置了流或关闭了连接。
    async fn send(
        client: &h2::client::SendRequest<Bytes>,
        method: &str,
        path: &str,
        headers: &[(&str, &str)],
        body: &str,
    ) -> Result<http::Response<h2::RecvStream>, h2::Error> {
        let mut request = http::Request::builder()
            .method(method)
            .uri(format!("https://127.0.0.1{path}"));
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        let mut client = bounded(client.clone().ready()).await?;
        let (response, mut stream) =
            client.send_request(request.body(()).unwrap(), body.is_empty())?;
        if !body.is_empty() {
            stream.send_data(Bytes::from(body.to_owned()), true)?;
        }
        bounded(response).await
    }

    async fn read_all(response: http::Response<h2::RecvStream>) -> Result<String, h2::Error> {
        let mut body = response.into_body();
        let mut out = Vec::new();
        while let Some(chunk) = bounded(body.data()).await {
            let chunk = chunk?;
            let _ = body.flow_control().release_capacity(chunk.len());
            out.extend_from_slice(&chunk);
        }
        Ok(String::from_utf8(out).unwrap())
    }

    /// 新开一条连接发一个模型请求，返回状态码与正文。
    async fn exchange(
        addr: SocketAddr,
        headers: &[(&str, &str)],
        body: &str,
    ) -> Result<(u16, String), h2::Error> {
        let client = connect(addr).await;
        let response = send(&client, "POST", "/chat/completions", headers, body).await?;
        let status = response.status().as_u16();
        Ok((status, read_all(response).await?))
    }

    /// The fake server accepts a real HTTP/2 Chat Completions request,
    /// captures it, and returns the script's SSE. This is the server tracer
    /// bullet: it proves the bind/TLS/h2/parse/serve path works and that the
    /// product's framer-compatible framing is on the wire.
    #[test]
    fn server_captures_request_and_returns_plain_text_sse() {
        let mut server = FakeLlmServer::start(SseScript::plain("hi")).expect("start server");
        let body = serde_json::json!({
            "model": "compat:test",
            "messages": [{"role": "user", "content": "ping"}],
            "stream": true
        })
        .to_string();
        let (content_type, text) = runtime().block_on(async {
            let client = connect(server.local_addr()).await;
            let response = send(&client, "POST", "/chat/completions", &[], &body)
                .await
                .unwrap();
            assert_eq!(response.status(), 200);
            let content_type = response.headers()["content-type"]
                .to_str()
                .unwrap()
                .to_owned();
            (content_type, read_all(response).await.unwrap())
        });
        assert_eq!(content_type, "text/event-stream");
        assert!(text.contains("data: {"));
        assert!(text.contains("data: [DONE]"));
        let captured = server.captured_requests();
        assert_eq!(captured.len(), 1);
        assert_eq!(captured[0].model, "compat:test");
        assert_eq!(captured[0].message_roles, vec!["user"]);
        server.shutdown(Duration::from_secs(2)).unwrap();
    }

    #[test]
    fn expected_api_key_rejects_foreign_credentials_without_capturing_them() {
        let mut server = FakeLlmServer::start(SseScript::plain("ok")).unwrap();
        server.expect_api_key("expected-fixture-key");
        let runtime = runtime();
        for (key, status) in [("foreign-fixture-key", 401), ("expected-fixture-key", 200)] {
            let authorization = format!("Bearer {key}");
            let (got, text) = runtime
                .block_on(exchange(
                    server.local_addr(),
                    &[("authorization", &authorization)],
                    "{}",
                ))
                .unwrap();
            assert_eq!(got, status);
            assert!(!text.contains(key));
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
        let runtime = runtime();
        let mut clients = Vec::new();
        for index in 0..REQUESTS {
            let addr = server.local_addr();
            clients.push(runtime.spawn(async move {
                let model = format!("request-{index}");
                let body = format!(r#"{{"model":"{model}","messages":[],"stream":true}}"#);
                let (_, text) = exchange(addr, &[], &body).await.unwrap();
                (model, text)
            }));
            server
                .wait_for_requests(index + 1, Duration::from_secs(2))
                .expect("capture in launch order");
        }

        let capture_order = server.captured_requests();
        gate.release(Duration::from_secs(2))
            .expect("bounded concurrent release");
        let responses: HashMap<_, _> = runtime.block_on(async {
            let mut responses = HashMap::new();
            for client in clients {
                let (model, text) = bounded(client).await.expect("client task");
                responses.insert(model, text);
            }
            responses
        });
        for (sequence_index, captured) in capture_order.iter().enumerate() {
            let response = responses.get(&captured.model).expect("response by model");
            let expected = format!(r#""content":"sequence-{sequence_index}""#);
            assert!(
                response.contains(&expected),
                "capture position {sequence_index} ({}) received the wrong Sequence script",
                captured.model
            );
        }
    }

    #[test]
    fn gated_response_waits_for_release_and_shutdown_joins_handlers() {
        let runtime = runtime();
        let (server, gate) = FakeLlmServer::start_gated(SseScript::plain("gated")).expect("server");
        let addr = server.local_addr();
        let response = runtime.spawn(async move { exchange(addr, &[], "{}").await });
        server
            .wait_for_requests(1, Duration::from_secs(2))
            .expect("captured request");
        gate.wait_until_stalled(1, Duration::from_secs(2))
            .expect("handler reached gate");
        assert!(!response.is_finished());
        gate.release(Duration::from_secs(2))
            .expect("bounded release acknowledgement");
        let (_, text) = runtime
            .block_on(async { tokio::time::timeout(Duration::from_secs(2), response).await })
            .expect("bounded response")
            .unwrap()
            .expect("response");
        assert!(text.contains("gated"));

        let (mut server, gate) =
            FakeLlmServer::start_gated(SseScript::plain("cancelled")).expect("server");
        let addr = server.local_addr();
        let response = runtime.spawn(async move {
            let client = connect(addr).await;
            send(&client, "POST", "/chat/completions", &[], "{}").await
        });
        server
            .wait_for_requests(1, Duration::from_secs(2))
            .expect("captured request");
        gate.wait_until_stalled(1, Duration::from_secs(2))
            .expect("handler reached cancellable gate");
        server
            .shutdown(Duration::from_secs(2))
            .expect("bounded shutdown");
        let outcome = runtime
            .block_on(async { tokio::time::timeout(Duration::from_secs(2), response).await })
            .expect("cancelled handler joined")
            .unwrap();
        assert!(outcome.is_err(), "cancelled response must not be answered");

        assert!(
            server.serve_handle.is_none(),
            "serve thread handle was not joined"
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
        for cancel in [false, true] {
            let script = SseScript::PlainText {
                chunks: vec!["old-token".into(), "新-token".into()],
                finish_reason: "stop".into(),
            };
            let (mut server, gate) =
                FakeLlmServer::start_segmented(script, Duration::from_secs(5)).unwrap();
            let runtime = runtime();
            let mut body = runtime.block_on(async {
                let client = connect(server.local_addr()).await;
                send(&client, "POST", "/chat/completions", &[], "{}")
                    .await
                    .unwrap()
                    .into_body()
            });
            for (index, token) in ["old-token", "新-token"].into_iter().enumerate() {
                gate.wait_until_stalled(index + 1, Duration::from_secs(2))
                    .unwrap();
                let early = runtime.block_on(async {
                    tokio::time::timeout(Duration::from_millis(100), body.data()).await
                });
                assert!(early.is_err(), "frame {index} arrived before its release");
                gate.release_next(Duration::from_secs(2)).unwrap();
                let frame = runtime.block_on(bounded(body.data())).unwrap().unwrap();
                let frame = String::from_utf8(frame.to_vec()).unwrap();
                assert!(frame.contains(token) && !frame.contains("\"finish_reason\":\"stop\""));
                assert!(frame.ends_with("\n\n"));
            }
            gate.wait_until_stalled(3, Duration::from_secs(2)).unwrap();
            if cancel {
                server.shutdown(Duration::from_secs(2)).unwrap();
            } else {
                gate.release(Duration::from_secs(2)).unwrap();
            }
            // 正常放行时读到 EOF；取消时只能是流被重置或连接关闭，不能先读到终态再出错。
            let mut terminal = String::new();
            let mut cut = None;
            loop {
                match runtime.block_on(bounded(body.data())) {
                    Some(Ok(chunk)) => terminal.push_str(std::str::from_utf8(&chunk).unwrap()),
                    Some(Err(error)) => {
                        cut = Some(error);
                        break;
                    }
                    None => break,
                }
            }
            assert_eq!(cut.is_some(), cancel, "terminal read ended with {cut:?}");
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
            assert_eq!(server.state.0.lock().unwrap().active_handlers, 0);
        }
    }

    #[test]
    fn segmented_delivery_deadline_fails_and_joins_without_release() {
        let (mut server, gate) =
            FakeLlmServer::start_segmented(SseScript::plain("withheld"), Duration::ZERO).unwrap();
        let outcome = runtime().block_on(exchange(server.local_addr(), &[], "{}"));
        assert!(!matches!(outcome, Ok((_, ref text)) if text.contains("withheld")));
        assert!(gate.release_next(Duration::from_secs(2)).is_err());
        let error = server.shutdown(Duration::from_secs(2)).unwrap_err();
        assert!(error.to_string().contains("segment"));
        assert_eq!(server.state.0.lock().unwrap().active_handlers, 0);
    }

    /// 只有模型请求进入 capture；其他请求不能被静默接受，必须成为夹具失败。
    #[test]
    fn non_model_request_is_a_fixture_failure_and_not_captured() {
        let mut server = FakeLlmServer::start(SseScript::plain("unused")).unwrap();
        let status = runtime().block_on(async {
            let client = connect(server.local_addr()).await;
            send(&client, "GET", "/models", &[], "")
                .await
                .unwrap()
                .status()
        });
        assert_eq!(status, 404);
        assert!(server.captured_requests().is_empty());
        let error = server.shutdown(Duration::from_secs(2)).unwrap_err();
        assert!(
            matches!(error, ServeError::ThreadFailure(ref failure) if failure == "not a model request: GET /models"),
            "unexpected shutdown error: {error}"
        );
    }

    /// 客户端停在 TLS 握手中途时 shutdown 仍能及时结束；第一次 shutdown 超时后，
    /// 再次调用会继续等待并 join 服务线程，而不是直接报告成功。
    #[test]
    fn shutdown_retries_after_timeout_and_cancels_stalled_handshakes() {
        let mut server = FakeLlmServer::start(SseScript::plain("unused")).unwrap();
        let stalled = std::net::TcpStream::connect(server.local_addr()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while server.state.0.lock().unwrap().active_handlers == 0 {
            assert!(Instant::now() < deadline, "handshake task never started");
            thread::sleep(Duration::from_millis(5));
        }
        assert!(matches!(
            server.shutdown(Duration::ZERO),
            Err(ServeError::Timeout { .. })
        ));
        assert!(
            server.serve_handle.is_some(),
            "timed-out shutdown kept the thread"
        );
        let started = Instant::now();
        server.shutdown(Duration::from_secs(2)).unwrap();
        assert!(started.elapsed() < Duration::from_secs(2));
        assert!(
            server.serve_handle.is_none(),
            "retry joined the serve thread"
        );
        assert_eq!(server.state.0.lock().unwrap().active_handlers, 0);
        drop(stalled);
    }
    #[test]
    fn shutdown_returns_fixture_failure() {
        let log_directory = tempfile::tempdir().expect("log directory");
        let mut server =
            FakeLlmServer::start_logged(SseScript::plain("unused"), log_directory.path())
                .expect("server");
        let outcome = runtime().block_on(exchange(server.local_addr(), &[], "{}"));
        assert!(outcome.is_err(), "failed handler must not send a response");

        let error = server
            .shutdown(Duration::from_secs(2))
            .expect_err("shutdown must return the fixture failure");
        assert!(
            matches!(error, ServeError::ThreadFailure(ref failure) if failure.starts_with("io:")),
            "unexpected shutdown error: {error}"
        );
    }
}
