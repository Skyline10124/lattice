use std::sync::LazyLock;
use std::time::Duration;

use crate::catalog::ResolvedModel;
use crate::streaming::TokenUsage;
use crate::types::{Message, ToolCall, ToolDefinition};

static SHARED_HTTP_CLIENT: LazyLock<reqwest::Client> = LazyLock::new(|| {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .read_timeout(Duration::from_secs(120))
        .build()
        .expect("Failed to build shared reqwest::Client")
});

pub fn shared_http_client() -> &'static reqwest::Client {
    &SHARED_HTTP_CLIENT
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
    /// DeepSeek thinking mode: `{"type": "enabled"}` or `{"type": "disabled"}`.
    pub thinking: Option<serde_json::Value>,
    /// DeepSeek reasoning effort: `"high"` or `"max"`.
    pub reasoning_effort: Option<String>,
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
            thinking: None,
            reasoning_effort: None,
        }
    }
}

/// A response received from an LLM provider.
#[derive(Debug, Clone)]
pub struct ChatResponse {
    pub content: Option<String>,
    /// Reasoning/thinking content (e.g., DeepSeek R1/V4 thinking chain).
    pub reasoning_content: Option<String>,
    pub tool_calls: Option<Vec<ToolCall>>,
    pub usage: Option<TokenUsage>,
    pub finish_reason: String,
    pub model: String,
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
            credential_status: crate::catalog::CredentialStatus::Missing,
        }
    }

    #[test]
    fn test_chat_request_new() {
        let messages = vec![Message {
            role: Role::User,
            content: "hello".to_string(),
            reasoning_content: None,
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
