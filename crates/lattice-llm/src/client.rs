// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 按协议构造与解析请求，按认证方式附加凭据，并维护 Turn 内连接。

use std::pin::Pin;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

use futures::stream::{self, StreamExt};
use net_guard::NetGuardError;

use crate::auth::Credentials;
use crate::protocol::{self, Features, Protocol};
use crate::provider::Auth;
use crate::reply::{ProtocolEvent, ReplyProtocolError, StreamStop};
use crate::reply_stream::{ProtocolStream, StreamControl, assemble_reply_stream};
use crate::sse;
use crate::stub::{LlmError, LlmStream};
use crate::transport;
use crate::types::ChatRequest;

/// 兜底总时限，与 profile `timeout_secs` 的上限一致；流的实际时限由 Turn deadline 约束。
const HTTP_REQUEST_BACKSTOP: Duration = Duration::from_secs(86_400);

/// Maximum idle time allowed while connecting to, or reading from, a provider.
const HTTP_IO_TIMEOUT: Duration = Duration::from_secs(30);

/// Maximum number of bytes read from a non-success provider response body.
const MAX_ERROR_BODY_SIZE: usize = 64 * 1024;

/// 成功响应是 SSE，错误响应是 JSON；两种都声明，避免按 `Accept` 协商的网关拒绝其一。
const ACCEPT: &str = "text/event-stream, application/json";

static USER_AGENT: LazyLock<String> = LazyLock::new(|| {
    let release = std::fs::read_to_string("/proc/sys/kernel/osrelease").ok();
    transport::user_agent(release.as_deref())
});

type BodyStream = Pin<Box<dyn futures::Stream<Item = reqwest::Result<bytes::Bytes>> + Send>>;

/// 认证与协议独立选择；无认证时不构造任何认证头。
fn build_auth_headers(
    auth: Auth,
    api_key: Option<&str>,
) -> Result<Vec<(&'static str, String)>, LlmError> {
    let name = match auth {
        Auth::Bearer => "Authorization",
        Auth::XApiKey => "x-api-key",
        Auth::XGoogApiKey => "x-goog-api-key",
        Auth::None => return Ok(Vec::new()),
        Auth::OauthChatgpt => {
            return Err(LlmError::Config {
                message: "auth oauth:chatgpt is not implemented".into(),
            });
        }
    };
    let key = api_key.ok_or_else(|| LlmError::Config {
        message: "provider credentials are missing from auth.json".into(),
    })?;
    let value = if auth == Auth::Bearer {
        format!("Bearer {key}")
    } else {
        key.to_owned()
    };
    Ok(vec![(name, value)])
}

/// 每个响应独享解析状态，协议同时决定请求路径、请求体与回复来源。
enum ProtocolParser {
    ChatCompletions(protocol::chat_completions::OpenAiSseParser),
    Responses(protocol::responses::ResponsesSseParser),
    Messages(protocol::anthropic_messages::AnthropicSseParser),
    GenerateContent(protocol::generate_content::GeminiSseParser),
    Interactions(protocol::interactions::InteractionsSseParser),
}

impl ProtocolParser {
    fn new(protocol: Protocol) -> Self {
        match protocol {
            Protocol::ChatCompletions => {
                Self::ChatCompletions(protocol::chat_completions::OpenAiSseParser::new())
            }
            Protocol::Messages => {
                Self::Messages(protocol::anthropic_messages::AnthropicSseParser::new())
            }
            Protocol::GenerateContent => {
                Self::GenerateContent(protocol::generate_content::GeminiSseParser::new())
            }
            Protocol::Responses => Self::Responses(protocol::responses::ResponsesSseParser::new()),
            Protocol::Interactions => {
                Self::Interactions(protocol::interactions::InteractionsSseParser::new())
            }
        }
    }

    fn endpoint_url(&self, base_url: &str, model: &str) -> String {
        match self {
            Self::ChatCompletions(_) => {
                format!("{}/chat/completions", base_url.trim_end_matches('/'))
            }
            Self::Responses(_) => format!("{}/responses", base_url.trim_end_matches('/')),
            Self::Interactions(_) => format!("{}/interactions", base_url.trim_end_matches('/')),
            Self::Messages(_) => format!("{}/v1/messages", base_url.trim_end_matches('/')),
            Self::GenerateContent(_) => {
                protocol::generate_content::build_stream_url(base_url, model)
            }
        }
    }

    fn request_body(
        &self,
        request: &ChatRequest,
        output_limit: Option<u64>,
        model_reasoning: Option<bool>,
        features: Features,
        reasoning: Option<crate::reasoning::ReasoningPlan>,
        origin: &lattice_conversation::ReplayOrigin,
    ) -> Result<serde_json::Value, LlmError> {
        match self {
            Self::ChatCompletions(_) => {
                let Features::ChatCompletions(features) = features else {
                    unreachable!("resolved features match the protocol")
                };
                protocol::chat_completions::build_request_json(
                    request,
                    &request.target.model,
                    features,
                    reasoning,
                    origin,
                )
            }
            Self::Messages(_) => {
                let Features::Messages(features) = features else {
                    unreachable!("resolved features match the protocol")
                };
                return protocol::anthropic_messages::build_request_json(
                    request,
                    &request.target.model,
                    output_limit,
                    features,
                    reasoning,
                    model_reasoning,
                    origin,
                );
            }
            Self::Responses(_) => {
                protocol::responses::build_request_json(request, reasoning, model_reasoning, origin)
            }
            Self::Interactions(_) => protocol::interactions::build_request_json(
                request,
                reasoning,
                model_reasoning,
                origin,
            ),
            Self::GenerateContent(_) => protocol::generate_content::build_request_json(
                request,
                reasoning,
                model_reasoning,
                origin,
            ),
        }
        .map_err(Into::into)
    }

    fn buffer_limit(&self) -> usize {
        match self {
            Self::Responses(_) => 64 * 1024 * 1024,
            Self::Interactions(_) => 16 * 1024 * 1024,
            _ => sse::MAX_SSE_BUFFER_SIZE,
        }
    }

    fn parse_event(&mut self, event_text: &str) -> Vec<ProtocolEvent> {
        let (event_type, data) = parse_sse_event_block(event_text);
        if data.is_empty() {
            return Vec::new();
        }
        match self {
            Self::ChatCompletions(p) => p.parse_chunk(event_type, &data),
            Self::Responses(p) => p.parse_chunk(event_type, &data),
            Self::Interactions(p) => p.parse_chunk(event_type, &data),
            Self::Messages(p) => p.parse_chunk(event_type.unwrap_or(""), &data),
            Self::GenerateContent(p) => p.parse_chunk(event_type, &data),
        }
    }
}

/// Parse a single SSE event text block into (event_type, data) pair.
fn parse_sse_event_block(event_text: &str) -> (Option<&str>, String) {
    let mut event_type: Option<&str> = None;
    let mut data = String::new();

    for line in event_text.lines() {
        let line = line.trim_end_matches('\r');
        if line.starts_with(':') {
            continue; // SSE comment
        }
        if let Some(value) = line.strip_prefix("event:") {
            event_type = Some(value.trim());
        } else if let Some(value) = line.strip_prefix("data:") {
            let value = value.strip_prefix(' ').unwrap_or(value);
            if !data.is_empty() {
                data.push('\n');
            }
            data.push_str(value);
        }
    }

    (event_type, data)
}

/// State carried by the streaming SSE unfold loop.
struct SseStreamState {
    body_stream: Pin<Box<dyn futures::Stream<Item = reqwest::Result<bytes::Bytes>> + Send>>,
    parser: ProtocolParser,
    client: Arc<TurnClient>,
    buffer: Vec<u8>,
    scanned: usize,
    event_count: usize,
    finished: bool,
    pending: std::collections::VecDeque<ProtocolEvent>,
}

/// 将 HTTP 响应中的 SSE 事件逐步解析为 [`ProtocolEvent`]。
///
/// 每个请求独享解析器，避免请求间共享状态。响应体按块读取；
/// 未完成分帧的 SSE 缓冲超过本协议上限，
/// 或事件数超过 `MAX_SSE_EVENTS` 时，发出错误事件并终止流。
fn stream_sse_response(
    body_stream: BodyStream,
    parser: ProtocolParser,
    client: Arc<TurnClient>,
) -> ProtocolStream {
    let state = SseStreamState {
        body_stream,
        parser,
        client,
        buffer: Vec::new(),
        scanned: 0,
        event_count: 0,
        finished: false,
        pending: std::collections::VecDeque::new(),
    };

    let event_stream = stream::unfold(state, |mut state| async move {
        if state.finished && state.pending.is_empty() {
            return None;
        }

        // Drain any pending events from the previous chunk first.
        if let Some(event) = state.pending.pop_front() {
            return Some(deliver(state, event).await);
        }

        // Pull more chunks until we produce at least one event or exhaust
        // the body.
        loop {
            match state.body_stream.next().await {
                Some(Ok(chunk)) => {
                    state.buffer.extend_from_slice(&chunk);
                    if state.buffer.len() > state.parser.buffer_limit() {
                        state.finished = true;
                        return Some((
                            ProtocolEvent::error(
                                format!(
                                    "SSE in-flight buffer size exceeded: {} bytes",
                                    state.buffer.len()
                                ),
                                false,
                            ),
                            state,
                        ));
                    }
                    drain_complete_events(&mut state);
                    if let Some(event) = state.pending.pop_front() {
                        return Some(deliver(state, event).await);
                    }
                }
                Some(Err(e)) => {
                    state.finished = true;
                    return Some((
                        {
                            // 响应已经开始后读失败就是传输中断。reqwest 把这类错误都归为
                            // decode，不能按请求阶段的规则判断，一律可重试。
                            let error = llm_error_from_request(e);
                            ProtocolEvent::error(error.to_string(), true)
                        },
                        state,
                    ));
                }
                None => {
                    // Body exhausted: flush any trailing event.
                    if !state.buffer.is_empty() {
                        let trailing_bytes = std::mem::take(&mut state.buffer);
                        let trailing = String::from_utf8_lossy(&trailing_bytes);
                        if !trailing.trim().is_empty() && state.event_count < sse::MAX_SSE_EVENTS {
                            state.event_count += 1;
                            for event in state.parser.parse_event(&trailing) {
                                state.pending.push_back(event);
                            }
                        }
                    }
                    state.finished = true;
                    return state.pending.pop_front().map(|event| (event, state));
                }
            }
        }
    });

    Box::pin(event_stream)
}

/// 交付 `Done` 之前读完响应体。调用方拿到 `Done` 就丢弃流，而 HTTP/1.1 连接只有在响应体
/// 读到结尾后才回到连接池；分块响应的结尾常晚于最后一个事件到达，不读完同一 Turn 的下一次
/// 请求就得新建连接。provider 在结束事件后随即结束响应体，因此只等很短时间、读很少数据：
/// `Done` 最多推迟 0.5 秒，超出时放弃这条连接。
async fn deliver(
    mut state: SseStreamState,
    event: ProtocolEvent,
) -> (ProtocolEvent, SseStreamState) {
    const TAIL_WAIT: Duration = Duration::from_millis(500);
    const TAIL_BYTES: usize = 64 * 1024;
    if matches!(event, ProtocolEvent::Done { .. }) && !state.finished {
        state.finished = true;
        let body = &mut state.body_stream;
        let complete = tokio::time::timeout(TAIL_WAIT, async {
            let mut read = 0;
            while let Some(chunk) = body.next().await {
                let Ok(chunk) = chunk else {
                    return false;
                };
                read += chunk.len();
                if read > TAIL_BYTES {
                    return false;
                }
            }
            true
        })
        .await
        .unwrap_or(false);
        if !complete {
            state.client.forget();
        }
    }
    (event, state)
}
/// Drain complete SSE events from the byte buffer.
///
/// Recognizes `\n\n`, `\r\n\r\n`, and `\r\r` as event separators per the SSE
/// specification. Decodes each complete event as UTF-8 only after a full
/// boundary is found, preventing corruption of multi-byte characters that span
/// HTTP chunk boundaries.
fn drain_complete_events(state: &mut SseStreamState) {
    loop {
        let sep_len = match find_event_boundary(&state.buffer, &mut state.scanned) {
            Some(v) => v,
            None => return,
        };
        let (idx, slen) = sep_len;
        let event_bytes: Vec<u8> = state.buffer.drain(..idx).collect();
        // Drop the separator.
        state.buffer.drain(..slen);
        state.scanned = 0;

        let event_text = String::from_utf8_lossy(&event_bytes);
        if event_text.trim().is_empty() {
            continue;
        }
        if state.event_count >= sse::MAX_SSE_EVENTS {
            state.pending.push_back(ProtocolEvent::error(
                format!("SSE event count exceeded: {}", sse::MAX_SSE_EVENTS),
                false,
            ));
            state.finished = true;
            return;
        }
        state.event_count += 1;
        for event in state.parser.parse_event(&event_text) {
            state.pending.push_back(event);
        }
    }
}

/// Find the first SSE event boundary in a byte buffer.
/// Returns `Some((offset, separator_length))` or `None`.
/// Recognizes `\r\n\r\n` (4), `\n\n` (2), and `\r\r` (2).
fn find_event_boundary(buf: &[u8], scanned: &mut usize) -> Option<(usize, usize)> {
    let mut i = (*scanned).min(buf.len());
    while i < buf.len() {
        if buf[i] == b'\r' {
            // Check \r\n\r\n first (longest match)
            if i + 3 < buf.len()
                && buf[i + 1] == b'\n'
                && buf[i + 2] == b'\r'
                && buf[i + 3] == b'\n'
            {
                return Some((i, 4));
            }
            // Check \r\r
            if i + 1 < buf.len() && buf[i + 1] == b'\r' {
                return Some((i, 2));
            }
        } else if buf[i] == b'\n' {
            // Check \n\n
            if i + 1 < buf.len() && buf[i + 1] == b'\n' {
                return Some((i, 2));
            }
        }
        i += 1;
    }
    // 最长分隔符为四字节，保留末尾三字节以识别跨 HTTP 块的边界。
    *scanned = buf.len().saturating_sub(3);
    None
}

fn parsed_request_host_port(url: &str) -> Option<(String, u16)> {
    let parsed = url::Url::parse(url).ok()?;
    let host = parsed.host_str()?.to_string();
    let port = parsed.port_or_known_default()?;
    Some((host, port))
}

fn is_localhost_request_host(host: &str) -> bool {
    matches!(host, "localhost" | "127.0.0.1" | "::1" | "[::1]")
}

/// Build the shared provider client configuration.
///
/// Redirects are disabled because every network safety check this crate
/// applies — `validate_base_url`, `host_is_dns_rebinding_domain`, and the
/// `resolve_to_addrs` pinning in [`build_http_client_for_url`] — binds the
/// *first* hop only. reqwest's default policy follows up to 10 hops, so a
/// redirect would reach an unvalidated host with none of those checks, and on
/// 307/308 would replay the request body (the whole conversation, including
/// workspace file contents the agent just read). Provider-specific auth
/// headers make it worse: reqwest strips `Authorization` across hosts but not
/// `x-api-key` (Anthropic) or `x-goog-api-key` (Gemini). `fetch__web` disables
/// redirects for the same reason.
fn http_client_builder() -> reqwest::ClientBuilder {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(HTTP_REQUEST_BACKSTOP)
        .connect_timeout(HTTP_IO_TIMEOUT)
        .read_timeout(HTTP_IO_TIMEOUT)
        .user_agent(USER_AGENT.as_str())
}

fn build_http_client_for_url(
    url: &str,
    builder: reqwest::ClientBuilder,
) -> Result<reqwest::Client, LlmError> {
    pin_http_client(url, builder)
        .map_err(llm_error_from_net_guard)?
        .build()
        .map_err(|_| LlmError::Config {
            message: "failed to build HTTP client".into(),
        })
}

/// 在 blocking 线程上解析并固定公网地址；精确的 loopback 主机沿用本地连接例外。
pub(crate) fn pin_http_client(
    url: &str,
    builder: reqwest::ClientBuilder,
) -> Result<reqwest::ClientBuilder, NetGuardError> {
    let Some((host, port)) = parsed_request_host_port(url) else {
        return Err(NetGuardError::Config {
            message: "invalid HTTP URL".into(),
        });
    };
    if net_guard::host_is_dns_rebinding_domain(&host) {
        return Err(NetGuardError::Config {
            message: "DNS rebinding hostname is not allowed".into(),
        });
    }
    if is_localhost_request_host(&host) {
        Ok(builder)
    } else {
        let pinned = net_guard::resolve_public_socket_addrs(&host, port)?;
        Ok(builder.resolve_to_addrs(host.as_str(), &pinned))
    }
}

/// 本 Turn 的 provider client：解析并校验地址、固定连接目标，并带上协议层重发策略与新建连接日志。
/// DNS 解析是阻塞调用，必须在 blocking 线程上执行。
fn provider_client(
    url: &str,
    roots: Vec<reqwest::Certificate>,
) -> Result<reqwest::Client, LlmError> {
    let host = parsed_request_host_port(url).map_or_else(String::new, |(host, _)| host);
    // 当前 Cargo features 没有 system-proxy；显式禁用防止未来特性合并偷偷改变连接边界。
    let builder = http_client_builder()
        .no_proxy()
        .tls_certs_only(roots)
        .retry(protocol_nack_retry(host))
        .connector_layer(ConnectLog);
    build_http_client_for_url(url, builder)
}

/// 与 reqwest 默认策略的判断相同：只重发服务端明确没有处理的请求（HTTP/2 `GOAWAY(NO_ERROR)`、
/// `REFUSED_STREAM`，RFC 9113 §8.7），最多 2 次。换成自有策略只为每次重发留下 WARN 日志；
/// 这些重发计入同一次尝试。
fn protocol_nack_retry(host: String) -> reqwest::retry::Builder {
    reqwest::retry::for_host(host)
        .no_budget()
        .max_retries_per_request(2)
        .classify_fn(
            |req_rep| match req_rep.error().and_then(unprocessed_by_server) {
                Some(reason) => {
                    tracing::warn!(reason, "provider did not process the request; resending it");
                    req_rep.retryable()
                }
                None => req_rep.success(),
            },
        )
}

fn unprocessed_by_server(error: &(dyn std::error::Error + 'static)) -> Option<&'static str> {
    let mut source = Some(error);
    while let Some(error) = source {
        if let Some(h2) = error.downcast_ref::<h2::Error>() {
            if !h2.is_remote() {
                return None;
            }
            return match h2.reason() {
                Some(h2::Reason::NO_ERROR) if h2.is_go_away() => Some("GOAWAY(NO_ERROR)"),
                Some(h2::Reason::REFUSED_STREAM) if h2.is_reset() => Some("REFUSED_STREAM"),
                _ => None,
            };
        }
        source = error.source();
    }
    None
}

/// 每新建一条 provider 连接记一条 DEBUG。hyper-util 对“复用连接已被服务端关闭、请求尚未发出”
/// 的重发无法关闭也无法挂钩，日志里只能间接表现为本次尝试新建了连接。
#[derive(Clone)]
struct ConnectLog;

impl<S> tower_layer::Layer<S> for ConnectLog {
    type Service = ConnectLogService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        ConnectLogService(inner)
    }
}

#[derive(Clone)]
struct ConnectLogService<S>(S);

impl<S, R> tower_service::Service<R> for ConnectLogService<S>
where
    S: tower_service::Service<R>,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = S::Future;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.0.poll_ready(cx)
    }

    fn call(&mut self, request: R) -> Self::Future {
        tracing::debug!("opening provider connection");
        self.0.call(request)
    }
}

fn llm_error_from_net_guard(error: NetGuardError) -> LlmError {
    match error {
        NetGuardError::Config { message } => LlmError::Config { message },
        NetGuardError::Dns { message } => LlmError::Dns { message },
    }
}

/// 只有连接、发送与读取失败可以重试；构造请求和解码出错重发也不会成功。
fn llm_error_from_request(err: reqwest::Error) -> LlmError {
    let message = err.to_string();
    if err.is_timeout() {
        LlmError::Timeout { message }
    } else {
        LlmError::Transport {
            message,
            retryable: err.is_connect() || err.is_request() || err.is_body(),
        }
    }
}

/// 保留错误种类与重试属性，只替换诊断中的已知凭据。
pub(crate) fn redact_error(error: LlmError, credentials: &Credentials) -> LlmError {
    redact_error_with(error, |message| credentials.redact(message))
}

pub(crate) fn redact_error_with(
    mut error: LlmError,
    redact: impl FnOnce(&str) -> String,
) -> LlmError {
    match &mut error {
        LlmError::Credential { message }
        | LlmError::Config { message }
        | LlmError::Dns { message }
        | LlmError::Transport { message, .. }
        | LlmError::Timeout { message }
        | LlmError::ProviderHttp { message, .. }
        | LlmError::StreamRead { message }
        | LlmError::ProtocolLimit { message }
        | LlmError::Other(message) => *message = redact(message),
        LlmError::Projection(_) => {}
    }
    error
}

fn append_bounded_error_chunk(body: &mut Vec<u8>, chunk: &[u8]) -> bool {
    let remaining = MAX_ERROR_BODY_SIZE.saturating_sub(body.len());
    if chunk.len() <= remaining {
        body.extend_from_slice(chunk);
        false
    } else {
        body.extend_from_slice(&chunk[..remaining]);
        true
    }
}

/// 读最多 64 KiB 错误正文；第二项表示响应体未完整读完。
async fn read_error_body_capped(response: reqwest::Response) -> (String, bool) {
    let mut stream = response.bytes_stream();
    let mut body = Vec::new();

    while let Some(chunk_result) = stream.next().await {
        match chunk_result {
            Ok(chunk) => {
                if append_bounded_error_chunk(&mut body, &chunk) {
                    return (String::from_utf8_lossy(&body).into_owned(), true);
                }
            }
            Err(e) => {
                let mut text = String::from_utf8_lossy(&body).into_owned();
                if !text.is_empty() {
                    text.push_str("; ");
                }
                text.push_str(&format!("failed to read provider error body: {}", e));
                return (text, true);
            }
        }
    }

    (String::from_utf8_lossy(&body).into_owned(), false)
}

/// 地址固定后的 client 只在所属 Turn 与 endpoint 内复用。
#[derive(Default)]
pub(crate) struct TurnClient {
    cached: Mutex<Option<reqwest::Client>>,
}

impl TurnClient {
    pub(crate) fn forget(&self) {
        self.cached.lock().unwrap().take();
    }

    #[cfg(test)]
    pub(crate) fn is_cached(&self) -> bool {
        self.cached.lock().unwrap().is_some()
    }

    async fn get(
        &self,
        url: &str,
        roots: &[reqwest::Certificate],
    ) -> Result<reqwest::Client, LlmError> {
        if let Some(client) = self.cached.lock().unwrap().clone() {
            return Ok(client);
        }
        let (url, roots) = (url.to_owned(), roots.to_vec());
        let client = tokio::task::spawn_blocking(move || provider_client(&url, roots))
            .await
            .map_err(|_| LlmError::Config {
                message: "provider client setup task failed".into(),
            })??;
        *self.cached.lock().unwrap() = Some(client.clone());
        Ok(client)
    }
}

const MAX_TLS_ROOTS: usize = 1024;
const MAX_TLS_ROOT_BYTES: usize = 65_536;
const MAX_TLS_ROOT_TOTAL_BYTES: usize = 4 * 1024 * 1024;

/// 按原顺序保留根证书，直到个数或总字节达到上限。
///
/// 单张超过大小上限的根先跳过，其余先解析，无法解析的根跳过，不计入个数
/// 或总量；每张被跳过的根恰好记一条警告（超大的不再解析，不会重复）。
/// 一张有效根放不下（个数或累计字节将超过上限）时就停止，之后的根全部
/// 不信任，所以保留的总是可用根的有序前缀。
/// 截断只减少信任锚，被截掉的 CA 签发的 HTTPS 请求会失败。
fn select_roots<T, E: std::fmt::Display>(
    roots_der: Vec<Vec<u8>>,
    parse: impl Fn(&[u8]) -> Result<T, E>,
) -> Vec<(Vec<u8>, T)> {
    let mut kept = Vec::new();
    let mut total = 0usize;
    let mut truncated = 0usize;
    let mut roots = roots_der.into_iter();
    while let Some(der) = roots.next() {
        if der.len() > MAX_TLS_ROOT_BYTES {
            tracing::warn!(
                bytes = der.len(),
                "skipping TLS root certificate larger than {MAX_TLS_ROOT_BYTES} bytes"
            );
            continue;
        }
        // 先解析再查上限：无效根不计入个数或总量，也不会触发截断。
        match parse(&der) {
            Ok(parsed) => {
                if kept.len() == MAX_TLS_ROOTS
                    || total.saturating_add(der.len()) > MAX_TLS_ROOT_TOTAL_BYTES
                {
                    truncated = 1 + roots.len();
                    break;
                }
                total += der.len();
                kept.push((der, parsed));
            }
            Err(error) => {
                tracing::warn!(%error, "skipping invalid TLS root certificate");
            }
        }
    }
    if truncated > 0 {
        tracing::warn!(
            kept = kept.len(),
            total,
            truncated,
            "truncated TLS roots to the certificate limits; later roots are not trusted"
        );
    }
    kept
}

/// 先按 rustls 的信任锚规则解析 DER，再交给 reqwest。
///
/// `reqwest::Certificate::from_der` 只保存字节、不解析；无效的根会被保留并占用
/// 名额，直到构建 client 时 rustls 才报 `BadEncoding`，整个 client 无法构建。
/// 这里用 `RootCertStore::add`（构建时 rustls 做的同一项检查）提前拒绝它们。
fn parse_root(der: &[u8]) -> Result<reqwest::Certificate, LlmError> {
    let invalid = |error: &dyn std::fmt::Display| LlmError::Config {
        message: format!("invalid TLS root certificate: {error}"),
    };
    rustls::RootCertStore::empty()
        .add(rustls::pki_types::CertificateDer::from(der))
        .map_err(|error| invalid(&error))?;
    reqwest::Certificate::from_der(der).map_err(|error| invalid(&error))
}

pub(crate) fn load_bounded_roots(
    roots_der: Vec<Vec<u8>>,
) -> (Vec<Vec<u8>>, Vec<reqwest::Certificate>) {
    select_roots(roots_der, parse_root).into_iter().unzip()
}

/// 网关为单次请求提供解析结果，包含该 Turn 对应 endpoint 的 client。
pub(crate) struct RequestEndpoint<'a> {
    pub(crate) provider: &'a str,
    pub(crate) model: &'a str,
    pub(crate) base_url: &'a str,
    pub(crate) protocol: Protocol,
    pub(crate) auth: Auth,
    pub(crate) credentials: Arc<Credentials>,
    pub(crate) chatgpt_auth: &'a crate::auth::ChatgptAuth,
    pub(crate) roots: &'a [reqwest::Certificate],
    pub(crate) client: Arc<TurnClient>,
    pub(crate) output_limit: Option<u64>,
    pub(crate) model_reasoning: Option<bool>,
    pub(crate) features: Features,
    pub(crate) reasoning: Option<crate::reasoning::ReasoningPlan>,
}

pub(crate) fn validate_implementation(auth: Auth) -> Result<(), LlmError> {
    if auth == Auth::OauthChatgpt {
        return Err(LlmError::Config {
            message: "auth oauth:chatgpt is not implemented".into(),
        });
    }
    Ok(())
}

pub(crate) fn validate_configuration(auth: Auth, api_key: Option<&str>) -> Result<(), LlmError> {
    validate_implementation(auth)?;
    build_auth_headers(auth, api_key)?;
    Ok(())
}

pub(crate) async fn dispatch(
    request: ChatRequest,
    endpoint: RequestEndpoint<'_>,
    control: StreamControl,
) -> Result<LlmStream, LlmError> {
    validate_implementation(endpoint.auth)?;
    let access_token = if endpoint.auth == Auth::OauthChatgpt {
        Some(endpoint.chatgpt_auth.access_token().await?)
    } else {
        None
    };
    let credentials = access_token.as_ref().map_or_else(
        || endpoint.credentials.clone(),
        |token| token.credentials.clone(),
    );
    let parser = ProtocolParser::new(endpoint.protocol);
    let headers = if access_token.is_some() {
        Vec::new()
    } else {
        build_auth_headers(endpoint.auth, credentials.api_key(endpoint.provider))?
    };
    let provider = credentials.redact(endpoint.provider);
    let origin = lattice_conversation::ReplayOrigin {
        protocol: endpoint.protocol,
        provider_id: endpoint.provider.into(),
    };
    let url = parser.endpoint_url(endpoint.base_url, endpoint.model);
    let body = parser.request_body(
        &request,
        endpoint.output_limit,
        endpoint.model_reasoning,
        endpoint.features,
        endpoint.reasoning,
        &origin,
    )?;
    let client = endpoint
        .client
        .get(&url, endpoint.roots)
        .await
        .map_err(|error| redact_error(error, &credentials))?;
    let mut req_builder = client
        .post(&url)
        .header(reqwest::header::ACCEPT, ACCEPT)
        .json(&body);
    if endpoint.protocol == Protocol::Messages {
        req_builder = req_builder.header(
            "anthropic-version",
            protocol::anthropic_messages::ANTHROPIC_VERSION,
        );
    }
    if let Some(id) = &request.client_request_id {
        req_builder = req_builder.header("x-client-request-id", id.as_str());
    }
    for (name, value) in headers {
        req_builder = req_builder.header(name, value);
    }
    if let Some(token) = access_token {
        req_builder = req_builder.header(reqwest::header::AUTHORIZATION, token.authorization()?);
    }
    let started = Instant::now();
    let mut guard = ForgetUnlessDone {
        client: endpoint.client.clone(),
        done: false,
    };
    let response = match req_builder.send().await {
        Ok(response) => response,
        Err(error) => {
            let error = redact_error(llm_error_from_request(error), &credentials);
            tracing::warn!(provider, %error, "provider request failed before a response");
            return Err(error);
        }
    };
    let status = response.status();
    let request_id =
        transport::provider_request_id(response.headers()).map(|id| credentials.redact(&id));
    tracing::debug!(
        provider,
        status = status.as_u16(),
        request_id = request_id.as_deref(),
        headers_ms = started.elapsed().as_millis() as u64,
        "provider responded"
    );
    if !status.is_success() {
        let response_headers = response.headers().clone();
        let (body, cut) = read_error_body_capped(response).await;
        if !cut {
            guard.finish();
        }
        let body = credentials.redact(&body);
        let error = transport::status_error(
            status.as_u16(),
            &response_headers,
            &body,
            request_id.as_deref(),
        );
        tracing::warn!(provider, request_id = request_id.as_deref(), %error, "provider returned an error status");
        return Err(error);
    }
    let body_stream = response.bytes_stream();
    let stream = stream_sse_response(Box::pin(body_stream), parser, endpoint.client.clone());
    // provider 的流内错误同样会进入公开诊断和 Turn 日志，必须在越过适配边界前脱敏。
    let input = Box::pin(stream.map(move |event| match event {
        ProtocolEvent::ResponsesError { code, message } => {
            let code = code.map(|code| credentials.redact(&code));
            let message = credentials.redact(&message);
            let failure = transport::responses_error(
                code.as_deref(),
                &message,
                Some(status.as_u16()),
                request_id.as_deref(),
            );
            tracing::warn!(
                provider,
                request_id = request_id.as_deref(),
                error = %failure.message,
                "provider stream error"
            );
            ProtocolEvent::Failed(StreamStop::Provider(failure))
        }
        ProtocolEvent::Failed(StreamStop::Provider(mut failure)) => {
            failure.message = transport::bound_diagnostic(&credentials.redact(&failure.message));
            tracing::warn!(
                provider,
                request_id = request_id.as_deref(),
                error = %failure.message,
                "provider stream error"
            );
            ProtocolEvent::Failed(StreamStop::Provider(failure))
        }
        ProtocolEvent::UnsupportedContentBlock {
            protocol,
            index,
            block_type,
        } => ProtocolEvent::Failed(StreamStop::Protocol(ReplyProtocolError::unsupported(
            protocol,
            index,
            &credentials.redact(&block_type),
        ))),
        ProtocolEvent::Done { finish_reason } => {
            guard.finish();
            tracing::debug!(
                provider,
                request_id = request_id.as_deref(),
                total_ms = started.elapsed().as_millis() as u64,
                "provider stream completed"
            );
            ProtocolEvent::Done { finish_reason }
        }
        other => other,
    }));
    Ok(assemble_reply_stream(Some(origin), input, control))
}

/// 发送请求前开始守护 client；发送失败或被中止、错误正文未读完，以及 SSE 未交付
/// `Done` 就结束、出错或被丢弃时，清空 Turn client，使下次请求重新解析并固定地址。
/// 完整读完错误正文或交付 `Done` 后解除守护。
struct ForgetUnlessDone {
    client: Arc<TurnClient>,
    done: bool,
}

impl ForgetUnlessDone {
    // 经方法设置而不是直接写字段：闭包按字段捕获时只会带走 `done`，guard 本身会提前丢弃。
    fn finish(&mut self) {
        self.done = true;
    }
}
impl Drop for ForgetUnlessDone {
    fn drop(&mut self) {
        if !self.done {
            self.client.forget();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{LlmEvent, ReplyOutcome, ReplyStop};

    fn chat_request() -> ChatRequest {
        let mut request = ChatRequest::new(Vec::new(), crate::ModelTarget::new("test", "model"));
        request.client_request_id = Some("test-session".into());
        request
    }

    #[test]
    fn reasoning_sources_and_feature_values_write_exact_control_parameters() {
        use lattice_conversation::Effort::{High, Low, Medium, Minimal, Off};
        use serde_json::json;
        let effort = json!([{"type":"effort","values":["none","high"]}]);
        let interactions_effort =
            json!([{"type":"effort","values":["minimal","low","medium","high"]}]);
        let toggle = json!([{"type":"toggle"}]);
        let budget = json!([{"type":"budget_tokens"}]);
        let combined_effort = json!([{"type":"toggle"},{"type":"effort","values":["high"]}]);
        let combined_budget = json!([{"type":"toggle"},{"type":"budget_tokens"}]);
        for (protocol, output, overrides, options, selected, expected) in [
            (
                Protocol::Interactions,
                None,
                "",
                interactions_effort.clone(),
                Minimal,
                json!({"generation_config":{"thinking_level":"minimal"}}),
            ),
            (
                Protocol::Interactions,
                None,
                "",
                interactions_effort.clone(),
                Low,
                json!({"generation_config":{"thinking_level":"low"}}),
            ),
            (
                Protocol::Interactions,
                None,
                "",
                interactions_effort.clone(),
                Medium,
                json!({"generation_config":{"thinking_level":"medium"}}),
            ),
            (
                Protocol::Interactions,
                None,
                "",
                interactions_effort,
                High,
                json!({"generation_config":{"thinking_level":"high"}}),
            ),
            (
                Protocol::Responses,
                None,
                "",
                effort.clone(),
                Off,
                json!({"reasoning":{"effort":"none"}}),
            ),
            (
                Protocol::Responses,
                None,
                "",
                effort.clone(),
                High,
                json!({"reasoning":{"effort":"high"}}),
            ),
            (
                Protocol::ChatCompletions,
                None,
                "",
                effort.clone(),
                Off,
                json!({"reasoning_effort":"none"}),
            ),
            (
                Protocol::ChatCompletions,
                None,
                "",
                effort.clone(),
                High,
                json!({"reasoning_effort":"high"}),
            ),
            (
                Protocol::ChatCompletions,
                None,
                "effort_param='reasoning-object'",
                effort.clone(),
                Off,
                json!({"reasoning":{"effort":"none"}}),
            ),
            (
                Protocol::ChatCompletions,
                None,
                "effort_param='reasoning-object'",
                effort.clone(),
                High,
                json!({"reasoning":{"effort":"high"}}),
            ),
            (
                Protocol::ChatCompletions,
                None,
                "thinking_toggle='thinking-type'",
                toggle.clone(),
                Off,
                json!({"thinking":{"type":"disabled"}}),
            ),
            (
                Protocol::ChatCompletions,
                None,
                "thinking_toggle='thinking-type'",
                toggle.clone(),
                High,
                json!({"thinking":{"type":"enabled"}}),
            ),
            (
                Protocol::ChatCompletions,
                None,
                "thinking_toggle='enable-thinking'",
                toggle.clone(),
                Off,
                json!({"enable_thinking":false}),
            ),
            (
                Protocol::ChatCompletions,
                None,
                "thinking_toggle='enable-thinking'",
                toggle.clone(),
                High,
                json!({"enable_thinking":true}),
            ),
            (
                Protocol::ChatCompletions,
                None,
                "thinking_budget='thinking-budget'",
                budget.clone(),
                Low,
                json!({"thinking_budget":2048}),
            ),
            (
                Protocol::ChatCompletions,
                None,
                "thinking_budget='reasoning-max-tokens'",
                budget.clone(),
                High,
                json!({"reasoning":{"max_tokens":16384}}),
            ),
            (
                Protocol::ChatCompletions,
                None,
                "thinking_toggle='thinking-type'",
                combined_effort.clone(),
                High,
                json!({"reasoning_effort":"high","thinking":{"type":"enabled"}}),
            ),
            (
                Protocol::ChatCompletions,
                None,
                "thinking_toggle='thinking-type'",
                effort.clone(),
                High,
                json!({"reasoning_effort":"high"}),
            ),
            (
                Protocol::ChatCompletions,
                None,
                "",
                combined_effort.clone(),
                High,
                json!({"reasoning_effort":"high"}),
            ),
            (
                Protocol::ChatCompletions,
                None,
                "thinking_toggle='enable-thinking'\nthinking_budget='reasoning-max-tokens'",
                combined_budget.clone(),
                High,
                json!({"reasoning":{"max_tokens":16384},"enable_thinking":true}),
            ),
            (
                Protocol::Messages,
                None,
                "",
                effort.clone(),
                Off,
                json!({"thinking":{"type":"disabled"}}),
            ),
            (
                Protocol::Messages,
                None,
                "",
                effort.clone(),
                High,
                json!({"thinking":{"type":"adaptive"},"output_config":{"effort":"high"}}),
            ),
            (
                Protocol::Messages,
                None,
                "",
                toggle.clone(),
                Off,
                json!({"thinking":{"type":"disabled"}}),
            ),
            (
                Protocol::Messages,
                None,
                "",
                toggle.clone(),
                High,
                json!({"thinking":{"type":"adaptive"}}),
            ),
            (
                Protocol::Messages,
                None,
                "",
                budget.clone(),
                Low,
                json!({"thinking":{"type":"enabled","budget_tokens":2048}}),
            ),
            // 元数据 max 把预算压到 512，Messages 仍按协议下限发送 1024。
            (
                Protocol::Messages,
                Some(32768),
                "",
                json!([{"type":"budget_tokens","max":512}]),
                Low,
                json!({"thinking":{"type":"enabled","budget_tokens":1024}}),
            ),
            // 输出上限的一半（16384）低于元数据 min，发送时抬回 min。
            (
                Protocol::Messages,
                Some(32768),
                "",
                json!([{"type":"budget_tokens","min":20000}]),
                Low,
                json!({"thinking":{"type":"enabled","budget_tokens":20000}}),
            ),
            // min 低于 1024 时，协议下限 1024 仍然生效。
            (
                Protocol::Messages,
                Some(32768),
                "",
                json!([{"type":"budget_tokens","min":512,"max":600}]),
                Low,
                json!({"thinking":{"type":"enabled","budget_tokens":1024}}),
            ),
            (
                Protocol::GenerateContent,
                None,
                "",
                effort.clone(),
                Off,
                json!({"generationConfig":{"thinkingConfig":{"thinkingBudget":0}}}),
            ),
            (
                Protocol::GenerateContent,
                None,
                "",
                effort,
                High,
                json!({"generationConfig":{"thinkingConfig":{"thinkingLevel":"high"}}}),
            ),
            (
                Protocol::GenerateContent,
                None,
                "",
                toggle.clone(),
                Off,
                json!({"generationConfig":{"thinkingConfig":{"thinkingBudget":0}}}),
            ),
            (
                Protocol::GenerateContent,
                None,
                "",
                toggle,
                High,
                json!({"generationConfig":{"thinkingConfig":{"thinkingBudget":-1}}}),
            ),
            (
                Protocol::GenerateContent,
                None,
                "",
                budget,
                Low,
                json!({"generationConfig":{"thinkingConfig":{"thinkingBudget":2048}}}),
            ),
        ] {
            let cases = if protocol == Protocol::Responses {
                let reasoning_enabled = match selected {
                    Off => {
                        json!({"include":["reasoning.encrypted_content"],"reasoning":{"effort":"none"}})
                    }
                    High => {
                        json!({"include":["reasoning.encrypted_content"],"reasoning":{"effort":"high","summary":"auto"}})
                    }
                    _ => unreachable!("Responses matrix selects off or high"),
                };
                vec![
                    (None, json!({}), expected.clone()),
                    (Some(false), json!({}), expected.clone()),
                    (
                        Some(true),
                        json!({"include":["reasoning.encrypted_content"],"reasoning":{"summary":"auto"}}),
                        reasoning_enabled,
                    ),
                ]
            } else if protocol == Protocol::Messages {
                let mut displayed = expected.clone();
                if displayed["thinking"]["type"] == "adaptive" {
                    displayed["thinking"]["display"] = "summarized".into();
                }
                vec![
                    (Some(false), json!({}), expected.clone()),
                    (Some(true), json!({}), displayed),
                    (None, json!({}), expected),
                ]
            } else if protocol == Protocol::Interactions {
                let mut with_summary = expected.clone();
                with_summary["generation_config"]["thinking_summaries"] = "auto".into();
                let unselected = json!({"generation_config":{"thinking_summaries":"auto"}});
                vec![
                    (Some(false), json!({}), expected.clone()),
                    (Some(true), unselected, with_summary),
                    (None, json!({}), expected),
                ]
            } else if protocol == Protocol::GenerateContent {
                let mut with_thoughts = expected.clone();
                if selected != Off {
                    with_thoughts["generationConfig"]["thinkingConfig"]["includeThoughts"] =
                        true.into();
                }
                let unselected =
                    json!({"generationConfig":{"thinkingConfig":{"includeThoughts":true}}});
                vec![
                    (Some(false), json!({}), expected.clone()),
                    (Some(true), unselected, with_thoughts),
                    (None, json!({}), expected),
                ]
            } else {
                vec![(None, json!({}), expected)]
            };
            for (model_reasoning, unselected, expected) in cases {
                let mut features = Features::defaults(protocol);
                let overrides: std::collections::BTreeMap<String, toml::Value> =
                    toml::from_str(overrides).unwrap();
                for (name, value) in overrides {
                    features.apply(&name, value).unwrap();
                }
                let model: crate::metadata::Model = serde_json::from_value(json!({
                    "id":"model", "reasoning":model_reasoning,
                    "reasoning_options":options, "limit":{"output":output},
                }))
                .unwrap();
                let capabilities = crate::reasoning::ReasoningCapabilities::new(
                    "test",
                    "model",
                    Some(&model),
                    features,
                )
                .unwrap();
                for selection in [None, Some(selected)] {
                    let mut request = chat_request();
                    request.target.effort = selection;
                    let plan = selection
                        .map(|effort| capabilities.select(effort).expect("supported source"));
                    let origin = lattice_conversation::ReplayOrigin {
                        protocol,
                        provider_id: "test".into(),
                    };
                    let body = ProtocolParser::new(protocol)
                        .request_body(&request, output, model.reasoning, features, plan, &origin)
                        .unwrap();
                    let controls: serde_json::Map<_, _> = body
                        .as_object()
                        .unwrap()
                        .iter()
                        .filter(|(key, _)| {
                            matches!(
                                key.as_str(),
                                "include"
                                    | "reasoning_effort"
                                    | "reasoning"
                                    | "thinking"
                                    | "thinking_budget"
                                    | "enable_thinking"
                                    | "output_config"
                                    | "generationConfig"
                                    | "generation_config"
                            )
                        })
                        .map(|(key, value)| (key.clone(), value.clone()))
                        .collect();
                    assert_eq!(
                        serde_json::Value::Object(controls),
                        if selection.is_some() {
                            expected.clone()
                        } else {
                            unselected.clone()
                        },
                        "{protocol:?}, {features:?}, {model_reasoning:?}, {selection:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn cache_features_mark_only_existing_request_content() {
        use crate::protocol::{Cache, ChatCompletionsFeatures, MessagesFeatures, ReasoningReplay};
        use crate::test_reply::{bind_replay_step, cache_controls, wire_to_native};
        use lattice_conversation::{
            ConversationItem, MessagePart, MessageRole, MessageSemantic, NativeMessage,
            ReplayOrigin, StepContent,
        };
        use serde_json::json;
        let markers = |paths: &[&str]| {
            paths
                .iter()
                .map(|path| (path.to_string(), json!({"type":"ephemeral"})))
                .collect::<Vec<_>>()
        };
        let message = |role, text: &str| {
            ConversationItem::Message(NativeMessage::new(
                role,
                None,
                vec![MessagePart::semantic_only(MessageSemantic::Text {
                    text: text.into(),
                })],
            ))
        };
        let mut request = chat_request();
        request.messages = vec![
            message(MessageRole::System, "system"),
            message(MessageRole::User, "question"),
        ];
        let history = request.messages.clone();
        for protocol in [
            Protocol::ChatCompletions,
            Protocol::Messages,
            Protocol::GenerateContent,
        ] {
            let origin = ReplayOrigin {
                protocol,
                provider_id: "test".into(),
            };
            for enabled in [false, true] {
                let features = match protocol {
                    Protocol::ChatCompletions => {
                        Features::ChatCompletions(ChatCompletionsFeatures {
                            cache_markers: enabled,
                            ..Default::default()
                        })
                    }
                    Protocol::Messages => Features::Messages(MessagesFeatures {
                        cache: if enabled {
                            Cache::Automatic
                        } else {
                            Cache::None
                        },
                    }),
                    _ => Features::GenerateContent,
                };
                let parser = ProtocolParser::new(protocol);
                let body = parser
                    .request_body(&request, None, None, features, None, &origin)
                    .unwrap();
                let expected = match protocol {
                    Protocol::ChatCompletions if enabled => markers(&[
                        "/messages/0/content/0/cache_control",
                        "/messages/1/content/0/cache_control",
                    ]),
                    Protocol::Messages if enabled => {
                        markers(&["/cache_control", "/system/0/cache_control"])
                    }
                    _ => vec![],
                };
                assert_eq!(
                    cache_controls(&body),
                    expected,
                    "{protocol:?}, enabled={enabled}"
                );
                assert_eq!(request.messages, history);
            }
        }
        let mut signed_wire: serde_json::Value = serde_json::from_str(include_str!(
            "../tests/fixtures/projection/anthropic_thinking_tool_use.json"
        ))
        .unwrap();
        signed_wire["content"].as_array_mut().unwrap().truncate(2);
        let mut signed = wire_to_native(Protocol::Messages, &signed_wire)
            .unwrap()
            .bind_zero_tool()
            .unwrap();
        signed.provider_origin.as_mut().unwrap().provider_id = "historical-source".into();
        for with_system in [true, false] {
            let mut prefix = if with_system {
                vec![message(MessageRole::System, "system")]
            } else {
                vec![]
            };
            prefix.push(message(MessageRole::User, "earlier"));
            prefix.push(ConversationItem::Step(StepContent {
                assistant: signed.clone(),
                results: vec![],
            }));
            for (protocol, fixture, features) in [
                (
                    Protocol::ChatCompletions,
                    include_str!("../tests/fixtures/projection/openai_assistant_tool_calls.json"),
                    Features::ChatCompletions(ChatCompletionsFeatures {
                        cache_markers: true,
                        ..Default::default()
                    }),
                ),
                (
                    Protocol::Messages,
                    include_str!("../tests/fixtures/projection/anthropic_thinking_tool_use.json"),
                    Features::Messages(MessagesFeatures {
                        cache: Cache::Automatic,
                    }),
                ),
            ] {
                let (assistant, results) = bind_replay_step(
                    wire_to_native(protocol, &serde_json::from_str(fixture).unwrap()).unwrap(),
                );
                assert_eq!(results.len(), 2);
                request.messages = prefix.clone();
                request
                    .messages
                    .push(ConversationItem::Step(StepContent { assistant, results }));
                let history = request.messages.clone();
                let origin = ReplayOrigin {
                    protocol,
                    provider_id: "other".into(),
                };
                let body = ProtocolParser::new(protocol)
                    .request_body(&request, None, None, features, None, &origin)
                    .unwrap();
                let expected = match (protocol, with_system) {
                    (Protocol::ChatCompletions, true) => markers(&[
                        "/messages/0/content/0/cache_control",
                        "/messages/5/content/0/cache_control",
                    ]),
                    (Protocol::ChatCompletions, false) => {
                        markers(&["/messages/4/content/0/cache_control"])
                    }
                    (Protocol::Messages, true) => {
                        markers(&["/cache_control", "/system/0/cache_control"])
                    }
                    (Protocol::Messages, false) => markers(&["/cache_control"]),
                    _ => unreachable!(),
                };
                assert_eq!(
                    cache_controls(&body),
                    expected,
                    "{protocol:?}, system={with_system}"
                );
                let body_text = body.to_string();
                for private in [
                    "Need the weather first, then the time; both are independent calls.",
                    "ErUBCkYIAxgCIkD3c2FmZGVmNTQ2Ny0xMjM0LWFiY2QtZXhhbXBsZXNpZ25hdHVyZQ==",
                    "The user wants the weather; call get_weather first, then get_time.",
                ] {
                    assert!(
                        !body_text.contains(private),
                        "跨来源回放保留了私有材料：{protocol:?}"
                    );
                }
                let messages = body["messages"].as_array().unwrap();
                if protocol == Protocol::ChatCompletions {
                    assert_eq!(messages[messages.len() - 2]["role"], "tool");
                    assert_eq!(messages.last().unwrap()["role"], "tool");
                } else {
                    assert_eq!(
                        messages.last().unwrap()["content"]
                            .as_array()
                            .unwrap()
                            .len(),
                        2
                    );
                    assert_eq!(
                        messages.last().unwrap()["content"][1]["type"],
                        "tool_result"
                    );
                }
                assert_eq!(request.messages, history);
            }
            let reasoning_only = wire_to_native(Protocol::ChatCompletions, &json!({"choices":[{"message":{"content":null,"reasoning_content":"reasoning-only"}}]})).unwrap().bind_zero_tool().unwrap();
            for (assistant, expected_content) in [
                (
                    NativeMessage::new(MessageRole::Assistant, None, vec![]),
                    json!(null),
                ),
                (
                    reasoning_only,
                    json!([{"type":"thinking","thinking":[{"type":"text","text":"reasoning-only"}]}]),
                ),
            ] {
                request.messages = prefix.clone();
                request.messages.push(ConversationItem::Step(StepContent {
                    assistant,
                    results: vec![],
                }));
                let history = request.messages.clone();
                let origin = ReplayOrigin {
                    protocol: Protocol::ChatCompletions,
                    provider_id: "test".into(),
                };
                let body = ProtocolParser::new(Protocol::ChatCompletions)
                    .request_body(
                        &request,
                        None,
                        None,
                        Features::ChatCompletions(ChatCompletionsFeatures {
                            cache_markers: true,
                            reasoning_replay: ReasoningReplay::ContentChunks,
                            ..Default::default()
                        }),
                        None,
                        &origin,
                    )
                    .unwrap();
                let messages = body["messages"].as_array().unwrap();
                assert_eq!(
                    messages.last().unwrap(),
                    &json!({"role":"assistant","content":expected_content})
                );
                assert_eq!(
                    cache_controls(&body),
                    if with_system {
                        markers(&["/messages/0/content/0/cache_control"])
                    } else {
                        vec![]
                    }
                );
                assert_eq!(request.messages, history);
            }
        }
    }

    async fn resolved_request(protocol: &str, url: &str, key: &str) -> crate::ResolvedRequest {
        let directory = tempfile::tempdir().unwrap();
        std::fs::set_permissions(
            directory.path(),
            <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o700),
        )
        .unwrap();
        std::fs::write(
            directory.path().join("providers.toml"),
            format!(
                "[providers.test]\nprotocol='{protocol}'\nbase_url='{url}'\nauth='{}'\n",
                if protocol == "anthropic-messages" {
                    "x-api-key"
                } else {
                    "bearer"
                }
            ),
        )
        .unwrap();
        crate::auth::AuthStore::new(directory.path().into())
            .set("test", key)
            .unwrap();
        crate::RequestGateway::new(directory.path().into(), vec![])
            .unwrap()
            .open_turn()
            .resolve(&crate::ModelTarget::new("test", "model"))
            .await
            .unwrap()
    }

    /// 生成彼此不同的根：前 4 字节是序号，其余用 `fill` 填满 `len` 字节。
    fn distinct_root(index: u32, len: usize, fill: u8) -> Vec<u8> {
        let mut der = index.to_be_bytes().to_vec();
        der.resize(len, fill);
        der
    }

    fn accept_all(_: &[u8]) -> Result<(), std::convert::Infallible> {
        Ok(())
    }

    fn capture_warnings<T>(run: impl FnOnce() -> T) -> (T, String) {
        #[derive(Clone, Default)]
        struct Output(Arc<std::sync::Mutex<Vec<u8>>>);
        impl std::io::Write for Output {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let output = Output::default();
        let writer = output.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_env_filter("lattice_llm=warn")
            .without_time()
            .with_ansi(false)
            .with_writer(move || writer.clone())
            .finish();
        // 同时保留空 subscriber，使并行测试首次注册调用点时按全部 subscriber 计算兴趣。
        let _other_threads = tracing::Dispatch::new(tracing::subscriber::NoSubscriber::default());
        let result = tracing::subscriber::with_default(subscriber, run);
        let logs = String::from_utf8(output.0.lock().unwrap().clone()).unwrap();
        (result, logs)
    }

    #[test]
    fn root_selection_stops_at_the_count_limit_and_keeps_the_original_order() {
        let mut roots = vec![distinct_root(u32::MAX, 65_537, 9)];
        roots.extend((0..2_000).map(|index| distinct_root(index, 64, 1)));
        let (kept, logs) = capture_warnings(|| super::select_roots(roots.clone(), accept_all));
        let expected: Vec<_> = roots[1..=1_024].to_vec();
        let kept: Vec<_> = kept.into_iter().map(|(der, ())| der).collect();
        assert_eq!(
            kept, expected,
            "the first 1024 usable roots in original order"
        );
        assert!(
            logs.contains("truncated=976") && logs.contains("later roots are not trusted"),
            "truncation must log how many roots were dropped, got {logs}"
        );
        assert!(
            logs.contains("bytes=65537"),
            "the oversized root must be logged as skipped, got {logs}"
        );
    }

    #[test]
    fn root_selection_stops_at_the_byte_limit_instead_of_filling_with_later_roots() {
        // 60_000 * 69 = 4_140_000，再加一张 60_000 会超过 4 MiB（4_194_304）。
        // 第 70 张之后放一张小根：旧实现跳过大根后会把它补进来，打乱前缀。
        let mut roots: Vec<_> = (0..70)
            .map(|index| distinct_root(index, 60_000, 2))
            .collect();
        roots.push(distinct_root(70, 100, 3));
        let (kept, logs) = capture_warnings(|| super::select_roots(roots.clone(), accept_all));
        let kept: Vec<_> = kept.into_iter().map(|(der, ())| der).collect();
        assert_eq!(
            kept,
            roots[..69].to_vec(),
            "byte limit must keep an in-order prefix"
        );
        assert!(
            logs.contains("truncated=2"),
            "both roots after the limit must be counted as truncated, got {logs}"
        );
    }

    #[test]
    fn invalid_root_that_would_cross_the_byte_limit_is_skipped_not_truncating() {
        // 69 张 60_000 字节有效根共 4_140_000 字节；再加一张 60_000 字节的无效根会
        // 超过 4 MiB（4_194_304），但它解析失败、不计入总量，后面 100 字节的有效根
        // 仍放得下，必须保留。
        let mut roots: Vec<_> = (0..69)
            .map(|index| distinct_root(index, 60_000, 2))
            .collect();
        roots.push(distinct_root(69, 60_000, 0xff));
        roots.push(distinct_root(70, 100, 3));
        let (kept, logs) = capture_warnings(|| {
            super::select_roots(roots.clone(), |der: &[u8]| {
                if der[4] == 0xff {
                    Err("bad certificate")
                } else {
                    Ok(())
                }
            })
        });
        let kept: Vec<_> = kept.into_iter().map(|(der, ())| der).collect();
        let mut expected = roots[..69].to_vec();
        expected.push(roots[70].clone());
        assert_eq!(
            kept.len(),
            70,
            "69 large valid roots plus the small valid root after the invalid one"
        );
        assert!(
            kept == expected,
            "kept roots must be the valid ones in original order"
        );
        assert!(
            logs.matches("skipping invalid TLS root certificate")
                .count()
                == 1
                && !logs.contains("truncated="),
            "the invalid root is skipped and nothing is truncated, got {logs}"
        );
    }

    #[test]
    fn root_selection_skips_unparseable_roots_without_using_a_slot() {
        let roots: Vec<_> = (0..4).map(|index| distinct_root(index, 16, 4)).collect();
        let (kept, logs) = capture_warnings(|| {
            super::select_roots(roots.clone(), |der: &[u8]| {
                if der[3] == 1 {
                    Err("bad certificate")
                } else {
                    Ok(())
                }
            })
        });
        let kept: Vec<_> = kept.into_iter().map(|(der, ())| der).collect();
        assert_eq!(
            kept,
            vec![roots[0].clone(), roots[2].clone(), roots[3].clone()],
            "unparseable roots are skipped and the rest keep their order"
        );
        assert!(
            logs.contains("skipping invalid TLS root certificate"),
            "the unparseable root must be logged, got {logs}"
        );
    }

    #[test]
    fn each_skipped_root_warns_exactly_once() {
        // 两张超大、三张无效、两张有效：只有被跳过的 5 张各记一条 WARN。
        // 超大根带无效标记也不得再解析，否则会同时记大小和解析两条。
        let roots = vec![
            distinct_root(0, 65_537, 0xff),
            distinct_root(1, 16, 0xff),
            distinct_root(2, 16, 1),
            distinct_root(3, 70_000, 1),
            distinct_root(4, 16, 0xff),
            distinct_root(5, 16, 0xff),
            distinct_root(6, 16, 1),
        ];
        let (kept, logs) = capture_warnings(|| {
            super::select_roots(roots.clone(), |der: &[u8]| {
                if der[4] == 0xff {
                    Err("bad certificate")
                } else {
                    Ok(())
                }
            })
        });
        let kept: Vec<_> = kept.into_iter().map(|(der, ())| der).collect();
        assert_eq!(kept, vec![roots[2].clone(), roots[6].clone()]);
        let warns: Vec<_> = logs.lines().filter(|line| line.contains("WARN")).collect();
        assert_eq!(warns.len(), 5, "one WARN per skipped root, got {logs}");
        assert_eq!(
            logs.matches("skipping TLS root certificate larger than")
                .count(),
            2,
            "{logs}"
        );
        assert_eq!(
            logs.matches("skipping invalid TLS root certificate")
                .count(),
            3,
            "{logs}"
        );
    }

    #[test]
    fn gateway_warns_once_per_skipped_root_with_real_parsing() {
        // 网关构造只选一次根；真实解析下超大根和无效根各记一条 WARN。
        let directory = tempfile::tempdir().unwrap();
        let ca = crate::test_tls::TLS.ca_der.clone();
        let roots = vec![vec![0_u8; 65_537], vec![1, 2, 3], ca.clone(), vec![4, 5]];
        let (gateway, logs) = capture_warnings(|| {
            crate::RequestGateway::new(directory.path().into(), roots).unwrap()
        });
        assert_eq!(gateway.tls_roots_der(), &[ca][..]);
        let warns = logs.lines().filter(|line| line.contains("WARN")).count();
        assert_eq!(warns, 3, "one WARN per skipped root, got {logs}");
    }

    #[test]
    fn request_pinning_uses_exact_loopback_exceptions_and_rejects_unsafe_addresses() {
        for url in [
            "http://localhost:8080/models/m?alt=sse",
            "https://127.0.0.1:8443/models/m?alt=sse",
            "http://[::1]:8080/models/m?alt=sse",
            "https://8.8.8.8/models/m?alt=sse",
        ] {
            pin_http_client(url, reqwest::Client::builder())
                .unwrap()
                .build()
                .unwrap();
        }
        for url in [
            "https://10.0.0.1/models/m?alt=sse",
            "https://127.0.0.2/models/m?alt=sse",
            "https://[fc00::1]/models/m?alt=sse",
            "https://[2001:db8::1]/models/m?alt=sse",
            "https://127.0.0.1.nip.io/models/m?alt=sse",
        ] {
            assert!(
                matches!(
                    pin_http_client(url, reqwest::Client::builder()),
                    Err(NetGuardError::Config { .. })
                ),
                "{url}"
            );
        }
    }

    /// #90: URL validation and DNS pinning bind the first hop only, so a
    /// followed redirect reaches an unvalidated host with none of them — and
    /// on 307 replays the request body and the `x-api-key` header reqwest
    /// does not strip across hosts. The redirect must surface to the caller.
    #[tokio::test]
    async fn provider_client_does_not_follow_redirects() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        let target = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind redirect target");
        let target_port = target.local_addr().expect("target addr").port();
        let reached = Arc::new(AtomicBool::new(false));
        let reached_in_server = Arc::clone(&reached);
        tokio::spawn(async move {
            if let Ok((mut stream, _)) = target.accept().await {
                reached_in_server.store(true, Ordering::SeqCst);
                let mut buf = [0u8; 1024];
                let _ = stream.read(&mut buf).await;
                let _ = stream
                    .write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok",
                    )
                    .await;
            }
        });

        let redirector = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind redirector");
        let redirector_addr = redirector.local_addr().expect("redirector addr");
        tokio::spawn(async move {
            if let Ok((mut stream, _)) = redirector.accept().await {
                let mut buf = [0u8; 1024];
                let _ = stream.read(&mut buf).await;
                let response = format!(
                    "HTTP/1.1 307 Temporary Redirect\r\nLocation: http://127.0.0.1:{target_port}/collect\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                );
                let _ = stream.write_all(response.as_bytes()).await;
            }
        });

        let url = format!("http://{redirector_addr}/v1/chat/completions");
        let client = build_http_client_for_url(&url, http_client_builder()).expect("build client");
        let response = client
            .post(&url)
            .header("x-api-key", "sk-ant-secret")
            .json(&serde_json::json!({ "messages": [] }))
            .send()
            .await
            .expect("request completes");

        assert_eq!(
            response.status().as_u16(),
            307,
            "redirect must reach the caller instead of being followed"
        );
        assert!(
            !reached.load(Ordering::SeqCst),
            "redirect target must never be contacted"
        );
    }

    // 分别防止 HTTP 兜底早于 86,400 秒截断合法长流，以及未设 Turn deadline 时
    // HTTP 86,400 秒兜底失效导致持续流无界运行。外部二进制测试无法在合理墙钟时间内
    // 等待 24 小时，因此用暂停的虚拟时钟在进程内隔离验证两道超时边界。
    #[tokio::test]
    async fn provider_stream_observes_turn_deadline_and_http_backstop() {
        use futures::FutureExt;
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        struct KeepPausedClockFrozen(tokio::task::JoinHandle<()>);
        impl Drop for KeepPausedClockFrozen {
            fn drop(&mut self) {
                self.0.abort();
            }
        }

        let total_secs = 86_400;
        let listener = Arc::new(tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap());
        let address = listener.local_addr().unwrap();
        let wall_deadline = Instant::now() + Duration::from_secs(20);

        for use_turn_deadline in [true, false] {
            let connection = resolved_request(
                "chat-completions",
                &format!("http://{address}"),
                "local-test-key",
            )
            .await;

            if use_turn_deadline {
                tokio::time::pause();
            }
            // 保持运行队列非空，避免等待本地 loopback I/O 或 spawn_blocking 时暂停的时钟自动快进。
            let _keep_frozen = KeepPausedClockFrozen(tokio::spawn(async move {
                loop {
                    assert!(
                        Instant::now() < wall_deadline,
                        "provider timeout test stalled"
                    );
                    tokio::task::yield_now().await;
                }
            }));

            let (send, mut receive) = tokio::sync::mpsc::unbounded_channel::<()>();
            let server_listener = Arc::clone(&listener);
            let server = tokio::spawn(async move {
                let (mut socket, _) = server_listener.accept().await.unwrap();
                socket.set_nodelay(true).unwrap();
                let mut request = Vec::new();
                while !request.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
                    let mut chunk = [0; 1024];
                    let count = socket.read(&mut chunk).await.unwrap();
                    assert!(count > 0);
                    request.extend_from_slice(&chunk[..count]);
                }
                socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n").await.unwrap();
                let event = "data: {\"choices\":[{\"delta\":{\"content\":\"x\"}}]}\n\n";
                let chunk = format!("{:x}\r\n{event}\r\n", event.len());
                while receive.recv().await.is_some() {
                    if socket.write_all(chunk.as_bytes()).await.is_err() {
                        break;
                    }
                }
            });

            let started = tokio::time::Instant::now();
            let control = if use_turn_deadline {
                StreamControl {
                    deadline: Some(started + Duration::from_secs(total_secs)),
                    ..Default::default()
                }
            } else {
                StreamControl::default()
            };
            let mut stream = connection
                .stream_chat(chat_request(), control)
                .await
                .unwrap();
            assert_eq!(tokio::time::Instant::now(), started);
            assert_eq!(stream.next().await, Some(LlmEvent::Started));
            send.send(()).unwrap();
            assert!(matches!(stream.next().await, Some(LlmEvent::Delta(_))));

            let mut elapsed = 0;
            let outcome = loop {
                let mut next = std::pin::pin!(stream.next());
                assert!(
                    next.as_mut().now_or_never().is_none(),
                    "use_turn_deadline={use_turn_deadline}: stream stopped at {elapsed}s"
                );
                elapsed = if elapsed < total_secs - 1 {
                    (elapsed + 25).min(total_secs - 1)
                } else {
                    total_secs
                };
                // 留出 1 毫秒让定时器在整秒边界后唤醒，先核对 86,399 秒存活，再核对 86,400 秒终止。
                let at = started + Duration::from_secs(elapsed) + Duration::from_millis(1);
                tokio::time::advance(at.duration_since(tokio::time::Instant::now())).await;
                assert_eq!(tokio::time::Instant::now(), at);
                if elapsed == total_secs {
                    break next.await;
                }
                send.send(()).unwrap();
                assert!(
                    matches!(next.await, Some(LlmEvent::Delta(_))),
                    "use_turn_deadline={use_turn_deadline}: stream stopped at {elapsed}s, before {total_secs}s"
                );
            };

            let Some(LlmEvent::Finished {
                outcome:
                    ReplyOutcome::Interrupted {
                        reason: ReplyStop::Stream(reason),
                    },
                ..
            }) = outcome
            else {
                panic!(
                    "use_turn_deadline={use_turn_deadline}: expected interrupted outcome at {total_secs}s, got {outcome:?}"
                );
            };
            if use_turn_deadline {
                assert_eq!(reason, StreamStop::Deadline);
            } else {
                assert!(
                    matches!(
                        &reason,
                        StreamStop::Provider(crate::ProviderFailure { retryable: true, message })
                            if message.contains("timed out")
                    ),
                    "expected retryable HTTP backstop timeout, got {reason:?}"
                );
            }
            assert!(stream.next().await.is_none());
            drop(send);
            server.await.unwrap();
        }
    }

    #[tokio::test]
    async fn subscription_usage_limit_http_and_stream_keep_safe_diagnostics() {
        use serde_json::json;
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let request_id = format!("req-frozen-secret{}", "x".repeat(111));
        for streaming in [false, true] {
            let error = json!({"code":"subscription_sharing_usage_limit_exceeded","message":format!("frozen-secret {}", "界".repeat(400))});
            let status = if streaming {
                "200 OK"
            } else {
                "429 Too Many Requests"
            };
            let body = if streaming {
                [
                    json!({"type":"response.output_item.added","output_index":0,"item":{"type":"message","id":"msg","role":"assistant","content":[]}}),
                    json!({"type":"response.content_part.added","output_index":0,"item_id":"msg","content_index":0,"part":{"type":"output_text","text":""}}),
                    json!({"type":"response.output_text.delta","output_index":0,"item_id":"msg","content_index":0,"delta":"partial"}),
                    json!({"type":"response.failed","response":{"status":"failed","error":error,"usage":{"input_tokens":11,"output_tokens":3}}}),
                ].iter().map(|event| format!("data: {event}\n\n")).collect::<String>()
            } else {
                json!({"error":error}).to_string()
            };
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let response = format!(
                "HTTP/1.1 {status}\r\nx-should-retry: true\r\nretry-after: 2\r\nx-request-id: {request_id}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let server = tokio::spawn(async move {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                while !request.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
                    let mut chunk = [0; 1024];
                    let count = socket.read(&mut chunk).await.unwrap();
                    assert!(count > 0);
                    request.extend_from_slice(&chunk[..count]);
                    assert!(request.len() <= 8192);
                }
                socket.write_all(response.as_bytes()).await.unwrap();
            });
            let connection =
                resolved_request("responses", &format!("http://{address}"), "frozen-secret").await;
            let result = tokio::time::timeout(Duration::from_secs(3), async {
                match connection
                    .stream_chat(chat_request(), StreamControl::default())
                    .await
                {
                    Ok(stream) => Ok(stream.collect::<Vec<_>>().await),
                    Err(error) => Err(error),
                }
            })
            .await
            .unwrap();
            let diagnostic = if streaming {
                let events = result.unwrap();
                assert!(
                    matches!(&events[1], LlmEvent::Delta(crate::LiveDelta::Text(text)) if text == "partial")
                );
                let LlmEvent::Finished {
                    outcome:
                        ReplyOutcome::Interrupted {
                            reason: ReplyStop::Stream(StreamStop::Provider(failure)),
                        },
                    report,
                } = events.last().unwrap()
                else {
                    panic!("provider failure: {events:?}")
                };
                assert!(!failure.retryable);
                assert_eq!(report.finish_reason, None);
                let usage = report.usage.as_ref().unwrap();
                assert_eq!((usage.input, usage.output), (Some(11), Some(3)));
                failure.message.clone()
            } else {
                let error = result.unwrap_err();
                assert_eq!(error.status_code(), Some(429));
                assert_eq!(error.retry_after(), Some(Duration::from_secs(2)));
                assert!(!error.retryable());
                error.to_string()
            };
            assert!(
                diagnostic.starts_with(if streaming {
                    "HTTP 200 Responses "
                } else {
                    "HTTP 429 "
                }),
                "{diagnostic}"
            );
            assert!(diagnostic.contains("subscription_sharing_usage_limit_exceeded"));
            assert!(
                diagnostic.contains("请到 ChatGPT 设置查看用量"),
                "{diagnostic}"
            );
            assert!(
                diagnostic.contains(&request_id.replace("frozen-secret", "[REDACTED]")),
                "{diagnostic}"
            );
            assert!(!diagnostic.contains("frozen-secret"));
            assert_eq!(diagnostic.chars().count(), transport::MAX_DIAGNOSTIC_CHARS);
            server.await.unwrap();
        }
    }

    #[tokio::test]
    async fn resolved_request_redacts_http_stream_and_unknown_blocks() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            for (status, body, auth) in [
                (
                    "401 Unauthorized",
                    "frozen-secret rejected",
                    "authorization: bearer frozen-secret",
                ),
                (
                    "200 OK",
                    "data: {\"error\":{\"message\":\"frozen-secret rejected\"}}\n\n",
                    "authorization: bearer frozen-secret",
                ),
                (
                    "200 OK",
                    "event: content_block_start\ndata: {\"index\":9,\"content_block\":{\"type\":\"frozen-secret\",\"data\":\"private-payload\"}}\n\n",
                    "x-api-key: frozen-secret",
                ),
            ] {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                loop {
                    let mut chunk = [0; 1024];
                    let count = socket.read(&mut chunk).await.unwrap();
                    assert!(count > 0);
                    request.extend_from_slice(&chunk[..count]);
                    assert!(request.len() <= 8192);
                    if request.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
                        break;
                    }
                }
                let request = String::from_utf8(request).unwrap();
                assert!(request.to_ascii_lowercase().contains(auth));
                socket.write_all(format!("HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
            }
        });
        let connection = resolved_request(
            "chat-completions",
            &format!("http://{address}"),
            "frozen-secret",
        )
        .await;
        let request = chat_request();
        let error = match tokio::time::timeout(
            Duration::from_secs(3),
            connection.stream_chat(request, StreamControl::default()),
        )
        .await
        .unwrap()
        {
            Ok(_) => panic!("expected provider failure"),
            Err(error) => error,
        };
        assert_eq!(error.status_code(), Some(401));
        assert!(!format!("{error:?}").contains("frozen-secret"));
        let request = chat_request();
        let events = tokio::time::timeout(Duration::from_secs(3), async {
            connection
                .stream_chat(request, StreamControl::default())
                .await
                .unwrap()
                .collect::<Vec<_>>()
                .await
        })
        .await
        .unwrap();
        assert!(
            matches!(events.as_slice(), [LlmEvent::Started, LlmEvent::Finished { outcome: ReplyOutcome::Interrupted { reason: ReplyStop::Stream(StreamStop::Provider(failure)) }, .. }] if failure.message == "API error: [REDACTED] rejected" && !failure.retryable)
        );
        let connection = resolved_request(
            "anthropic-messages",
            &format!("http://{address}"),
            "frozen-secret",
        )
        .await;
        let events = tokio::time::timeout(Duration::from_secs(3), async {
            connection
                .stream_chat(chat_request(), StreamControl::default())
                .await
                .unwrap()
                .collect::<Vec<_>>()
                .await
        })
        .await
        .unwrap();
        assert!(matches!(
            events.as_slice(),
            [LlmEvent::Started, LlmEvent::Finished {
                outcome: ReplyOutcome::Interrupted {
                    reason: ReplyStop::Stream(StreamStop::Protocol(
                        crate::ReplyProtocolError::UnsupportedContentBlock { protocol, index: 9, block_type }
                    ))
                }, ..
            }] if protocol == "anthropic-messages" && block_type == "[REDACTED]"
        ));
        assert!(!format!("{events:?}").contains("private-payload"));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn unsupported_type_redacts_full_credentials_before_bounding() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        for (key, prefix, expected_type) in [
            (
                "secret-long-".repeat(8),
                String::new(),
                "[REDACTED]-tail".into(),
            ),
            (
                "secret-crosses-the-boundary".into(),
                "p".repeat(60),
                format!("{}[RED", "p".repeat(60)),
            ),
        ] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let body = format!(
                "event: content_block_start\ndata: {}\n\n",
                serde_json::json!({"index":9,"content_block":{"type":format!("{prefix}{key}-tail"),"data":"private-payload"}})
            );
            let server = tokio::spawn(async move {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                while !request.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
                    let mut chunk = [0; 1024];
                    let count = socket.read(&mut chunk).await.unwrap();
                    assert!(count > 0);
                    request.extend_from_slice(&chunk[..count]);
                    assert!(request.len() <= 8192);
                }
                socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
            });
            let connection =
                resolved_request("anthropic-messages", &format!("http://{address}"), &key).await;
            let events = tokio::time::timeout(Duration::from_secs(3), async {
                connection
                    .stream_chat(chat_request(), StreamControl::default())
                    .await
                    .unwrap()
                    .collect::<Vec<_>>()
                    .await
            })
            .await
            .unwrap();
            let [
                LlmEvent::Started,
                LlmEvent::Finished {
                    outcome:
                        ReplyOutcome::Interrupted {
                            reason:
                                ReplyStop::Stream(StreamStop::Protocol(
                                    error @ ReplyProtocolError::UnsupportedContentBlock {
                                        protocol,
                                        index: 9,
                                        block_type,
                                    },
                                )),
                        },
                    ..
                },
            ] = events.as_slice()
            else {
                panic!("typed protocol failure");
            };
            assert_eq!(protocol, "anthropic-messages");
            assert_eq!(block_type, &expected_type);
            assert!(!error.to_string().contains("secret-"));
            assert!(!format!("{events:?}").contains("private-payload"));
            server.await.unwrap();
        }
    }

    #[test]
    fn find_event_boundary_crlf_crlf() {
        let buf = b"data: hello\r\n\r\ndata: world\r\n\r\n";
        let (idx, len) = find_event_boundary(buf, &mut 0).unwrap();
        assert_eq!(idx, 11);
        assert_eq!(len, 4);
    }

    #[test]
    fn find_event_boundary_cr_cr() {
        let buf = b"data: hello\r\rdata: world\r\r";
        let (idx, len) = find_event_boundary(buf, &mut 0).unwrap();
        assert_eq!(idx, 11);
        assert_eq!(len, 2);
    }

    #[test]
    fn event_scan_keeps_progress_and_recognizes_every_split_separator() {
        for separator in [b"\n\n".as_slice(), b"\r\n\r\n", b"\r\r"] {
            for split in 0..separator.len() {
                let mut bytes = vec![b'x'; 3 * 1024 * 1024];
                let boundary = bytes.len();
                bytes.extend_from_slice(&separator[..split]);
                let mut scanned = 0;
                assert_eq!(find_event_boundary(&bytes, &mut scanned), None);
                assert_eq!(scanned, bytes.len() - 3);
                bytes.extend_from_slice(&separator[split..]);
                assert_eq!(
                    find_event_boundary(&bytes, &mut scanned),
                    Some((boundary, separator.len()))
                );
            }
        }
    }

    #[tokio::test]
    async fn interactions_named_frames_and_large_content_use_the_shared_assembler() {
        let fixture =
            include_str!("../tests/fixtures/projection/interactions_stream.sse").to_owned();
        let mut streams = vec![(
            fixture,
            "tool_calls",
            Some(lattice_conversation::TokenUsage {
                input: Some(12),
                cache_read: Some(4),
                cache_write: None,
                output: Some(8),
                reasoning: Some(3),
            }),
            None,
            None,
        )];
        for (initial, text) in [(false, "x"), (true, "x"), (false, "\0")] {
            let text = text.repeat(lattice_conversation::MAX_ASSISTANT_CONTENT_BYTES);
            let mut values = vec![
                serde_json::json!({"event_type":"step.start","index":0,"step":{"type":"model_output"}}),
            ];
            if initial {
                values[0]["step"]["content"] = serde_json::json!([{"type":"text","text":text}]);
            } else {
                values.push(serde_json::json!({"event_type":"step.delta","index":0,"delta":{"type":"text","text":text}}));
            }
            values.extend([serde_json::json!({"event_type":"step.stop","index":0}),serde_json::json!({"event_type":"interaction.completed","interaction":{"status":"completed"}})]);
            let raw: String = values
                .iter()
                .map(|value| {
                    format!(
                        "event: {}\ndata: {value}\n\n",
                        value["event_type"].as_str().unwrap()
                    )
                })
                .collect();
            assert!(raw.len() > 2 * 1024 * 1024);
            streams.push((raw, "stop", None, None, None));
        }
        let summary = "**Calculating Arrival Time**\n\nI've got the core objective crystal clear: determine the train's arrival time, presenting the solution in a concise sentence. Currently, I'm focusing on extracting the necessary data points – the departure time, trip duration, and delay. I see the input clearly. I'm moving toward the calculation and output formatting step.\n\n\n";
        streams.extend([
            (
                include_str!("../tests/fixtures/projection/interactions_function_call.sse").into(),
                "tool_calls",
                Some(lattice_conversation::TokenUsage {
                    input: Some(80),
                    cache_read: Some(0),
                    cache_write: None,
                    output: Some(18),
                    reasoning: Some(0),
                }),
                Some(serde_json::json!([
                    {"type":"thought","signature":"EmAKXgFpFH0TSRV7PsnZyMenWOiMgXsK"},
                    {"type":"function_call","id":"call_336116","name":"read_file","arguments":{"path":"README.md"}}
                ])),
                None,
            ),
            (
                include_str!("../tests/fixtures/projection/interactions_thought_and_text.sse").into(),
                "stop",
                Some(lattice_conversation::TokenUsage {
                    input: Some(51),
                    cache_read: Some(0),
                    cache_write: None,
                    output: Some(302),
                    reasoning: Some(290),
                }),
                Some(serde_json::json!([
                    {"type":"thought","signature":"EsAICr0IAWkUfRP1OQnzCiN1KQPKOmxy"},
                    {"type":"model_output","content":[{"type":"text","text":"The train will arrive at 12:42."}]}
                ])),
                None,
            ),
            (
                include_str!("../tests/fixtures/projection/interactions_thought_summary.sse").into(),
                "stop",
                Some(lattice_conversation::TokenUsage {
                    input: Some(43),
                    cache_read: Some(0),
                    cache_write: None,
                    output: Some(305),
                    reasoning: Some(293),
                }),
                Some(serde_json::json!([
                    {"type":"thought","summary":[{"type":"text","text":summary}],"signature":"EroICrcIAWkUfRMIDShXBuL7NZZ67Fha"},
                    {"type":"model_output","content":[{"type":"text","text":"The train will arrive at 12:42."}]}
                ])),
                Some(summary),
            ),
        ]);
        let origin = crate::test_reply::replay_origin(Protocol::Interactions);
        for (raw, finish_reason, usage, steps, reasoning) in streams {
            let chunks: Vec<_> = raw
                .as_bytes()
                .chunks(16 * 1024)
                .map(|chunk| Ok(bytes::Bytes::copy_from_slice(chunk)))
                .collect();
            let input = stream_sse_response(
                Box::pin(stream::iter(chunks)),
                ProtocolParser::new(Protocol::Interactions),
                Arc::default(),
            );
            let events: Vec<_> =
                assemble_reply_stream(Some(origin.clone()), input, StreamControl::default())
                    .collect()
                    .await;
            assert_eq!(
                events
                    .iter()
                    .filter(|event| matches!(event, crate::LlmEvent::Finished { .. }))
                    .count(),
                1
            );
            let Some(crate::LlmEvent::Finished { outcome, report }) = events.last() else {
                panic!("missing terminal event: {:?}", events.last())
            };
            assert_eq!(report.finish_reason.as_deref(), Some(finish_reason));
            assert_eq!(report.usage, usage);
            let crate::ReplyOutcome::Complete { message } = outcome else {
                panic!("complete: {outcome:?}")
            };
            if let Some(steps) = steps {
                assert_eq!(
                    crate::protocol::projection::native_to_wire(&origin, message.as_message())
                        .unwrap(),
                    steps
                );
            }
            if let Some(reasoning) = reasoning {
                let live: String = events
                    .iter()
                    .filter_map(|event| match event {
                        crate::LlmEvent::Delta(crate::LiveDelta::Reasoning(text)) => {
                            Some(text.as_str())
                        }
                        _ => None,
                    })
                    .collect();
                assert_eq!(live, reasoning);
            }
        }
        let limit = ProtocolParser::new(Protocol::Interactions).buffer_limit();
        let prefix = "event: keepalive\ndata: {\"event_type\":\"keepalive\"}";
        let raw = format!("{prefix}{}\n\n", " ".repeat(limit - prefix.len() - 2));
        assert_eq!(raw.len(), limit);
        let input = stream_sse_response(
            Box::pin(stream::iter([Ok(bytes::Bytes::from(raw))])),
            ProtocolParser::new(Protocol::Interactions),
            Arc::default(),
        );
        assert!(input.collect::<Vec<_>>().await.is_empty());
    }

    #[tokio::test]
    async fn responses_large_completed_and_protocol_buffer_limits() {
        let completed = serde_json::json!({"type":"response.completed","response":{
            "status":"completed", "instructions":"x".repeat(2*1024*1024), "tools":[{"parameters":{"description":"echo"}}],
            "output":[{"type":"message","id":"msg","role":"assistant","content":[{"type":"output_text","text":"a".repeat(lattice_conversation::MAX_ASSISTANT_CONTENT_BYTES)}]}]
        }});
        let raw = format!("data: {completed}\n\n");
        assert!(raw.len() > 2 * 1024 * 1024);
        let chunks: Vec<_> = raw
            .as_bytes()
            .chunks(16 * 1024)
            .map(|chunk| Ok(bytes::Bytes::copy_from_slice(chunk)))
            .collect();
        let input = stream_sse_response(
            Box::pin(stream::iter(chunks)),
            ProtocolParser::new(Protocol::Responses),
            Arc::default(),
        );
        let events: Vec<_> = assemble_reply_stream(None, input, StreamControl::default())
            .collect()
            .await;
        assert!(matches!(
            events.last(),
            Some(crate::LlmEvent::Finished {
                outcome: crate::ReplyOutcome::Complete { .. },
                ..
            })
        ));
        for protocol in [
            Protocol::Responses,
            Protocol::Interactions,
            Protocol::ChatCompletions,
            Protocol::Messages,
            Protocol::GenerateContent,
        ] {
            let parser = ProtocolParser::new(protocol);
            let limit = parser.buffer_limit();
            assert_eq!(
                limit,
                if protocol == Protocol::Responses {
                    64 * 1024 * 1024
                } else if protocol == Protocol::Interactions {
                    16 * 1024 * 1024
                } else {
                    2 * 1024 * 1024
                }
            );
            let bytes = bytes::Bytes::from(vec![b' '; limit + 1]);
            let input =
                stream_sse_response(Box::pin(stream::iter([Ok(bytes)])), parser, Arc::default());
            let events: Vec<_> = input.collect().await;
            assert!(
                matches!(&events[..], [ProtocolEvent::Failed(StreamStop::Provider(failure))] if !failure.retryable && failure.message.contains("buffer size exceeded"))
            );
        }
    }

    #[test]
    fn drain_preserves_split_utf8_across_chunks() {
        // "你好" is 6 bytes: [0xE4,0xBD,0xA0, 0xE5,0xA5,0xBD]
        // We build a valid OpenAI SSE event with Chinese content, then split
        // the raw bytes inside a multi-byte character to prove no corruption.
        let han = "你好";
        let event = format!(
            "data: {{\"choices\":[{{\"delta\":{{\"content\":\"{}\"}}}}]}}\n\n",
            han
        );
        let full_bytes = event.as_bytes();

        // Split inside the first byte of '好' (byte offset within the JSON)
        let han_start = full_bytes
            .windows(3)
            .position(|w| w == &han.as_bytes()[3..6])
            .unwrap();
        let split_point = han_start + 1; // mid-character

        let http_chunk1 = &full_bytes[..split_point];
        let http_chunk2 = &full_bytes[split_point..];

        let mut state = SseStreamState {
            body_stream: Box::pin(stream::empty()),
            parser: ProtocolParser::new(Protocol::ChatCompletions),
            client: Arc::default(),
            buffer: Vec::new(),
            scanned: 0,
            event_count: 0,
            finished: false,
            pending: std::collections::VecDeque::new(),
        };

        // First chunk — incomplete UTF-8, no event boundary yet
        state.buffer.extend_from_slice(http_chunk1);
        drain_complete_events(&mut state);
        assert!(state.pending.is_empty());

        // Second chunk — completes the character and closes the event
        state.buffer.extend_from_slice(http_chunk2);
        drain_complete_events(&mut state);
        assert!(!state.pending.is_empty());

        match state
            .pending
            .iter()
            .find(|event| {
                matches!(
                    event,
                    ProtocolEvent::Part(crate::reply::PartDelta::Text { .. })
                )
            })
            .expect("one translated text delta")
        {
            ProtocolEvent::Part(crate::reply::PartDelta::Text { delta: content, .. }) => {
                assert!(content.contains(han))
            }
            other => panic!("expected Token event, got: {:?}", other),
        }
    }

    #[test]
    fn drain_handles_crlf_event_separator() {
        let mut state = SseStreamState {
            body_stream: Box::pin(stream::empty()),
            parser: ProtocolParser::new(Protocol::ChatCompletions),
            client: Arc::default(),
            buffer: Vec::new(),
            scanned: 0,
            event_count: 0,
            finished: false,
            pending: std::collections::VecDeque::new(),
        };

        // Two events separated by \r\n\r\n
        let raw = b"data: {\"choices\":[{\"delta\":{\"content\":\"A\"}}]}\r\n\r\ndata: {\"choices\":[{\"delta\":{\"content\":\"B\"}}]}\r\n\r\n";
        state.buffer.extend_from_slice(raw);
        drain_complete_events(&mut state);
        assert_eq!(state.event_count, 2);
        assert_eq!(
            state
                .pending
                .iter()
                .filter(|event| matches!(
                    event,
                    ProtocolEvent::Part(crate::reply::PartDelta::Text { .. })
                ))
                .count(),
            2
        );
    }
}
