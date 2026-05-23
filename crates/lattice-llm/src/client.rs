//! Concrete provider dispatch for LLM execution.

use std::pin::Pin;

use futures::stream::{self, StreamExt};

use crate::providers;
use crate::router::{ProviderKind, ResolvedModel};
use crate::security;
use crate::sse;
use crate::stub::{LlmError, LlmStream};
use crate::types::{ChatRequest, LlmEvent};

/// Validate base URL and read the API key from the environment.
pub fn validate_and_read_key(resolved: &ResolvedModel) -> Result<String, LlmError> {
    security::validate_base_url(&resolved.base_url)?;

    let key = std::env::var(&resolved.api_key_env).map_err(|_| {
        LlmError::Other(format!(
            "missing API key: environment variable '{}' is not set",
            resolved.api_key_env
        ))
    })?;

    if key.is_empty() {
        return Err(LlmError::Other(format!(
            "missing API key: environment variable '{}' is empty",
            resolved.api_key_env
        )));
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
fn build_request_body(request: &ChatRequest, resolved: &ResolvedModel) -> serde_json::Value {
    match resolved.provider {
        ProviderKind::OpenAi | ProviderKind::OpenAiCompatible => {
            providers::openai::build_request_json(request, resolved)
        }
        ProviderKind::Anthropic => providers::anthropic::build_request_json(request, resolved),
        ProviderKind::Gemini => providers::gemini::build_request_json(request, resolved),
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
/// sequential requests on the same tokio worker thread (H-01).
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
    total_bytes: usize,
    event_count: usize,
    finished: bool,
    pending: std::collections::VecDeque<LlmEvent>,
}

/// Stream SSE events incrementally from an HTTP response into [`LlmEvent`] values.
///
/// Each call creates its own parser instance — no thread-local state leaks
/// across requests. Body chunks are read incrementally; if the cumulative
/// SSE buffer exceeds `MAX_SSE_BUFFER_SIZE` or the event count exceeds
/// `MAX_SSE_EVENTS`, an error event is emitted and the stream terminates.
async fn stream_sse_response(response: reqwest::Response, provider: ProviderKind) -> LlmStream {
    let body_stream = Box::pin(response.bytes_stream());
    let state = SseStreamState {
        body_stream,
        parser: ProviderParser::new(provider),
        buffer: Vec::new(),
        total_bytes: 0,
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
            return Some((event, state));
        }

        // Pull more chunks until we produce at least one event or exhaust
        // the body.
        loop {
            match state.body_stream.next().await {
                Some(Ok(chunk)) => {
                    state.total_bytes = state.total_bytes.saturating_add(chunk.len());
                    if state.total_bytes > sse::MAX_SSE_BUFFER_SIZE {
                        state.finished = true;
                        return Some((
                            LlmEvent::Error {
                                message: format!(
                                    "SSE buffer size exceeded: {} bytes",
                                    state.total_bytes
                                ),
                            },
                            state,
                        ));
                    }
                    state.buffer.extend_from_slice(&chunk);
                    drain_complete_events(&mut state);
                    if let Some(event) = state.pending.pop_front() {
                        return Some((event, state));
                    }
                }
                Some(Err(e)) => {
                    state.finished = true;
                    return Some((
                        LlmEvent::Error {
                            message: format!("HTTP response read error: {}", e),
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
            state.pending.push_back(LlmEvent::Error {
                message: format!("SSE event count exceeded: {}", sse::MAX_SSE_EVENTS),
            });
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

fn build_http_client_for_url(url: &str) -> Result<reqwest::Client, LlmError> {
    let Some((host, port)) = parsed_request_host_port(url) else {
        return Ok(reqwest::Client::new());
    };

    if security::host_is_dns_rebinding_domain(&host) {
        return Err(LlmError::Other(format!(
            "hostname '{}' is a known DNS rebinding domain and is not allowed",
            host
        )));
    }

    if is_localhost_request_host(&host) {
        return Ok(reqwest::Client::new());
    }

    let pinned = security::resolve_public_socket_addrs(&host, port)?;
    reqwest::Client::builder()
        .resolve_to_addrs(host.as_str(), &pinned)
        .build()
        .map_err(|e| LlmError::Other(format!("failed to build HTTP client: {}", e)))
}

/// Dispatch a chat request to the appropriate provider and return a stream.
pub async fn dispatch_stream(
    request: ChatRequest,
    resolved: ResolvedModel,
) -> Result<LlmStream, LlmError> {
    let api_key = validate_and_read_key(&resolved)?;

    let url = build_endpoint_url(&resolved);
    let body = build_request_body(&request, &resolved);
    let headers = build_auth_headers(&resolved, &api_key);

    let client = build_http_client_for_url(&url)?;
    let mut req_builder = client.post(&url).json(&body);

    for (name, value) in &headers {
        req_builder = req_builder.header(name.as_str(), value.as_str());
    }

    let response = req_builder
        .send()
        .await
        .map_err(|e| LlmError::Other(format!("HTTP request failed: {}", e)))?;

    let status = response.status();
    if !status.is_success() {
        let status_text = response.text().await.unwrap_or_default();
        return Err(LlmError::Other(format!(
            "provider returned HTTP {}: {}",
            status.as_u16(),
            status_text
        )));
    }

    let provider = resolved.provider.clone();
    Ok(stream_sse_response(response, provider).await)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::endpoints::{ANTHROPIC_BASE_URL, GEMINI_BASE_URL, OPENAI_BASE_URL};
    use crate::router::{CredentialStatus, ModelRouter};
    use crate::types::{Message, ModelTarget};

    fn make_router() -> ModelRouter {
        ModelRouter::new()
    }

    #[test]
    fn missing_credential_returns_named_error() {
        let resolved = ResolvedModel {
            alias: "gpt-4o".to_string(),
            provider: ProviderKind::OpenAi,
            provider_model: "gpt-4o".to_string(),
            base_url: OPENAI_BASE_URL.to_string(),
            api_key_env: "OPENAI_API_KEY_MISSING_TEST".to_string(),
            credential_status: CredentialStatus::Missing,
        };
        let result = validate_and_read_key(&resolved);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("OPENAI_API_KEY_MISSING_TEST"),
            "error should name the env var, got: {}",
            err
        );
    }

    #[test]
    fn present_credential_reads_key_value() {
        std::env::set_var("LATTICE_TEST_CLIENT_KEY", "sk-secret-do-not-leak");
        let resolved = ResolvedModel {
            alias: "gpt-4o".to_string(),
            provider: ProviderKind::OpenAi,
            provider_model: "gpt-4o".to_string(),
            base_url: OPENAI_BASE_URL.to_string(),
            api_key_env: "LATTICE_TEST_CLIENT_KEY".to_string(),
            credential_status: CredentialStatus::Present,
        };
        let result = validate_and_read_key(&resolved);
        assert!(result.is_ok());
        let key = result.unwrap();
        assert_eq!(key, "sk-secret-do-not-leak");
        std::env::remove_var("LATTICE_TEST_CLIENT_KEY");
    }

    #[test]
    fn missing_credential_error_never_leaks_key_value() {
        std::env::set_var("LATTICE_TEST_OK_KEY", "sk-ok-value");
        let resolved = ResolvedModel {
            alias: "gpt-4o".to_string(),
            provider: ProviderKind::OpenAi,
            provider_model: "gpt-4o".to_string(),
            base_url: OPENAI_BASE_URL.to_string(),
            api_key_env: "LATTICE_TEST_MISSING_KEY".to_string(),
            credential_status: CredentialStatus::Missing,
        };
        let result = validate_and_read_key(&resolved);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            !err.contains("sk-ok-value"),
            "error must not leak other keys: {}",
            err
        );
        assert!(
            err.contains("LATTICE_TEST_MISSING_KEY"),
            "error should name the missing env var"
        );
        std::env::remove_var("LATTICE_TEST_OK_KEY");
    }

    #[test]
    fn empty_credential_returns_error() {
        std::env::set_var("LATTICE_TEST_EMPTY_KEY", "");
        let resolved = ResolvedModel {
            alias: "gpt-4o".to_string(),
            provider: ProviderKind::OpenAi,
            provider_model: "gpt-4o".to_string(),
            base_url: OPENAI_BASE_URL.to_string(),
            api_key_env: "LATTICE_TEST_EMPTY_KEY".to_string(),
            credential_status: CredentialStatus::Missing,
        };
        let result = validate_and_read_key(&resolved);
        assert!(result.is_err());
        std::env::remove_var("LATTICE_TEST_EMPTY_KEY");
    }

    #[test]
    fn dispatch_does_not_use_registry_or_plugins() {
        let router = make_router();
        let target = ModelTarget::new("gpt-5.5");
        let resolved = router.resolve(&target).unwrap();
        assert_eq!(resolved.provider, ProviderKind::OpenAi);
        let req = ChatRequest::new(vec![Message::user("hi")], target);
        let json = build_request_body(&req, &resolved);
        assert_eq!(json["model"], "gpt-5.5");
    }

    #[test]
    fn dispatch_requires_base_url_validation() {
        let resolved = ResolvedModel {
            alias: "bad".to_string(),
            provider: ProviderKind::OpenAiCompatible,
            provider_model: "bad".to_string(),
            base_url: "http://10.0.0.1/v1".to_string(),
            api_key_env: "OPENAI_COMPATIBLE_API_KEY".to_string(),
            credential_status: CredentialStatus::Missing,
        };
        let result = validate_and_read_key(&resolved);
        assert!(result.is_err(), "private IP base URL should be rejected");
    }

    #[test]
    fn openai_auth_headers_use_bearer() {
        let resolved = ResolvedModel {
            alias: "gpt-4o".to_string(),
            provider: ProviderKind::OpenAi,
            provider_model: "gpt-4o".to_string(),
            base_url: OPENAI_BASE_URL.to_string(),
            api_key_env: "OPENAI_API_KEY".to_string(),
            credential_status: CredentialStatus::Present,
        };
        let headers = build_auth_headers(&resolved, "sk-test");
        assert_eq!(headers.len(), 1);
        assert_eq!(headers[0].0, "Authorization");
        assert_eq!(headers[0].1, "Bearer sk-test");
    }

    #[test]
    fn anthropic_auth_headers_use_x_api_key() {
        let resolved = ResolvedModel {
            alias: "sonnet".to_string(),
            provider: ProviderKind::Anthropic,
            provider_model: "claude-sonnet-4-6".to_string(),
            base_url: ANTHROPIC_BASE_URL.to_string(),
            api_key_env: "ANTHROPIC_API_KEY".to_string(),
            credential_status: CredentialStatus::Present,
        };
        let headers = build_auth_headers(&resolved, "sk-ant-test");
        assert_eq!(headers.len(), 2);
        assert_eq!(headers[0].0, "x-api-key");
        assert_eq!(headers[0].1, "sk-ant-test");
        assert_eq!(headers[1].0, "anthropic-version");
        assert_eq!(headers[1].1, "2023-06-01");
    }

    #[test]
    fn gemini_auth_headers_use_x_goog_api_key() {
        let resolved = ResolvedModel {
            alias: "gemini".to_string(),
            provider: ProviderKind::Gemini,
            provider_model: "gemini-2.5-flash".to_string(),
            base_url: GEMINI_BASE_URL.to_string(),
            api_key_env: "GEMINI_API_KEY".to_string(),
            credential_status: CredentialStatus::Present,
        };
        let headers = build_auth_headers(&resolved, "gemini-test-key");
        assert_eq!(headers.len(), 1);
        assert_eq!(headers[0].0, "x-goog-api-key");
        assert_eq!(headers[0].1, "gemini-test-key");
    }

    #[test]
    fn openai_endpoint_url() {
        let resolved = ResolvedModel {
            alias: "gpt-4o".to_string(),
            provider: ProviderKind::OpenAi,
            provider_model: "gpt-4o".to_string(),
            base_url: OPENAI_BASE_URL.to_string(),
            api_key_env: "OPENAI_API_KEY".to_string(),
            credential_status: CredentialStatus::Present,
        };
        let url = build_endpoint_url(&resolved);
        assert_eq!(url, format!("{}/chat/completions", OPENAI_BASE_URL));
    }

    #[test]
    fn anthropic_endpoint_url() {
        let resolved = ResolvedModel {
            alias: "sonnet".to_string(),
            provider: ProviderKind::Anthropic,
            provider_model: "claude-sonnet-4-6".to_string(),
            base_url: ANTHROPIC_BASE_URL.to_string(),
            api_key_env: "ANTHROPIC_API_KEY".to_string(),
            credential_status: CredentialStatus::Present,
        };
        let url = build_endpoint_url(&resolved);
        assert_eq!(url, format!("{}/v1/messages", ANTHROPIC_BASE_URL));
    }

    #[test]
    fn gemini_endpoint_url() {
        let resolved = ResolvedModel {
            alias: "gemini".to_string(),
            provider: ProviderKind::Gemini,
            provider_model: "gemini-2.5-flash".to_string(),
            base_url: GEMINI_BASE_URL.to_string(),
            api_key_env: "GEMINI_API_KEY".to_string(),
            credential_status: CredentialStatus::Present,
        };
        let url = build_endpoint_url(&resolved);
        assert!(url.contains("streamGenerateContent"));
        assert!(url.contains("alt=sse"));
    }

    #[test]
    fn find_event_boundary_lf_lf() {
        let buf = b"data: hello\n\ndata: world\n\n";
        let (idx, len) = find_event_boundary(buf).unwrap();
        assert_eq!(idx, 11);
        assert_eq!(len, 2);
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
            total_bytes: 0,
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
            total_bytes: 0,
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
