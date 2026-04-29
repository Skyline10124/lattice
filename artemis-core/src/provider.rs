use async_trait::async_trait;
use std::sync::LazyLock;
use std::time::Duration;

use crate::catalog::ResolvedModel;
use crate::streaming::{EventStream, TokenUsage};
use crate::types::{Message, ToolCall, ToolDefinition};

static SHARED_HTTP_CLIENT: LazyLock<reqwest::Client> = LazyLock::new(|| {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(30))
        .build()
        .expect("Failed to build shared reqwest::Client")
});

pub fn shared_http_client() -> &'static reqwest::Client {
    &SHARED_HTTP_CLIENT
}

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// Errors that can occur during provider operations.
#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    /// A general, non-specific provider error.
    #[error("Provider error: {0}")]
    General(String),

    /// An error returned by the upstream API.
    #[error("API error: {0}")]
    Api(String),

    /// An error during streaming.
    #[error("Stream error: {0}")]
    Stream(String),

    /// The requested provider was not found in the registry.
    #[error("Provider not found: {0}")]
    NotFound(String),
}

// ---------------------------------------------------------------------------
// Request / Response
// ---------------------------------------------------------------------------

/// A request to be sent to an LLM provider.
#[derive(Debug, Clone)]
pub struct ChatRequest {
    pub messages: Vec<Message>,
    pub tools: Vec<ToolDefinition>,
    pub model: String,
    pub temperature: Option<f64>,
    pub max_tokens: Option<u32>,
    pub stream: bool,
    pub resolved: ResolvedModel,
}

impl ChatRequest {
    /// Create a new ChatRequest with `model` derived from `resolved.api_model_id`.
    pub fn new(
        messages: Vec<Message>,
        tools: Vec<ToolDefinition>,
        resolved: ResolvedModel,
    ) -> Self {
        let model = resolved.api_model_id.clone();
        ChatRequest {
            messages,
            tools,
            model,
            temperature: None,
            max_tokens: None,
            stream: false,
            resolved,
        }
    }
}

/// A response received from an LLM provider.
#[derive(Debug, Clone)]
pub struct ChatResponse {
    pub content: Option<String>,
    pub tool_calls: Option<Vec<ToolCall>>,
    pub usage: Option<TokenUsage>,
    pub finish_reason: String,
    pub model: String,
}

// ---------------------------------------------------------------------------
// Provider trait
// ---------------------------------------------------------------------------

/// Interface that all LLM provider adapters must implement.
///
/// Each concrete provider (OpenAI, Anthropic, etc.) implements this trait.
#[async_trait]
pub trait Provider: Send + Sync {
    /// Send a chat request and receive a complete (non-streaming) response.
    async fn chat(&self, request: ChatRequest) -> Result<ChatResponse, ProviderError>;

    /// Send a chat request and receive a streaming response as an [`EventStream`].
    async fn chat_stream(&self, request: ChatRequest) -> Result<EventStream, ProviderError>;

    /// Human-readable provider name (e.g. `"openai"`, `"anthropic"`).
    fn name(&self) -> &str;

    /// Whether this provider supports streaming responses.
    fn supports_streaming(&self) -> bool;

    /// Whether this provider supports tool / function calling.
    fn supports_tools(&self) -> bool;
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::ApiProtocol;
    use crate::types::{Message, Role};
    use std::collections::HashMap;

    fn make_resolved(model_id: &str) -> ResolvedModel {
        ResolvedModel {
            canonical_id: model_id.to_string(),
            provider: "mock".to_string(),
            api_key: None,
            base_url: "http://localhost".to_string(),
            api_protocol: ApiProtocol::OpenAiChat,
            api_model_id: model_id.to_string(),
            context_length: 8192,
            provider_specific: HashMap::new(),
        }
    }

    #[test]
    fn test_chat_request_new() {
        let messages = vec![Message {
            role: Role::User,
            content: "hello".to_string(),
            tool_calls: None,
            tool_call_id: None,
            name: None,
        }];
        let tools = vec![];
        let resolved = make_resolved("test-model");
        let req = ChatRequest::new(messages.clone(), tools.clone(), resolved.clone());
        assert_eq!(req.model, "test-model");
        assert_eq!(req.messages, messages);
        assert_eq!(req.resolved.canonical_id, "test-model");
    }
}
