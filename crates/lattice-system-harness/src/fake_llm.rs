// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 供测试框架场景使用的本地假模型服务。
//!
//! 场景经 [`ScenarioWorld::wire_provider`](crate::ScenarioWorld::wire_provider)
//! 把模型与凭据接到假服务端，由请求路由选择已实现协议的响应格式。
//!
//! 传输与官方 provider 一致：TLS + HTTP/2，ALPN 只提供 `h2`，证书来自
//! [`TlsIdentity`]，场景经 `SSL_CERT_FILE` 信任它（见 `ScenarioWorld`）。
//! [`FakeLlmServer::start_plain_http1`] 另起明文 HTTP/1.1，只服务本地 provider
//! （如 Ollama）场景，只支持完整响应，空闲连接五秒后关闭。传输层故障在 HTTP/2 上构造：
//! 流在结束事件前正常结束、`RST_STREAM(INTERNAL_ERROR)`、响应事件之后的 `GOAWAY(NO_ERROR)`、
//! 最后流号小于在途请求的 `GOAWAY`，以及 `REFUSED_STREAM`。
//!
//! 依据 #41 的第四项决定，服务端做两件事：
//!
//! 1. 捕获每个模型请求（模型、消息、工具名、识别头与连接），供场景断言产品发出了什么。
//! 2. 按场景选定的脚本回复：正文、工具调用、HTTP 错误或传输故障。SSE 正文与真实流一致：
//!    每个分片一条 `data: {chunk}\n\n`，OpenAI 协议以 `data: [DONE]\n\n` 结束。
//!
//! 唯一的客户端是被测产品，所以服务端只做提取路由、凭据与请求体所需的最少解析。它对场景保证：
//!
//! 1. 计数准确：只有发往五种协议模型端点的请求进入捕获；其他请求得到错误响应，记为夹具失败。
//!    工具定义按 `tool_schema_rejection` 的规则核对，违反时返回 400 并记为夹具失败，该请求
//!    不进入捕获。非 JSON 的请求体保留为诊断。对端放弃流或连接、客户端在 TLS 握手中拒绝
//!    证书属于对端行为，握手被拒的次数由 [`FakeLlmServer::rejected_handshakes`] 给出。
//! 2. 回复确定：捕获、`Sequence` 的选择与推进、到达通知在同一临界区内发生；门闩只控制交付时机。
//!    请求数超过 `Sequence` 长度时重复最后一项；空 `Sequence` 产生 `IllegalResponse`。
//! 3. 失败有界：请求等待、门闩阻塞、放行与 `shutdown` 都是有界握手。`shutdown` 发出停止信号，同时
//!    中止停在握手或读请求中的连接任务，并取消门闩，等在途任务结束后再汇合服务线程；超时返回
//!    错误并保留线程，再次调用或 `Drop` 时继续有界等待。保留第一个夹具失败，`shutdown` 在
//!    有界清理后返回它。

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

/// 默认场景 API 密钥，由 provider 接线入口录入，也作为产物脱敏扫描的已知秘密。
pub const FAKE_API_KEY: &str = "harness-fake-key-do-not-ship";

/// 按协议捕获模型请求，原生 JSON 留给场景核对。
///
/// 仅把测试框架需要断言的字段做成强类型；其余字段保留在通用的 `raw` JSON 值中，
/// 以免未知的 provider 扩展字段破坏解析。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct CapturedRequest {
    pub model: String,
    /// 按消息到达顺序提取的 `role` 序列。
    pub message_roles: Vec<String>,
    /// 原样捕获请求体中的线上工具名。
    pub tool_names: Vec<String>,
    /// 请求协议：`openai`、`responses`、`interactions`、`anthropic` 或 `gemini`。
    pub provider: String,
    /// 完整解码后的请求体 JSON，供未单独提取的字段（如 `temperature`、`max_tokens`）做断言。
    pub raw: Value,
    /// 标识客户端的请求头（`user-agent`、`accept`、`x-client-request-id`），名称小写。
    pub headers: BTreeMap<String, String>,
    /// 承载本请求的 TCP 连接序号，按服务端接受顺序从 0 起；同号表示复用了同一条连接。
    pub connection: usize,
    /// 服务端启动到收到本请求的毫秒数，用于断言两次请求之间的等待。
    pub received_ms: u64,
}

/// 场景指定的 provider 原生用量分项；Messages 协议的开始与末尾报告分别编排。
#[derive(Debug, Clone)]
pub enum NativeUsage {
    ChatCompletions(Value),
    Responses(Value),
    Interactions(Value),
    Messages { start: Value, delta: Value },
    GenerateContent(Value),
}

/// 假服务端针对请求返回的 SSE 或错误脚本类型。
#[derive(Debug, Clone)]
pub enum SseScript {
    /// 保留内层文本或工具脚本，只替换对应协议原生的 `usage` 事件。
    WithUsage {
        script: Box<SseScript>,
        usage: NativeUsage,
    },
    /// 手工编排的合法 OpenAI 分片，保持跨类型到达顺序；结尾自动追加 `[DONE]`。
    Chunks { chunks: Vec<Value> },
    /// 原生 SSE 事件按序交付；不自动追加任何终止标记。
    NativeEvents {
        events: Vec<(Option<String>, Value)>,
    },
    /// 纯助手文本：发出一或多个 `delta.content` 分片，随后发出带
    /// `finish_reason: "stop"` 与 `usage` 的终态分片及 `data: [DONE]`。
    PlainText {
        chunks: Vec<String>,
        finish_reason: String,
    },
    /// 单次工具调用：发出 `tool_calls` 起始分片（调用编号与名称）、参数增量分片，
    /// 随后发出带 `finish_reason: "tool_calls"` 的终态分片。
    ToolCall {
        call_id: String,
        tool_name: String,
        arguments_json: String,
    },
    /// 同一条助手消息中的多个工具调用，保持绑定顺序。
    ToolCalls {
        calls: Vec<(String, String, String)>,
    },
    /// 返回非 2xx 状态码与 JSON 错误正文的非 SSE 响应（不含 `text/event-stream`），
    /// 模拟 provider 起始错误（如 429/500），触发产品的重试或错误路径。
    HttpError { status: u16, body: String },
    /// 带指定 `Content-Type` 与额外响应头的非 2xx 响应，用于 `retry-after`、HTML 网关错误页等形状。
    HttpResponse {
        status: u16,
        content_type: String,
        headers: Vec<(String, String)>,
        body: String,
    },
    /// 交付内层脚本的事件后先发 `GOAWAY(NO_ERROR)` 优雅关闭连接，再结束流，模拟 provider
    /// 回收连接。只用于 HTTP/2。
    CloseAfter { script: Box<SseScript> },
    /// 发出 `chunks_before_drop` 后正常结束流（`END_STREAM`），不含终态分片与 `[DONE]`：
    /// 产品看到的是流在结束事件之前结束。只用于 HTTP/2。
    Disconnect { chunks_before_drop: Vec<String> },
    /// 与 `Disconnect` 相同，但以 `RST_STREAM(INTERNAL_ERROR)` 结束：HTTP 层看到的是
    /// 响应体读到一半出错，而不是正常结束。只用于 HTTP/2。
    Truncated { chunks_before_drop: Vec<String> },
    /// 收到请求后发 `GOAWAY(NO_ERROR)` 并关闭连接，`GOAWAY` 的最后流号小于本请求：
    /// 按 RFC 9113 §6.8，客户端可确知本请求未被处理。只用于 HTTP/2。
    GoawayUnprocessed,
    /// 以 `RST_STREAM(REFUSED_STREAM)` 拒绝本请求（RFC 9113 §8.7：未被处理）。只用于 HTTP/2。
    RefusedStream,
    /// 返回 HTTP 200 且仍带 `text/event-stream` 以走通分帧器，但数据不符合 OpenAI
    /// 分片 JSON 结构（如纯字符串），用于验证解析器的错误路径。
    IllegalResponse { body: String },
    /// 按顺序将每个内层脚本依次交付给单次请求；超出序列长度的请求重复最后一个脚本。
    /// 这是本地直连多轮工具场景的基础（骨架见 #62 / Codeberg #40，扩展见 #46 / Codeberg #24）：
    /// 首次请求返回工具调用，二次请求返回纯文本，使调用工具的轮次仍能干净收尾。
    Sequence { scripts: Vec<SseScript> },
    /// 按请求轮转内层脚本，走完最后一个后回到第一个。`Sequence` 会
    /// 收敛到最后一个脚本，`Cycle` 则对后续每个请求重复整套模式，因此
    /// 同一个 TUI 会话里的每一轮次都能重演相同的失败或成功形状。
    Cycle { scripts: Vec<SseScript> },
}

impl SseScript {
    /// 便捷构造：以 `stop` 结束的单分片纯文本轮次。
    pub fn plain(text: impl Into<String>) -> Self {
        SseScript::PlainText {
            chunks: vec![text.into()],
            finish_reason: "stop".to_string(),
        }
    }
}

/// 有界的假 provider 协议或生命周期失败。
#[derive(Debug, Error)]
pub enum ServeError {
    #[error("I/O 错误：{0}")]
    Io(#[from] std::io::Error),
    #[error("等待{operation}超时（上限 {timeout:?}）")]
    Timeout {
        operation: &'static str,
        timeout: Duration,
    },
    #[error("非模型请求：{0}")]
    UnexpectedRequest(String),
    #[error("provider 会拒绝工具定义：{0}")]
    RejectedToolSchema(String),
    #[error("provider 会拒绝助手正文：{0}")]
    RejectedAssistantContent(String),
    #[error("脚本无法在 `{0}` 协议下渲染")]
    UnsupportedScript(&'static str),
    #[error("假 provider 服务线程失败：{0}")]
    ThreadFailure(String),
}

fn timeout_error(operation: &'static str, duration: Duration) -> ServeError {
    ServeError::Timeout {
        operation,
        timeout: duration,
    }
}

/// 控制假 provider 何时放行整份响应或下一帧 SSE 的同步门闩。
#[derive(Clone)]
pub struct ResponseGate {
    state: Arc<(Mutex<GateState>, Condvar)>,
    /// 分段交付共用一个场景期限；普通整份响应门闩保持原生命周期。
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
                    "响应门闩在达到预期阻塞数量前已被取消".into(),
                ));
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(timeout_error("假 provider 响应阻塞就位", timeout));
            }
            let (next, result) = wake.wait_timeout(state, remaining).unwrap();
            state = next;
            if result.timed_out() && state.stalled < count {
                return Err(timeout_error("假 provider 响应阻塞就位", timeout));
            }
        }
        Ok(())
    }

    pub fn release(&self, timeout: Duration) -> Result<(), ServeError> {
        self.release_through(true, timeout)
    }

    /// 只放行下一帧 SSE，并等待处理线程越过该门闩；不会顺带放行终态。
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
                return Err(ServeError::ThreadFailure("响应门闩已取消".into()));
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(timeout_error("假 provider 响应放行", timeout));
            }
            let (next, result) = wake.wait_timeout(state, remaining).unwrap();
            state = next;
            if result.timed_out() && state.departed < target {
                return Err(timeout_error("假 provider 响应放行", timeout));
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
                        "假 provider 分段交付已超过截止期限".into(),
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

    /// 在阻塞线程上等待门闩，不占用异步运行时的工作线程。
    async fn pass(&self) -> Result<bool, ServeError> {
        let gate = self.clone();
        tokio::task::spawn_blocking(move || gate.wait())
            .await
            .map_err(|_| ServeError::ThreadFailure("门闩等待任务失败".into()))?
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

    /// 整份响应门闩：返回 `false` 表示门闩已取消，不再响应。
    async fn pass_whole_response(&self) -> Result<bool, ServeError> {
        match &self.gate {
            Some(gate) if !self.segmented() => gate.pass().await,
            _ => Ok(true),
        }
    }

    /// 分段门闩：每帧 SSE 前等待一次。
    async fn pass_frame(&self) -> Result<bool, ServeError> {
        match &self.gate {
            Some(gate) if self.segmented() => gate.pass().await,
            _ => Ok(true),
        }
    }
}

/// 绑定在场景本地端口上运行的假模型服务端。
///
/// 显式 `shutdown` 会报告服务端失败且只等待到截止期限；`Drop` 使用固定的有界兜底等待，
/// 取消所有响应门闩并关闭监听器。在 `Drop` 前已捕获的请求始终可读。
pub struct FakeLlmServer {
    addr: SocketAddr,
    plain_http1: bool,
    state: Arc<(Mutex<ServerState>, Condvar)>,
    stop: watch::Sender<bool>,
    serve_handle: Option<JoinHandle<()>>,
    gate: Option<ResponseGate>,
}

impl FakeLlmServer {
    /// 在 `127.0.0.1:0`（操作系统分配端口）上绑定 TLS + HTTP/2 服务端，
    /// 使用测试框架共享的 [`TlsIdentity`] 按 `script` 提供响应。
    pub fn start(script: SseScript) -> Result<Self, ServeError> {
        Self::start_inner(script, None, None, Some(TlsIdentity::shared()?))
    }

    /// 明文 HTTP/1.1，只用于本地 provider 场景；只支持完整的流式响应与 HTTP 错误响应。
    pub fn start_plain_http1(script: SseScript) -> Result<Self, ServeError> {
        Self::start_inner(script, None, None, None)
    }

    /// 启动服务端并将每次请求追加写入跨崩溃保留的计数文件。
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

    /// 用指定的 TLS 身份启动带门闩的服务端，供需要不同信任根的场景使用。
    pub fn start_gated_with(
        script: SseScript,
        identity: &TlsIdentity,
    ) -> Result<(Self, ResponseGate), ServeError> {
        Self::start_controlled(script, None, identity)
    }

    /// 每帧 SSE 单独放行；全部交付受同一绝对期限约束，`shutdown`/`Drop` 仍会取消门闩。
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
                    Err(_) => state.record_failure("服务线程发生 panic"),
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

    /// 产品应连接的基础地址（不含末尾斜杠）。
    /// HTTP/2 用 `https://127.0.0.1`（证书含该 IP）；明文 HTTP/1.1 用本地 provider
    /// 常见的 `http://localhost`。
    pub fn base_url(&self) -> String {
        if self.plain_http1 {
            format!("http://localhost:{}", self.addr.port())
        } else {
            format!("https://127.0.0.1:{}", self.addr.port())
        }
    }

    /// 仅在首个请求前设置预期测试凭据；只比较内存字节，不写入请求捕获或错误正文。
    pub fn expect_api_key(&self, key: &str) {
        let mut state = self.state.0.lock().unwrap();
        assert!(state.requests.is_empty() && state.active_handlers == 0);
        state.expected_api_key = Some(key.to_string());
    }

    /// 按到达顺序返回目前已捕获请求的快照。
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
                    "服务线程在捕获到预期数量的请求前已退出".into(),
                ));
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(timeout_error("假 provider 收到请求", timeout));
            }
            let (next, result) = wake.wait_timeout(state, remaining).unwrap();
            state = next;
            if result.timed_out() && state.requests.len() < count {
                return Err(timeout_error("假 provider 收到请求", timeout));
            }
        }
        Ok(())
    }

    /// 发出停止信号并有界等待服务线程收尾。超时返回错误但保留线程句柄，
    /// 再次调用（或 `Drop`）会继续等待，直到线程真正汇合。
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
                    return Err(timeout_error("假 provider 关停", timeout));
                }
                let (next, result) = wake.wait_timeout(state, remaining).unwrap();
                state = next;
                if result.timed_out() && (!state.serve_exited || state.active_handlers != 0) {
                    return Err(timeout_error("假 provider 关停", timeout));
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
                .record_failure("服务线程汇合失败");
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
        // `Drop` 无法返回生命周期错误。显式调用 `shutdown` 会保留首个夹具失败；
        // 这里的兜底逻辑只保证等待时间有界。
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
    // 被门闩卡住的任务已由 `shutdown` 取消门闩放行，其余任务观察到 `stop` 后退出。
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
        state.record_failure("连接处理任务在关停前未结束");
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

/// 计入在途任务；任务失败或 panic 记为夹具失败。`shutdown` 发出停止信号时中止任务，
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
            Err(error) if error.is_panic() => state.record_failure("连接处理任务发生 panic"),
            Err(_) => {}
        }
        state.active_handlers -= 1;
        wake.notify_all();
    });
}

/// 等到 `shutdown` 发出停止信号。单独封装是为了避免把 `watch` 的读锁守卫留在 `select!` 的输出里。
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

/// 来自被测产品的一条请求。产品是假 provider 的唯一客户端，因此解析器只提取路由、凭据、
/// 客户端识别头与正文，不对畸形或恶意的 HTTP 报文做额外防御。
struct HttpRequest {
    method: String,
    path: String,
    /// 小写名称的 `authorization`、`x-api-key` 与 `x-goog-api-key`。
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

/// 由请求路由决定的单次模型请求流式协议。
#[derive(Clone, Copy, PartialEq, Eq)]
enum Protocol {
    OpenAi,
    Responses,
    Interactions,
    Anthropic,
    Gemini,
}

impl Protocol {
    /// 若命中模型路由，返回协议及路径中携带的模型名称。
    fn route(method: &str, path: &str) -> Option<(Self, Option<String>)> {
        if method != "POST" {
            return None;
        }
        match path {
            "/chat/completions" => Some((Self::OpenAi, None)),
            "/responses" => Some((Self::Responses, None)),
            "/interactions" | "/v1beta/interactions" => Some((Self::Interactions, None)),
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
            Self::Responses => "responses",
            Self::Interactions => "interactions",
            Self::Anthropic => "anthropic",
            Self::Gemini => "gemini",
        }
    }

    /// 提取该协议对应凭据请求头中的 API 密钥。
    fn api_key(self, request: &HttpRequest) -> Option<&str> {
        let (header, prefix) = match self {
            Self::OpenAi | Self::Responses => ("authorization", "Bearer "),
            Self::Anthropic => ("x-api-key", ""),
            Self::Gemini | Self::Interactions => ("x-goog-api-key", ""),
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

/// SSE 帧发完之后的流结束方式。
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

/// 路由、核对凭据、登记捕获记录并按脚本决定回应。第二项是回应之后要报告的夹具失败。
/// 会写请求日志并调用 `fsync`，由调用方放在阻塞线程上执行。
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
    if let Some(rejection) = responses_assistant_content_rejection(protocol, &request.body) {
        let body = serde_json::json!({"error": {
            "message": rejection,
            "type": "invalid_request_error",
            "code": "invalid_value",
        }});
        let reply = Reply::Status {
            status: 400,
            content_type: "application/json".into(),
            headers: Vec::new(),
            body: body.to_string(),
        };
        return Ok((reply, Some(ServeError::RejectedAssistantContent(rejection))));
    }
    let mut captured = parse_request_body(protocol, path_model, &request.body);
    captured.headers = request.headers;
    captured.connection = connection;
    // 脚本分配与请求捕获发布在同一个临界区内完成，因此即使受门闩阻塞的处理任务
    // 以不同顺序恢复，捕获顺序也始终与 `Sequence` 推进顺序一致。
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
        .map_err(|_| ServeError::ThreadFailure("请求决策任务失败".into()))?
}

/// `h2` 请求任务交给连接循环执行的动作；只有连接循环在驱动连接，帧的顺序由它保证。
enum Control {
    /// 发 `GOAWAY(NO_ERROR)` 优雅关闭，再结束该流。`h2` 每次轮询连接都先写出待发的 `GOAWAY`，
    /// 再编码各流排队的帧，所以客户端先处理 `GOAWAY`、后读到流结束，此后不会在这条连接上
    /// 开新流。若结束流在前，客户端可能在 `GOAWAY` 到达前就在这条连接上发出下一个请求。
    GracefulThenEnd(h2::SendStream<Bytes>),
    /// 先把已排队的帧交给传输，再以 `RST_STREAM(INTERNAL_ERROR)` 重置该流。`h2` 的重置会
    /// 丢弃该流尚未编码的 `HEADERS`/`DATA`，直接重置就可能让客户端只看到请求被重置。
    ResetAfterFlush(h2::SendStream<Bytes>),
    /// 在 `h2` 库发出任何别的帧之前，插入最后流号小于该请求的 `GOAWAY`，然后断开连接。
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
            "客户端未通过 ALPN 协商出 h2".into(),
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
                    // 只有本任务驱动连接：轮询一次，让 `h2` 把该流已排队的帧编码进传输
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

/// 发送 `GOAWAY(last_stream_id, NO_ERROR)` 帧（参见 RFC 9113 §6.8）。
fn goaway_frame(last_stream_id: u32) -> [u8; 17] {
    let mut frame = [0u8; 17];
    frame[2] = 8; // 负载长度
    frame[3] = 0x7; // `GOAWAY` 帧类型
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
            let response = response.body(()).expect("夹具响应头必须合法");
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
                .expect("夹具响应头必须合法");
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

/// `h2` 连接的传输层包装。写入先进本地缓冲、刷新时才发出，所以缓冲里总是 `h2` 库写完的整帧；
/// [`Tap::inject`] 借此在帧之间插入 `h2` 库不会发的帧（最后流号小于在途请求的 `GOAWAY`）。
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

    /// 只能由驱动 `h2` 连接的任务调用，此时 `h2` 库不在写帧。
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

/// 写出并调用 `flush`；失败只说明对端已关闭连接。
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

/// 将模型请求体解码为 [`CapturedRequest`]。若请求体不是合法 JSON，仍会记录一条字段为空的
/// 捕获记录，并把原始字符串存入 `raw["__raw_body"]`，以便场景检查产品实际发出的内容。
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
        Protocol::Responses | Protocol::Interactions => &raw["input"],
        _ => &raw["messages"],
    };
    let message_roles = messages
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|message| {
            if protocol == Protocol::Interactions {
                return match message["type"].as_str() {
                    Some("user_input") => Some("user"),
                    Some("model_output" | "thought" | "function_call") => Some("assistant"),
                    Some("function_result") => Some("tool"),
                    _ => None,
                };
            }
            if protocol == Protocol::Responses {
                match message["type"].as_str() {
                    Some("reasoning" | "function_call") => return Some("assistant"),
                    Some("function_call_output") => return Some("tool"),
                    _ => {}
                }
            }
            message["role"].as_str()
        })
        .map(str::to_string)
        .collect();
    let tools = raw["tools"].as_array().into_iter().flatten();
    let tool_names = match protocol {
        Protocol::OpenAi => tools
            .filter_map(|tool| tool["function"]["name"].as_str())
            .map(str::to_string)
            .collect(),
        Protocol::Anthropic | Protocol::Responses | Protocol::Interactions => tools
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

/// 按真实 provider 的工具定义校验规则拒绝违规请求；违反者返回 400，整轮 Turn 无法开始。
///
/// - Chat Completions、Responses、Anthropic：参数结构顶层必须是 `type: object`，且不能出现
///   `oneOf`、`anyOf`、`allOf`、`enum`、`not`。
/// - Gemini：`functionDeclarations[].parameters` 只接受 OpenAPI 子集，任意层级出现
///   `additionalProperties`、`const`、`contains`、`minContains` 都会被拒（2026-10-05 对
///   AI Studio 与 Vertex 实测）；完整 JSON Schema 须放在 `parametersJsonSchema`。
/// - Interactions：提供 `parameters` 时须为 JSON 对象；不把上游忽略的关键字模拟成拒绝条件。
fn tool_schema_rejection(protocol: Protocol, body: &str) -> Option<String> {
    let raw: Value = serde_json::from_str(body).ok()?;
    let tools = raw["tools"].as_array().into_iter().flatten();
    if protocol == Protocol::Interactions {
        return tools.into_iter().find_map(|tool| {
            tool.get("parameters")
                .filter(|schema| !schema.is_object())
                .map(|_| "Interactions function parameters must be a JSON object".into())
        });
    }
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
            Protocol::Responses => (&tool["name"], &tool["parameters"]),
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

/// Responses 的 `assistant` 正文只接受输出文本或拒绝块，用户输入块不能作为历史回复。
fn responses_assistant_content_rejection(protocol: Protocol, body: &str) -> Option<String> {
    if protocol != Protocol::Responses {
        return None;
    }
    let raw: Value = serde_json::from_str(body).ok()?;
    raw["input"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|item| item["role"] == "assistant")
        .flat_map(|item| item["content"].as_array().into_iter().flatten())
        .find_map(|part| {
            let kind = part["type"].as_str().unwrap_or("null");
            (!matches!(kind, "output_text" | "refusal")).then(|| {
                format!(
                    "Invalid value: '{kind}'. Supported values are: 'output_text' and 'refusal'."
                )
            })
        })
}

/// 单次脚本化工具调用的三元组：`(调用编号, 工具名, 参数 JSON)`。
type ScriptedCall = (String, String, String);

/// 按协议渲染文本、工具与原生用量，保留脚本控制的事件顺序。
fn render(protocol: Protocol, script: &SseScript) -> Option<String> {
    if let SseScript::NativeEvents { events } = script {
        let mut out = String::new();
        for (name, data) in events {
            if let Some(name) = name {
                out.push_str(&format!("event: {name}\n"));
            }
            if protocol == Protocol::Interactions
                && name.as_deref() == Some("done")
                && data.as_str() == Some("[DONE]")
            {
                // 结束标记不是 JSON 字符串，不能在数据外加引号。
                out.push_str("data: [DONE]\n\n");
            } else {
                out.push_str(&format!("data: {data}\n\n"));
            }
        }
        return Some(out);
    }
    if let SseScript::WithUsage { script, usage } = script {
        if !matches!(
            (protocol, usage),
            (Protocol::OpenAi, NativeUsage::ChatCompletions(_))
                | (Protocol::Responses, NativeUsage::Responses(_))
                | (Protocol::Interactions, NativeUsage::Interactions(_))
                | (Protocol::Anthropic, NativeUsage::Messages { .. })
                | (Protocol::Gemini, NativeUsage::GenerateContent(_))
        ) {
            return None;
        }
        let rendered = render(protocol, script)?;
        let mut out = String::new();
        for line in rendered.lines() {
            if let Some(data) = line.strip_prefix("data: ")
                && let Ok(mut root) = serde_json::from_str::<Value>(data)
            {
                match usage {
                    NativeUsage::Interactions(value)
                        if root["interaction"].get("usage").is_some() =>
                    {
                        root["interaction"]["usage"] = value.clone()
                    }
                    NativeUsage::Responses(value) if root["response"].get("usage").is_some() => {
                        root["response"]["usage"] = value.clone()
                    }
                    NativeUsage::ChatCompletions(value) if root.get("usage").is_some() => {
                        root["usage"] = value.clone()
                    }
                    NativeUsage::Messages { start, .. } if root["type"] == "message_start" => {
                        root["message"]["usage"] = start.clone()
                    }
                    NativeUsage::Messages { delta, .. } if root["type"] == "message_delta" => {
                        root["usage"] = delta.clone()
                    }
                    NativeUsage::GenerateContent(value) if root.get("usageMetadata").is_some() => {
                        root["usageMetadata"] = value.clone()
                    }
                    _ => {}
                }
                out.push_str(&format!("data: {root}\n"));
            } else {
                out.push_str(line);
                out.push('\n');
            }
        }
        return Some(out);
    }
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
        Protocol::Responses => render_responses(texts, &calls),
        Protocol::Interactions => render_interactions(texts, &calls),
        _ => render_gemini(texts, &calls, finish),
    })
}

/// 普通脚本也走原生 `step` 生命周期，工具轮以 `requires_action` 结束。
fn render_interactions(texts: &[String], calls: &[ScriptedCall]) -> String {
    use serde_json::json;
    let mut events = vec![
        json!({"event_type":"interaction.created","interaction":{"object":"interaction","model":"fixture-model"}}),
        json!({"event_type":"interaction.status_update","status":"in_progress"}),
    ];
    let mut index = 0;
    if !texts.is_empty() {
        events
            .push(json!({"event_type":"step.start","index":index,"step":{"type":"model_output"}}));
        for text in texts {
            events.push(json!({"event_type":"step.delta","index":index,"delta":{"type":"text","text":text}}));
        }
        events.push(json!({"event_type":"step.stop","index":index}));
        index += 1;
    }
    for (id, name, arguments) in calls {
        events.push(json!({"event_type":"step.start","index":index,"step":{"type":"function_call","id":id,"name":name,"arguments":{}}}));
        events.push(json!({"event_type":"step.delta","index":index,"delta":{"type":"arguments_delta","arguments":arguments}}));
        events.push(json!({"event_type":"step.stop","index":index}));
        index += 1;
    }
    events.push(json!({"event_type":"interaction.completed","interaction":{"status":if calls.is_empty() {"completed"} else {"requires_action"},"usage":{"total_tokens":12,"total_input_tokens":8,"total_output_tokens":4,"total_cached_tokens":0,"total_thought_tokens":0,"total_tool_use_tokens":0},"created":"2026-11-02T13:51:51Z","updated":"2026-11-02T13:51:51Z","service_tier":"standard","object":"interaction","model":"fixture-model"}}));
    let mut out = String::from(": keep-alive\n\n");
    for event in events {
        out.push_str(&format!(
            "event: {}\ndata: {event}\n\n",
            event["event_type"].as_str().unwrap()
        ));
    }
    out.push_str("event: done\ndata: [DONE]\n\n");
    out
}

/// Responses 使用 `item` 生命周期与完整终态，普通脚本也不依赖 Chat Completions 的 `[DONE]` 标记。
fn render_responses(texts: &[String], calls: &[ScriptedCall]) -> String {
    use serde_json::json;
    let mut out = String::new();
    let mut output = Vec::new();
    let mut event = |value: Value| push_data(&mut out, &value);
    event(json!({"type":"response.created","response":{"status":"in_progress","output":[]}}));
    if !texts.is_empty() {
        event(
            json!({"type":"response.output_item.added","output_index":0,"item":{"type":"message","id":"msg_0","role":"assistant","status":"in_progress","phase":"final_answer","content":[]}}),
        );
        event(
            json!({"type":"response.content_part.added","output_index":0,"item_id":"msg_0","content_index":0,"part":{"type":"output_text","text":"","annotations":[],"logprobs":[]}}),
        );
        for text in texts {
            event(
                json!({"type":"response.output_text.delta","output_index":0,"item_id":"msg_0","content_index":0,"delta":text}),
            );
        }
        event(
            json!({"type":"response.output_text.done","output_index":0,"item_id":"msg_0","content_index":0,"text":texts.concat()}),
        );
        let item = json!({"type":"message","id":"msg_0","role":"assistant","status":"completed","phase":"final_answer","content":[{"type":"output_text","text":texts.concat(),"annotations":[],"logprobs":[]}]});
        event(json!({"type":"response.output_item.done","output_index":0,"item":item}));
        output.push(item);
    }
    for (id, name, arguments) in calls {
        let index = output.len();
        let item_id = format!("item_{index}");
        let mut item =
            json!({"type":"function_call","id":item_id,"call_id":id,"name":name,"arguments":""});
        event(json!({"type":"response.output_item.added","output_index":index,"item":item}));
        event(
            json!({"type":"response.function_call_arguments.delta","output_index":index,"item_id":item_id,"delta":arguments}),
        );
        event(
            json!({"type":"response.function_call_arguments.done","output_index":index,"item_id":item_id,"arguments":arguments}),
        );
        item["arguments"] = arguments.clone().into();
        event(json!({"type":"response.output_item.done","output_index":index,"item":item}));
        output.push(item);
    }
    event(
        json!({"type":"response.completed","response":{"status":"completed","output":output,"usage":{"input_tokens":1,"output_tokens":texts.len().max(calls.len())}}}),
    );
    out
}

/// Anthropic Messages 流式响应：`message_start`、一个文本块、每次调用一个
/// `tool_use` 块，以及带停止原因的 `message_delta`。
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

/// Gemini `streamGenerateContent?alt=sse`：每个文本片段或 `functionCall`
/// 各发一个分片，最后发出带 `finishReason` 与用量的终态分片。
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

/// 将 [`SseScript`] 渲染为服务端在 HTTP 响应头之后写出的原始 SSE 文本。
/// 每个分片格式为 `data: {json}\n\n`；带内正常结束的流以 `data: [DONE]\n\n` 收尾。
/// 分片 JSON 与产品 `OpenAiSseParser` 预期的 OpenAI Chat Completions 流式结构一致
///（`choices[0].delta.content`、`choices[0].delta.tool_calls`、`choices[0].finish_reason`、`usage`）。
fn render_script(script: &SseScript) -> String {
    let mut out = String::new();
    match script {
        SseScript::WithUsage { .. } | SseScript::NativeEvents { .. } => {
            return render(Protocol::OpenAi, script)
                .expect("OpenAI 渲染路径下的脚本必须使用 Chat Completions 用量格式");
        }
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
            // 不发终态分片与 `[DONE]`——流在在途分片之后直接结束，
            // 产品应将其视为截断的流。
        }
        SseScript::IllegalResponse { body } => {
            // `data: <body>\n\n`——分帧器会将其作为一条数据事件接收，
            // 随后分片解析器会将其作为畸形 JSON 拒绝。
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
            // 这些脚本都在 `decide` 中先行处理；走到该分支说明测试框架有缺陷。
            out.push_str("data: fake_llm: unresolved HttpError script\n\n");
            out.push_str("data: [DONE]\n\n");
        }
        SseScript::Sequence { .. } => {
            // `Sequence` 会在渲染前由 `decide` 按请求展开；走到该分支说明测试框架有缺陷。
            // 发出畸形载荷，使轮次明确报错而不是静默通过。
            out.push_str("data: fake_llm: unresolved Sequence script\n\n");
            out.push_str("data: [DONE]\n\n");
        }
        SseScript::Cycle { .. } => {
            // `Cycle` 与 `Sequence` 一样在 `decide` 里按请求展开；走到该分支说明测试框架有缺陷。
            // 发出畸形载荷，使轮次明确报错而不是静默通过。
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
        let rows: [(_, _, &[&str], &[&str], Option<&str>); 5] = [
            (
                "native_events",
                SseScript::NativeEvents {
                    events: vec![(
                        Some("step.delta".into()),
                        serde_json::json!({"type":"step.delta","delta":"native"}),
                    )],
                },
                &["event: step.delta\n", "\"delta\":\"native\""],
                &["[DONE]", "response.completed"],
                Some("\n\n"),
            ),
            // 产品分帧器对纯文本响应预期收到正文分片、`finish_reason`、`usage` 以及最后的 `[DONE]` 标记。
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
            // 工具调用依次发出调用编号与名称、参数增量，随后以 `tool_calls` 结束。
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
            // `Disconnect` 直接截断流，不含终态分片或 `[DONE]`。
            (
                "disconnect",
                SseScript::Disconnect {
                    chunks_before_drop: vec!["partial".into()],
                },
                &["\"content\":\"partial\""],
                &["\"finish_reason\":\"", "[DONE]"],
                None,
            ),
            // 非 JSON 数据载荷用于覆盖产品分片解析器的错误路径。
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
                assert!(
                    sse.contains(substring),
                    "{name}：缺少预期子串 {substring:?}"
                );
            }
            for substring in forbidden_substrings {
                assert!(
                    !sse.contains(substring),
                    "{name}：出现不应包含的子串 {substring:?}"
                );
            }
            if let Some(suffix) = required_suffix {
                assert!(sse.ends_with(suffix), "{name}：缺少预期后缀 {suffix:?}");
            }
        }
    }

    /// 解码 Chat Completions 请求体应提取出模型名、按到达顺序排列的消息角色以及线上工具名。
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

    #[test]
    fn responses_native_route_capture_schema_and_usage() {
        use serde_json::json;
        assert!(Protocol::route("POST", "/responses") == Some((Protocol::Responses, None)));
        let body = json!({"model":"model","input":[{"role":"user","content":[]},{"type":"reasoning"},{"type":"function_call"},{"type":"function_call_output"}],"tools":[{"type":"function","name":"observe__workspace","parameters":{"type":"object"},"strict":false}]});
        let captured = parse_request_body(Protocol::Responses, None, &body.to_string());
        assert_eq!(captured.provider, "responses");
        assert_eq!(
            captured.message_roles,
            vec!["user", "assistant", "assistant", "tool"]
        );
        assert_eq!(captured.tool_names, vec!["observe__workspace"]);
        assert_eq!(captured.raw, body);
        assert!(tool_schema_rejection(Protocol::Responses, &body.to_string()).is_none());
        let mut invalid = body;
        invalid["tools"][0]["parameters"] = json!({"type":"array"});
        assert!(tool_schema_rejection(Protocol::Responses, &invalid.to_string()).is_some());
        let mut assistant_body = json!({"input":[
            {"role":"user","content":[{"type":"input_text","text":"question"}]},
            {"type":"message","role":"assistant","content":[
                {"type":"output_text","text":"answer","annotations":[]},
                {"type":"refusal","refusal":"declined"}
            ]}
        ]});
        assert!(
            responses_assistant_content_rejection(Protocol::Responses, &assistant_body.to_string())
                .is_none()
        );
        assistant_body["input"][1]["content"][0]["type"] = json!("input_text");
        assert_eq!(
            responses_assistant_content_rejection(Protocol::Responses, &assistant_body.to_string()),
            Some(
                "Invalid value: 'input_text'. Supported values are: 'output_text' and 'refusal'."
                    .into()
            )
        );
        let script = SseScript::WithUsage {
            script: Box::new(SseScript::plain("hello")),
            usage: NativeUsage::Responses(json!({"input_tokens":99})),
        };
        let sse = render(Protocol::Responses, &script).unwrap();
        assert!(sse.contains("\"input_tokens\":99"));
        assert!(sse.contains("response.completed"));
        assert!(!sse.contains("[DONE]"));
        for protocol in [
            Protocol::OpenAi,
            Protocol::Responses,
            Protocol::Anthropic,
            Protocol::Gemini,
        ] {
            let events = vec![(None, json!({"event_type":"custom.event"}))];
            assert_eq!(
                render(protocol, &SseScript::NativeEvents { events }).unwrap(),
                "data: {\"event_type\":\"custom.event\"}\n\n"
            );
        }
    }

    #[test]
    fn interactions_capture_schema_and_native_rendering_follow_current_reference() {
        use serde_json::json;
        for path in ["/interactions", "/v1beta/interactions"] {
            assert!(Protocol::route("POST", path) == Some((Protocol::Interactions, None)));
        }
        let mut body = json!({"model":"model","input":[{"type":"user_input"},{"type":"thought"},{"type":"model_output"},{"type":"function_call"},{"type":"function_result"}],"tools":[{"type":"function","name":"observe__workspace","parameters":{"type":"object"}}]});
        let captured = parse_request_body(Protocol::Interactions, None, &body.to_string());
        assert_eq!(
            captured.message_roles,
            ["user", "assistant", "assistant", "assistant", "tool"]
        );
        assert_eq!(captured.tool_names, ["observe__workspace"]);
        assert_eq!(captured.raw, body);
        for keyword in [
            "minLength",
            "maxLength",
            "minProperties",
            "maxProperties",
            "uniqueItems",
            "contains",
            "minContains",
            "maxContains",
            "additionalProperties",
            "const",
            "oneOf",
            "anyOf",
            "allOf",
            "enum",
            "not",
        ] {
            body["tools"][0]["parameters"] = json!({"type":"object",keyword:{}});
            assert!(
                tool_schema_rejection(Protocol::Interactions, &body.to_string()).is_none(),
                "{keyword}"
            );
        }
        for parameters in [json!("schema"), json!([]), json!(0), Value::Null] {
            body["tools"][0]["parameters"] = parameters;
            assert!(tool_schema_rejection(Protocol::Interactions, &body.to_string()).is_some());
        }
        body["tools"][0]
            .as_object_mut()
            .unwrap()
            .remove("parameters");
        assert!(tool_schema_rejection(Protocol::Interactions, &body.to_string()).is_none());
        for (script, status) in [
            (SseScript::plain("hello"), "completed"),
            (
                SseScript::ToolCall {
                    call_id: "a".into(),
                    tool_name: "t".into(),
                    arguments_json: "{}".into(),
                },
                "requires_action",
            ),
        ] {
            let rendered = render(
                Protocol::Interactions,
                &SseScript::WithUsage {
                    script: Box::new(script),
                    usage: NativeUsage::Interactions(json!({"total_input_tokens":99})),
                },
            )
            .unwrap();
            assert!(rendered.contains(&format!("\"status\":\"{status}\"")));
            assert!(rendered.contains("event: step.start\n"));
            assert!(rendered.contains("\"total_input_tokens\":99"));
            let events: Vec<Value> = rendered
                .lines()
                .filter_map(|line| line.strip_prefix("data: "))
                .filter(|data| *data != "[DONE]")
                .map(|data| serde_json::from_str(data).unwrap())
                .collect();
            assert_eq!(
                events[0]["interaction"],
                json!({"object":"interaction","model":"fixture-model"})
            );
            assert_eq!(events[1]["event_type"], "interaction.status_update");
            assert_eq!(events[1]["status"], "in_progress");
            let terminal = &events.last().unwrap()["interaction"];
            assert!(terminal.get("id").is_none());
            assert!(terminal.get("steps").is_none());
            assert_eq!(terminal["object"], "interaction");
            assert_eq!(terminal["model"], "fixture-model");
            assert!(rendered.starts_with(": keep-alive\n\n"));
            assert!(rendered.ends_with("event: done\ndata: [DONE]\n\n"));
        }
        assert_eq!(
            render(
                Protocol::Interactions,
                &SseScript::NativeEvents {
                    events: vec![(Some("done".into()), json!("[DONE]"))],
                },
            )
            .unwrap(),
            "event: done\ndata: [DONE]\n\n"
        );
    }

    #[test]
    fn interactions_invalid_parameters_are_fixture_failures_without_capture() {
        use serde_json::json;
        for parameters in [json!("schema"), json!([]), json!(0), Value::Null] {
            let mut server = FakeLlmServer::start(SseScript::plain("unused")).unwrap();
            let body = json!({"model":"model","tools":[{"type":"function","name":"t","parameters":parameters}],"input":"hello"}).to_string();
            let status = runtime().block_on(async {
                let client = connect(server.local_addr()).await;
                let response = send(&client, "POST", "/interactions", &[], &body)
                    .await
                    .unwrap();
                let status = response.status().as_u16();
                read_all(response).await.unwrap();
                status
            });
            assert_eq!(status, 400);
            assert!(server.captured_requests().is_empty());
            let error = server.shutdown(Duration::from_secs(2)).unwrap_err();
            assert!(
                matches!(error, ServeError::ThreadFailure(ref failure) if failure == "provider 会拒绝工具定义：Interactions function parameters must be a JSON object"),
                "关停返回了预期之外的错误：{error}"
            );
        }
    }

    /// 自测中每次等待夹具的上限；夹具回归为不响应时测试失败而不是挂住。
    async fn bounded<T>(future: impl Future<Output = T>) -> T {
        tokio::time::timeout(Duration::from_secs(5), future)
            .await
            .expect("假 provider 未在夹具时限内响应")
    }
    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap()
    }

    /// 像产品一样经 TLS + ALPN `h2` 连接，只信任测试框架 CA。
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

    /// 假服务端接受真实的 HTTP/2 Chat Completions 请求，记录捕获并返回脚本的 SSE。
    /// 这是服务端的曳光弹自测：验证端口绑定、TLS、`h2`、请求解析与响应写出整条链路通畅，
    /// 且线上报文与产品分帧器兼容。
    #[test]
    fn server_captures_request_and_returns_plain_text_sse() {
        let mut server = FakeLlmServer::start(SseScript::plain("hi")).expect("启动假模型服务失败");
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
            .expect("启动带门闩的序列假服务端失败");
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
                .expect("未能按发起顺序捕获请求");
        }

        let capture_order = server.captured_requests();
        gate.release(Duration::from_secs(2))
            .expect("并发放行应在时限内完成");
        let responses: HashMap<_, _> = runtime.block_on(async {
            let mut responses = HashMap::new();
            for client in clients {
                let (model, text) = bounded(client).await.expect("客户端任务执行失败");
                responses.insert(model, text);
            }
            responses
        });
        for (sequence_index, captured) in capture_order.iter().enumerate() {
            let response = responses.get(&captured.model).expect("缺少对应模型的响应");
            let expected = format!(r#""content":"sequence-{sequence_index}""#);
            assert!(
                response.contains(&expected),
                "第 {sequence_index} 个捕获位置（{}）收到了错误的 Sequence 脚本",
                captured.model
            );
        }
    }

    #[test]
    fn gated_response_waits_for_release_and_shutdown_joins_handlers() {
        let runtime = runtime();
        let (server, gate) =
            FakeLlmServer::start_gated(SseScript::plain("gated")).expect("启动假服务端失败");
        let addr = server.local_addr();
        let response = runtime.spawn(async move { exchange(addr, &[], "{}").await });
        server
            .wait_for_requests(1, Duration::from_secs(2))
            .expect("未能捕获请求");
        gate.wait_until_stalled(1, Duration::from_secs(2))
            .expect("处理任务未到达门闩");
        assert!(!response.is_finished());
        gate.release(Duration::from_secs(2))
            .expect("门闩放行确认超时");
        let (_, text) = runtime
            .block_on(async { tokio::time::timeout(Duration::from_secs(2), response).await })
            .expect("等待响应超时")
            .unwrap()
            .expect("读取响应失败");
        assert!(text.contains("gated"));

        let (mut server, gate) =
            FakeLlmServer::start_gated(SseScript::plain("cancelled")).expect("启动假服务端失败");
        let addr = server.local_addr();
        let response = runtime.spawn(async move {
            let client = connect(addr).await;
            send(&client, "POST", "/chat/completions", &[], "{}").await
        });
        server
            .wait_for_requests(1, Duration::from_secs(2))
            .expect("未能捕获请求");
        gate.wait_until_stalled(1, Duration::from_secs(2))
            .expect("处理任务未到达可取消门闩");
        server
            .shutdown(Duration::from_secs(2))
            .expect("有界关停失败");
        let outcome = runtime
            .block_on(async { tokio::time::timeout(Duration::from_secs(2), response).await })
            .expect("被取消的处理任务未及时收尾")
            .unwrap();
        assert!(outcome.is_err(), "被取消的响应不应返回正常应答");

        assert!(server.serve_handle.is_none(), "服务线程句柄尚未汇合");
        let state = server.state.0.lock().unwrap();
        assert!(state.serve_exited, "服务线程未标记已退出");
        assert_eq!(state.active_handlers, 0, "仍有在途处理任务未收尾");
        assert!(
            state.first_failure.is_none(),
            "关停记录了意外的线程失败：{:?}",
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
                assert!(early.is_err(), "第 {index} 帧在放行前提前到达");
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
            assert_eq!(cut.is_some(), cancel, "终态读取结束结果不符合预期：{cut:?}");
            if cancel {
                assert!(terminal.is_empty(), "关停取消后不得继续交付终态帧");
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
        assert!(error.to_string().contains("分段交付"));
        assert_eq!(server.state.0.lock().unwrap().active_handlers, 0);
    }

    /// 只有模型请求进入捕获记录；其他请求不能被静默接受，必须成为夹具失败。
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
            matches!(error, ServeError::ThreadFailure(ref failure) if failure == "非模型请求：GET /models"),
            "关停返回了预期之外的错误：{error}"
        );
    }

    /// 客户端停在 TLS 握手中途时 `shutdown` 仍能及时结束；第一次 `shutdown` 超时后，
    /// 再次调用会继续等待并汇合服务线程，而不是直接报告成功。
    #[test]
    fn shutdown_retries_after_timeout_and_cancels_stalled_handshakes() {
        let mut server = FakeLlmServer::start(SseScript::plain("unused")).unwrap();
        let stalled = std::net::TcpStream::connect(server.local_addr()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while server.state.0.lock().unwrap().active_handlers == 0 {
            assert!(Instant::now() < deadline, "握手处理任务始终未启动");
            thread::sleep(Duration::from_millis(5));
        }
        assert!(matches!(
            server.shutdown(Duration::ZERO),
            Err(ServeError::Timeout { .. })
        ));
        assert!(
            server.serve_handle.is_some(),
            "超时的关停应保留服务线程句柄"
        );
        let started = Instant::now();
        server.shutdown(Duration::from_secs(2)).unwrap();
        assert!(started.elapsed() < Duration::from_secs(2));
        assert!(server.serve_handle.is_none(), "重试关停应完成服务线程汇合");
        assert_eq!(server.state.0.lock().unwrap().active_handlers, 0);
        drop(stalled);
    }
    #[test]
    fn shutdown_returns_fixture_failure() {
        let log_directory = tempfile::tempdir().expect("创建临时目录失败");
        let mut server =
            FakeLlmServer::start_logged(SseScript::plain("unused"), log_directory.path())
                .expect("启动假服务端失败");
        let outcome = runtime().block_on(exchange(server.local_addr(), &[], "{}"));
        assert!(outcome.is_err(), "失败的处理任务不得发送正常响应");

        let error = server
            .shutdown(Duration::from_secs(2))
            .expect_err("关停必须返回已记录的夹具失败");
        assert!(
            matches!(error, ServeError::ThreadFailure(ref failure) if failure.starts_with("I/O 错误：")),
            "关停返回了预期之外的错误：{error}"
        );
    }
}
