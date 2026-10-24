// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Concrete provider dispatch for LLM execution.

use std::pin::Pin;
use std::sync::LazyLock;
use std::time::{Duration, Instant};

use futures::stream::{self, StreamExt};
use net_guard::NetGuardError;

use crate::providers;
use crate::router::{ProviderKind, ResolvedModel};
use crate::sse;
use crate::stub::{LlmError, LlmStream};
use crate::transport;
use crate::types::{ChatRequest, LlmEvent};

/// 兜底总时限，与 profile `timeout_secs` 的上限一致；流的实际时限由 Turn deadline 约束。
const HTTP_REQUEST_BACKSTOP: Duration = Duration::from_secs(3600);

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

/// Validate base URL and read the API key from the environment.
pub fn validate_and_read_key(resolved: &ResolvedModel) -> Result<String, LlmError> {
    net_guard::validate_base_url(&resolved.base_url).map_err(llm_error_from_net_guard)?;

    let key = std::env::var(&resolved.api_key_env).map_err(|_| LlmError::Credential {
        message: format!(
            "missing API key: environment variable '{}' is not set",
            resolved.api_key_env
        ),
    })?;

    if key.is_empty() {
        return Err(LlmError::Credential {
            message: format!(
                "missing API key: environment variable '{}' is empty",
                resolved.api_key_env
            ),
        });
    }

    Ok(key)
}

/// Build provider-specific HTTP headers from a resolved model and API key.
fn build_auth_headers(resolved: &ResolvedModel, api_key: &str) -> Vec<(String, String)> {
    match resolved.provider {
        ProviderKind::OpenAi | ProviderKind::OpenAiCompatible => {
            vec![(
                "Authorization".to_string(),
                providers::openai::auth_header_value(api_key),
            )]
        }
        ProviderKind::Anthropic => {
            vec![
                (
                    "x-api-key".to_string(),
                    providers::anthropic::auth_header_value(api_key),
                ),
                (
                    "anthropic-version".to_string(),
                    providers::anthropic::ANTHROPIC_VERSION.to_string(),
                ),
            ]
        }
        ProviderKind::Gemini => {
            vec![(
                "x-goog-api-key".to_string(),
                providers::gemini::auth_header_value(api_key),
            )]
        }
    }
}

/// Build the request body JSON for a given provider kind.
fn build_request_body(
    request: &ChatRequest,
    resolved: &ResolvedModel,
) -> Result<serde_json::Value, LlmError> {
    match resolved.provider {
        ProviderKind::OpenAi | ProviderKind::OpenAiCompatible => {
            providers::openai::build_request_json(request, resolved).map_err(Into::into)
        }
        ProviderKind::Anthropic => {
            providers::anthropic::build_request_json(request, resolved).map_err(Into::into)
        }
        ProviderKind::Gemini => {
            providers::gemini::build_request_json(request, resolved).map_err(Into::into)
        }
    }
}

/// Build the endpoint URL for a given provider kind.
fn build_endpoint_url(resolved: &ResolvedModel) -> String {
    match resolved.provider {
        ProviderKind::OpenAi | ProviderKind::OpenAiCompatible => {
            format!(
                "{}/chat/completions",
                resolved.base_url.trim_end_matches('/')
            )
        }
        ProviderKind::Anthropic => {
            format!("{}/v1/messages", resolved.base_url.trim_end_matches('/'))
        }
        ProviderKind::Gemini => {
            providers::gemini::build_stream_url(&resolved.base_url, &resolved.provider_model)
        }
    }
}

/// Per-request provider parser state. Each `stream_sse_response` call
/// owns its own parser, preventing state leakage across concurrent or
/// sequential requests on the same tokio worker thread.
enum ProviderParser {
    OpenAi(providers::openai::OpenAiSseParser),
    Anthropic(providers::anthropic::AnthropicSseParser),
    Gemini(providers::gemini::GeminiSseParser),
}

impl ProviderParser {
    fn new(provider: ProviderKind) -> Self {
        match provider {
            ProviderKind::OpenAi | ProviderKind::OpenAiCompatible => {
                ProviderParser::OpenAi(providers::openai::OpenAiSseParser::new())
            }
            ProviderKind::Anthropic => {
                ProviderParser::Anthropic(providers::anthropic::AnthropicSseParser::new())
            }
            ProviderKind::Gemini => {
                ProviderParser::Gemini(providers::gemini::GeminiSseParser::new())
            }
        }
    }

    fn parse_event(&mut self, event_text: &str) -> Vec<LlmEvent> {
        let (event_type, data) = parse_sse_event_block(event_text);
        if data.is_empty() {
            return Vec::new();
        }
        match self {
            ProviderParser::OpenAi(p) => p.parse_chunk(event_type, &data),
            ProviderParser::Anthropic(p) => p.parse_chunk(event_type.unwrap_or(""), &data),
            ProviderParser::Gemini(p) => p.parse_chunk(event_type, &data),
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
    parser: ProviderParser,
    buffer: Vec<u8>,
    event_count: usize,
    finished: bool,
    pending: std::collections::VecDeque<LlmEvent>,
}

/// Stream SSE events incrementally from an HTTP response into [`LlmEvent`] values.
///
/// Each call creates its own parser instance — no thread-local state leaks
/// across requests. Body chunks are read incrementally; if the in-flight
/// unframed SSE buffer exceeds `MAX_SSE_BUFFER_SIZE` or the event count exceeds
/// `MAX_SSE_EVENTS`, an error event is emitted and the stream terminates.
fn stream_sse_response(body_stream: BodyStream, provider: ProviderKind) -> LlmStream {
    let state = SseStreamState {
        body_stream,
        parser: ProviderParser::new(provider),
        buffer: Vec::new(),
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
                    if state.buffer.len() > sse::MAX_SSE_BUFFER_SIZE {
                        state.finished = true;
                        return Some((
                            LlmEvent::error(
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
                            LlmEvent::error(error.to_string(), true)
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
async fn deliver(mut state: SseStreamState, event: LlmEvent) -> (LlmEvent, SseStreamState) {
    const TAIL_WAIT: Duration = Duration::from_millis(500);
    const TAIL_BYTES: usize = 64 * 1024;
    if matches!(event, LlmEvent::Done { .. }) && !state.finished {
        state.finished = true;
        let body = &mut state.body_stream;
        let _ = tokio::time::timeout(TAIL_WAIT, async {
            let mut read = 0;
            while let Some(Ok(chunk)) = body.next().await {
                read += chunk.len();
                if read > TAIL_BYTES {
                    break;
                }
            }
        })
        .await;
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
        let sep_len = match find_event_boundary(&state.buffer) {
            Some(v) => v,
            None => return,
        };
        let (idx, slen) = sep_len;
        let event_bytes: Vec<u8> = state.buffer.drain(..idx).collect();
        // Drop the separator.
        state.buffer.drain(..slen);

        let event_text = String::from_utf8_lossy(&event_bytes);
        if event_text.trim().is_empty() {
            continue;
        }
        if state.event_count >= sse::MAX_SSE_EVENTS {
            state.pending.push_back(LlmEvent::error(
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
fn find_event_boundary(buf: &[u8]) -> Option<(usize, usize)> {
    let mut i = 0;
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
    let Some((host, port)) = parsed_request_host_port(url) else {
        return Err(LlmError::Config {
            message: "invalid provider URL".into(),
        });
    };
    if net_guard::host_is_dns_rebinding_domain(&host) {
        return Err(LlmError::Config {
            message: "DNS rebinding hostname is not allowed".into(),
        });
    }
    let builder = if is_localhost_request_host(&host) {
        builder
    } else {
        let pinned = net_guard::resolve_public_socket_addrs(&host, port)
            .map_err(llm_error_from_net_guard)?;
        builder.resolve_to_addrs(host.as_str(), &pinned)
    };
    builder.build().map_err(|_| LlmError::Config {
        message: "failed to build HTTP client".into(),
    })
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

/// 读最多 64 KiB 错误正文；第二项表示读到一半连接断了。
async fn read_error_body_capped(response: reqwest::Response) -> (String, bool) {
    let mut stream = response.bytes_stream();
    let mut body = Vec::new();

    while let Some(chunk_result) = stream.next().await {
        match chunk_result {
            Ok(chunk) => {
                if append_bounded_error_chunk(&mut body, &chunk) {
                    break;
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

/// 兼容单进程调用者；后台执行必须传入已经冻结的 LlmConnection。
pub async fn dispatch_stream(
    request: ChatRequest,
    resolved: ResolvedModel,
) -> Result<LlmStream, LlmError> {
    crate::LlmConnection::from_resolved_env(resolved)?
        .stream(request)
        .await
}

/// 取本 Turn 缓存的 client；没有时在 blocking 线程上解析地址并构建。
async fn turn_client(
    connection: &crate::LlmConnection,
    url: &str,
) -> Result<reqwest::Client, LlmError> {
    if let Some(client) = connection.client.lock().unwrap().clone() {
        return Ok(client);
    }
    let (url, roots) = (url.to_owned(), connection.roots.clone());
    let client = tokio::task::spawn_blocking(move || provider_client(&url, roots))
        .await
        .map_err(|_| LlmError::Config {
            message: "provider client setup task failed".into(),
        })??;
    *connection.client.lock().unwrap() = Some(client.clone());
    Ok(client)
}

pub(crate) async fn dispatch_frozen(
    request: ChatRequest,
    connection: &crate::LlmConnection,
) -> Result<LlmStream, LlmError> {
    let resolved = &connection.resolved;
    let provider = resolved.provider.as_str();
    let url = build_endpoint_url(resolved);
    let body = build_request_body(&request, resolved)?;
    let headers = build_auth_headers(resolved, &connection.api_key);
    let client = turn_client(connection, &url).await?;
    let mut req_builder = client
        .post(&url)
        .header(reqwest::header::ACCEPT, ACCEPT)
        .json(&body);
    if let Some(id) = &request.client_request_id {
        req_builder = req_builder.header("x-client-request-id", id.as_str());
    }
    for (name, value) in &headers {
        req_builder = req_builder.header(name.as_str(), value.as_str());
    }
    let started = Instant::now();
    let response = match req_builder.send().await {
        Ok(response) => response,
        Err(error) => {
            let error = llm_error_from_request(error);
            // 连接可能已不可用；下次尝试重新解析地址并新建 client。
            connection.forget_client();
            tracing::warn!(provider, %error, "provider request failed before a response");
            return Err(error);
        }
    };
    let status = response.status();
    let request_id = transport::provider_request_id(response.headers());
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
        if cut {
            connection.forget_client();
        }
        let body = body.replace(connection.api_key.as_str(), "[REDACTED]");
        let error = transport::status_error(status.as_u16(), &response_headers, &body);
        tracing::warn!(provider, request_id = request_id.as_deref(), %error, "provider returned an error status");
        return Err(error);
    }
    let mut guard = ForgetUnlessDone {
        client: connection.client.clone(),
        done: false,
    };
    let body_stream = response.bytes_stream();
    let stream = stream_sse_response(Box::pin(body_stream), resolved.provider.clone());
    let api_key = connection.api_key.clone();
    let provider = provider.to_owned();
    // provider 的流内错误同样会进入公开诊断和 Turn 日志，必须在越过适配边界前脱敏。
    Ok(Box::pin(stream.map(move |event| match event {
        LlmEvent::Error { mut failure } => {
            failure.message =
                transport::bound_diagnostic(&failure.message.replace(api_key.as_str(), "[REDACTED]"));
            tracing::warn!(provider, request_id = request_id.as_deref(), error = %failure.message, "provider stream error");
            LlmEvent::Error { failure }
        }
        LlmEvent::Done { finish_reason, usage } => {
            guard.finish();
            tracing::debug!(
                provider,
                request_id = request_id.as_deref(),
                total_ms = started.elapsed().as_millis() as u64,
                "provider stream completed"
            );
            LlmEvent::Done { finish_reason, usage }
        }
        other => other,
    })))
}

/// 随响应流一起丢弃。流没交付 `Done` 就结束、出错或被提前丢弃时，这条连接的状态不可信：
/// 丢掉本 Turn 的 client，下次尝试重新解析并固定地址。
struct ForgetUnlessDone {
    client: std::sync::Arc<std::sync::Mutex<Option<reqwest::Client>>>,
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
            self.client.lock().unwrap().take();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::endpoints::OPENAI_BASE_URL;

    use crate::router::CredentialStatus;
    use crate::test_env::with_env_vars;

    #[test]
    fn credential_validation_rejects_missing_keys_and_unsafe_urls() {
        for (
            name,
            env_vars,
            alias,
            provider,
            base_url,
            api_key_env,
            error_contains,
            error_excludes,
        ) in [
            (
                "missing_credential",
                vec![("LATTICE_TEST_OK_KEY", Some("sk-ok-value"))],
                "gpt-4o",
                ProviderKind::OpenAi,
                OPENAI_BASE_URL,
                "LATTICE_TEST_MISSING_KEY",
                Some("LATTICE_TEST_MISSING_KEY"),
                Some("sk-ok-value"),
            ),
            (
                "empty_credential",
                vec![("LATTICE_TEST_EMPTY_KEY", Some(""))],
                "gpt-4o",
                ProviderKind::OpenAi,
                OPENAI_BASE_URL,
                "LATTICE_TEST_EMPTY_KEY",
                None,
                None,
            ),
            (
                "private_ip_base_url",
                vec![],
                "bad",
                ProviderKind::OpenAiCompatible,
                "http://10.0.0.1/v1",
                "OPENAI_COMPATIBLE_API_KEY",
                None,
                None,
            ),
        ] {
            with_env_vars(&env_vars, || {
                let resolved = ResolvedModel {
                    alias: alias.to_string(),
                    provider,
                    provider_model: alias.to_string(),
                    base_url: base_url.to_string(),
                    api_key_env: api_key_env.to_string(),
                    credential_status: CredentialStatus::Missing,
                };
                let result = validate_and_read_key(&resolved);
                assert!(
                    result.is_err(),
                    "{name}: invalid credentials or base URL must be rejected"
                );
                if error_contains.is_some() || error_excludes.is_some() {
                    let err = result.unwrap_err().to_string();
                    if let Some(excluded) = error_excludes {
                        assert!(
                            !err.contains(excluded),
                            "{name}: error must not leak other keys: {err}"
                        );
                    }
                    if let Some(included) = error_contains {
                        assert!(
                            err.contains(included),
                            "{name}: error should name the missing env var"
                        );
                    }
                }
            });
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

    #[tokio::test]
    async fn explicit_connection_freezes_endpoint_credentials_and_redacts_errors() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            for (status, body) in [
                ("401 Unauthorized", "frozen-secret rejected"),
                (
                    "200 OK",
                    "data: {\"error\":{\"message\":\"frozen-secret rejected\"}}\n\n",
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
                assert!(
                    request
                        .to_ascii_lowercase()
                        .contains("authorization: bearer frozen-secret")
                );
                socket.write_all(format!("HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
            }
        });
        let resolved = ResolvedModel {
            alias: "compat:frozen-model".into(),
            provider: ProviderKind::OpenAiCompatible,
            provider_model: "frozen-model".into(),
            base_url: format!("http://{address}"),
            api_key_env: "UNUSED_FROZEN_CREDENTIAL".into(),
            credential_status: crate::CredentialStatus::Missing,
        };
        let connection =
            crate::LlmConnection::new(resolved, "frozen-secret".into(), Vec::new()).unwrap();
        let request = ChatRequest::new(Vec::new(), crate::ModelTarget::new("compat:frozen-model"));
        let error = match tokio::time::timeout(Duration::from_secs(3), connection.stream(request))
            .await
            .unwrap()
        {
            Ok(_) => panic!("expected provider failure"),
            Err(error) => error,
        };
        assert_eq!(error.status_code(), Some(401));
        assert!(!format!("{error:?}").contains("frozen-secret"));
        let request = ChatRequest::new(Vec::new(), crate::ModelTarget::new("compat:frozen-model"));
        let events = tokio::time::timeout(Duration::from_secs(3), async {
            connection
                .stream(request)
                .await
                .unwrap()
                .collect::<Vec<_>>()
                .await
        })
        .await
        .unwrap();
        assert!(
            matches!(events.as_slice(), [LlmEvent::Error { failure }] if failure.message == "API error: [REDACTED] rejected" && !failure.retryable)
        );
        server.await.unwrap();
    }

    /// 流没有交付 `Done` 就结束、读到一半断开，或错误正文读到一半断开时，本 Turn 的 client
    /// 必须丢弃，下次尝试才会重新解析并固定地址；完整的流保留 client 给下一 Step 复用。
    /// E2E 观察不到地址是否重新解析，所以直接检查缓存。
    #[tokio::test]
    async fn turn_client_is_dropped_after_any_unfinished_response() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let done = "data: {\"choices\":[{\"delta\":{\"content\":\"ok\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n";
        let partial = "data: {\"choices\":[{\"delta\":{\"content\":\"par\"}}]}\n\n";
        let sse = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n";
        let cases = [
            (
                "complete stream",
                format!("{sse}Content-Length: {}\r\n\r\n{done}", done.len()),
                true,
            ),
            (
                "eof before done",
                format!("{sse}Connection: close\r\n\r\n{partial}"),
                false,
            ),
            (
                "body cut mid-stream",
                format!(
                    "{sse}Content-Length: {}\r\n\r\n{partial}",
                    partial.len() + 64
                ),
                false,
            ),
            (
                "error body cut",
                "HTTP/1.1 503 Service Unavailable\r\nContent-Length: 100\r\n\r\n{\"error\"".into(),
                false,
            ),
        ];
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let responses: Vec<String> = cases.iter().map(|case| case.1.clone()).collect();
        let server = tokio::spawn(async move {
            for response in responses {
                let (mut socket, _) = listener.accept().await.unwrap();
                // 读完整个请求再回应，关闭连接时不会因未读数据触发 RST 而吞掉响应。
                let mut request = Vec::new();
                let body_start = loop {
                    let mut chunk = [0; 4096];
                    let count = socket.read(&mut chunk).await.unwrap();
                    assert!(count > 0);
                    request.extend_from_slice(&chunk[..count]);
                    if let Some(end) = request.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
                        break end + 4;
                    }
                };
                let head = String::from_utf8_lossy(&request[..body_start]).to_ascii_lowercase();
                let length: usize = head
                    .lines()
                    .find_map(|line| line.strip_prefix("content-length:"))
                    .map_or(0, |value| value.trim().parse().unwrap());
                while request.len() < body_start + length {
                    let mut chunk = [0; 4096];
                    let count = socket.read(&mut chunk).await.unwrap();
                    assert!(count > 0);
                    request.extend_from_slice(&chunk[..count]);
                }
                socket.write_all(response.as_bytes()).await.unwrap();
            }
        });
        let resolved = ResolvedModel {
            alias: "compat:m".into(),
            provider: ProviderKind::OpenAiCompatible,
            provider_model: "m".into(),
            base_url: format!("http://{address}"),
            api_key_env: "UNUSED".into(),
            credential_status: crate::CredentialStatus::Missing,
        };
        for (name, _, keeps_client) in cases {
            let connection =
                crate::LlmConnection::new(resolved.clone(), "key".into(), Vec::new()).unwrap();
            let request = ChatRequest::new(Vec::new(), crate::ModelTarget::new("compat:m"));
            let outcome = tokio::time::timeout(Duration::from_secs(3), async {
                match connection.stream(request).await {
                    Ok(stream) => Ok(stream.collect::<Vec<_>>().await),
                    Err(error) => Err(error),
                }
            })
            .await
            .unwrap();
            if name == "body cut mid-stream" {
                assert!(
                    matches!(outcome.as_deref(), Ok([.., LlmEvent::Error { failure }]) if failure.retryable),
                    "{name}: a body cut short is a retryable transport failure: {outcome:?}"
                );
            }
            assert_eq!(
                connection.client.lock().unwrap().is_some(),
                keeps_client,
                "{name}: {outcome:?}"
            );
        }
        server.await.unwrap();
    }

    #[test]
    fn find_event_boundary_crlf_crlf() {
        let buf = b"data: hello\r\n\r\ndata: world\r\n\r\n";
        let (idx, len) = find_event_boundary(buf).unwrap();
        assert_eq!(idx, 11);
        assert_eq!(len, 4);
    }

    #[test]
    fn find_event_boundary_cr_cr() {
        let buf = b"data: hello\r\rdata: world\r\r";
        let (idx, len) = find_event_boundary(buf).unwrap();
        assert_eq!(idx, 11);
        assert_eq!(len, 2);
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
            parser: ProviderParser::new(ProviderKind::OpenAi),
            buffer: Vec::new(),
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

        match &state.pending[0] {
            LlmEvent::Token { content, .. } => assert!(content.contains(han)),
            other => panic!("expected Token event, got: {:?}", other),
        }
    }

    #[test]
    fn drain_handles_crlf_event_separator() {
        let mut state = SseStreamState {
            body_stream: Box::pin(stream::empty()),
            parser: ProviderParser::new(ProviderKind::OpenAi),
            buffer: Vec::new(),
            event_count: 0,
            finished: false,
            pending: std::collections::VecDeque::new(),
        };

        // Two events separated by \r\n\r\n
        let raw = b"data: {\"choices\":[{\"delta\":{\"content\":\"A\"}}]}\r\n\r\ndata: {\"choices\":[{\"delta\":{\"content\":\"B\"}}]}\r\n\r\n";
        state.buffer.extend_from_slice(raw);
        drain_complete_events(&mut state);
        assert_eq!(state.event_count, 2);
        assert_eq!(state.pending.len(), 2);
    }
}
