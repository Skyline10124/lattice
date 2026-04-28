pub mod anthropic;
pub mod deepseek;
pub mod gemini;
pub mod groq;
pub mod mistral;
pub mod ollama;
pub mod openai;
pub mod xai;

use crate::provider::{ChatRequest, ChatResponse, ProviderError};
use crate::transport::chat_completions::{ChatCompletionsTransport, Transport};

/// Shared `chat()` implementation for OpenAI-compatible providers.
///
/// Handles the common flow: normalize the request, POST to
/// `{base_url}/chat/completions` with an optional Bearer auth header,
/// check status, parse the JSON response, and denormalize into a
/// [`ChatResponse`].
///
/// Each provider resolves its `base_url` (falling back to a provider-specific
/// default if `resolved.base_url` is empty) and passes it in.
pub async fn openai_compat_chat(
    transport: &ChatCompletionsTransport,
    request: &ChatRequest,
    base_url: &str,
) -> Result<ChatResponse, ProviderError> {
    let resolved = &request.resolved;

    let mut body = transport
        .normalize_request(request)
        .map_err(|e| ProviderError::General(e.to_string()))?;

    // Ensure stream is explicitly false for non-streaming chat.
    body["stream"] = serde_json::Value::Bool(false);

    let client = crate::provider::shared_http_client();
    let mut req = client
        .post(format!("{}/chat/completions", base_url))
        .json(&body);

    if let Some(ref api_key) = resolved.api_key {
        req = req.header("Authorization", format!("Bearer {}", api_key));
    }

    let resp = req
        .send()
        .await
        .map_err(|e| ProviderError::General(format!("HTTP request failed: {}", e)))?;

    let status = resp.status();
    if !status.is_success() {
        let text = resp
            .text()
            .await
            .map_err(|e| ProviderError::General(format!("Failed to read response body: {}", e)))?;
        return Err(ProviderError::Api(text));
    }

    let json: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| ProviderError::General(format!("Failed to parse response JSON: {}", e)))?;

    let response = transport
        .denormalize_response(&json)
        .map_err(|e| ProviderError::General(e.to_string()))?;

    Ok(response)
}
