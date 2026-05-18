//! Concrete provider dispatch for v1.1 LLM execution.
//!
//! Routes `ChatRequest` → `LlmStream` through a simple `match` on
//! `ProviderKind`. No transport registry, provider plugin trait, catalog,
//! or `lattice-core` dependency exists here.

use futures::stream::{self};

use crate::providers;
use crate::router::{ProviderKind, ResolvedModel};
use crate::security;
use crate::sse;
use crate::stub::{LlmError, LlmStream};
use crate::types::{ChatRequest, LlmEvent};

/// Validate base URL and read the API key from the environment.
///
/// Returns `Ok(key)` if the base URL passes SSRF validation and the
/// environment variable is set and non-empty.
/// Returns `Err(LlmError)` on URL validation failure or missing credential.
///
/// Never logs or serializes the key value. Errors mention only the
/// environment variable name.
pub fn validate_and_read_key(resolved: &ResolvedModel) -> Result<String, LlmError> {
    // Validate the base URL first (SSRF safety floor)
    security::validate_base_url(&resolved.base_url)?;

    // Read the API key from the environment
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

/// Stream SSE events from an HTTP response into [`LlmEvent`] values.
///
/// Collects the full response body, parses SSE events from it, and returns
/// a stream of parsed [`LlmEvent`] values.
async fn stream_sse_response(response: reqwest::Response, provider: ProviderKind) -> LlmStream {
    // Collect the full response body (bounded by safety limits)
    let bytes = match response.bytes().await {
        Ok(b) => b,
        Err(e) => {
            return Box::pin(stream::iter(vec![LlmEvent::Error {
                message: format!("HTTP response read error: {}", e),
            }]));
        }
    };

    if bytes.len() > sse::MAX_SSE_BUFFER_SIZE {
        return Box::pin(stream::iter(vec![LlmEvent::Error {
            message: format!("SSE buffer size exceeded: {} bytes", bytes.len()),
        }]));
    }

    let body_text = String::from_utf8_lossy(&bytes);
    let mut events: Vec<LlmEvent> = Vec::new();
    let mut event_count: usize = 0;

    // Split on double-newline for SSE event boundaries
    for event_text in body_text.split("\n\n") {
        if event_text.trim().is_empty() {
            continue;
        }
        if event_count >= sse::MAX_SSE_EVENTS {
            events.push(LlmEvent::Error {
                message: format!("SSE event count exceeded: {}", sse::MAX_SSE_EVENTS),
            });
            break;
        }
        event_count += 1;

        let parsed = parse_provider_sse(event_text, provider.clone());
        events.extend(parsed);
    }

    Box::pin(stream::iter(events))
}

/// Parse a single SSE event text block into [`LlmEvent`] values.
///
/// Uses a per-type parser (not stateful across the entire stream for this
/// implementation; state is scoped to each provider's parser).
fn parse_provider_sse(event_text: &str, provider: ProviderKind) -> Vec<LlmEvent> {
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

    if data.is_empty() {
        return vec![];
    }

    match provider {
        ProviderKind::OpenAi | ProviderKind::OpenAiCompatible => {
            // Use thread-local parser for state tracking
            thread_local! {
                static OPENAI_PARSER: std::cell::RefCell<providers::openai::OpenAiSseParser> =
                    std::cell::RefCell::new(providers::openai::OpenAiSseParser::new());
            }
            OPENAI_PARSER.with(|p| p.borrow_mut().parse_chunk(event_type, &data))
        }
        ProviderKind::Anthropic => {
            thread_local! {
                static ANTHROPIC_PARSER: std::cell::RefCell<providers::anthropic::AnthropicSseParser> =
                    std::cell::RefCell::new(providers::anthropic::AnthropicSseParser::new());
            }
            ANTHROPIC_PARSER.with(|p| p.borrow_mut().parse_chunk(event_type.unwrap_or(""), &data))
        }
        ProviderKind::Gemini => {
            thread_local! {
                static GEMINI_PARSER: std::cell::RefCell<providers::gemini::GeminiSseParser> =
                    std::cell::RefCell::new(providers::gemini::GeminiSseParser::new());
            }
            GEMINI_PARSER.with(|p| p.borrow_mut().parse_chunk(event_type, &data))
        }
    }
}

/// Dispatch a chat request to the appropriate provider and return a stream.
///
/// Uses a concrete `match resolved.provider` to select the provider path.
/// No registry, catalog, or plugin mechanism is involved.
pub async fn dispatch_stream(
    request: ChatRequest,
    resolved: ResolvedModel,
) -> Result<LlmStream, LlmError> {
    // Validate base URL and read credentials
    let api_key = validate_and_read_key(&resolved)?;

    // Build the request
    let url = build_endpoint_url(&resolved);
    let body = build_request_body(&request, &resolved);
    let headers = build_auth_headers(&resolved, &api_key);

    // Send the HTTP request
    let client = reqwest::Client::new();
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

    // Stream SSE events
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

    // ── Credential validation ─────────────────────────────────────────────

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
        // Even when the env var is set elsewhere, make sure the error
        // message for a DIFFERENT missing env var doesn't leak values
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

    // ── Provider dispatch by kind ────────────────────────────────────────

    #[test]
    fn dispatch_does_not_use_registry_or_plugins() {
        let router = make_router();
        let target = ModelTarget::new("gpt-5.5");
        let resolved = router.resolve(&target).unwrap();
        assert_eq!(resolved.provider, ProviderKind::OpenAi);
        // If this compiles without lattice_core, the test passes.
        // Verify build_request_body dispatches by kind
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
        // Private IP URL should be rejected by base URL validation
        assert!(result.is_err(), "private IP base URL should be rejected");
    }

    // ── Auth header construction ──────────────────────────────────────────

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

    // ── Endpoint URL construction ────────────────────────────────────────

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
}
