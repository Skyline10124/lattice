#![allow(deprecated)]
//! OpenAI Chat Completions transport — format conversion for the OpenAI API.
//!
//! This module defines the [`Transport`] trait and provides [`ChatCompletionsTransport`],
//! which handles normalization/denormalization for the OpenAI Chat Completions API format.
//!
//! Other OpenAI-compatible providers (Ollama, Groq, xAI, DeepSeek, Mistral, OpenRouter)
//! wrap this transport via [`crate::transport::openai_compat::OpenAICompatTransport`],
//! delegating all format logic here and only overriding the base URL and extra headers.

use std::collections::HashMap;

use crate::provider::{ChatRequest, ChatResponse};

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// Errors that can occur during transport-level normalization/denormalization.
#[derive(Debug, thiserror::Error)]
pub enum TransportError {
    /// A serialization error when converting to API format.
    #[error("Serialization error: {0}")]
    Serialization(String),

    /// A deserialization error when converting from API format.
    #[error("Deserialization error: {0}")]
    Deserialization(String),

    /// The API returned an unexpected format.
    #[error("Unexpected format: {0}")]
    UnexpectedFormat(String),
}

// ---------------------------------------------------------------------------
// Transport trait
// ---------------------------------------------------------------------------

/// Trait for converting between internal types and a specific API format.
///
/// Each transport implementation handles one API format (e.g. OpenAI Chat Completions,
/// Anthropic Messages). The transport is responsible for:
///
/// - Converting internal [`ChatRequest`]s into API-specific JSON bodies
/// - Converting API-specific JSON responses into internal [`ChatResponse`]s
/// - Providing the base URL and any extra headers needed for the HTTP client
/// - Identifying the API mode (for logging / routing)
pub trait Transport: Send + Sync {
    /// The API base URL for constructing the HTTP client.
    fn base_url(&self) -> &str;

    /// Extra HTTP headers to include in requests (e.g. OpenRouter's `HTTP-Referer`).
    fn extra_headers(&self) -> &HashMap<String, String>;

    /// A string identifying the API mode (e.g. `"chat_completions"`, `"anthropic"`).
    fn api_mode(&self) -> &str;

    /// Convert an internal [`ChatRequest`] into an API-specific JSON body.
    fn normalize_request(&self, request: &ChatRequest) -> Result<serde_json::Value, TransportError>;

    /// Convert an API-specific JSON response into an internal [`ChatResponse`].
    fn denormalize_response(
        &self,
        response: &serde_json::Value,
    ) -> Result<ChatResponse, TransportError>;
}

// ---------------------------------------------------------------------------
// ChatCompletionsTransport
// ---------------------------------------------------------------------------

/// Transport for the OpenAI Chat Completions API format.
///
/// This is the canonical implementation used by OpenAI and all OpenAI-compatible
/// providers. It handles converting between internal types and the
/// `chat.completions` / `chat.completions.chunk` JSON format.
///
/// OpenAI-compatible providers (Ollama, Groq, etc.) wrap this transport via
/// [`crate::transport::openai_compat::OpenAICompatTransport`] to override
/// the base URL and extra headers while reusing the same format logic.
pub struct ChatCompletionsTransport {
    base_url: String,
    extra_headers: HashMap<String, String>,
}

impl ChatCompletionsTransport {
    /// Create a new ChatCompletionsTransport with the default OpenAI base URL.
    pub fn new() -> Self {
        Self {
            base_url: "https://api.openai.com/v1".to_string(),
            extra_headers: HashMap::new(),
        }
    }

    /// Create a ChatCompletionsTransport with a custom base URL.
    pub fn with_base_url(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
            extra_headers: HashMap::new(),
        }
    }
}

impl Default for ChatCompletionsTransport {
    fn default() -> Self {
        Self::new()
    }
}

impl Transport for ChatCompletionsTransport {
    fn base_url(&self) -> &str {
        &self.base_url
    }

    fn extra_headers(&self) -> &HashMap<String, String> {
        &self.extra_headers
    }

    fn api_mode(&self) -> &str {
        "chat_completions"
    }

    fn normalize_request(&self, request: &ChatRequest) -> Result<serde_json::Value, TransportError> {
        let mut body = serde_json::json!({
            "model": request.model,
            "messages": [],
        });

        let messages = body
            .get_mut("messages")
            .and_then(|m| m.as_array_mut())
            .ok_or_else(|| TransportError::Serialization("failed to build messages array".into()))?;

        for msg in &request.messages {
            let mut m = serde_json::json!({
                "role": match msg.role {
                    crate::types::Role::System => "system",
                    crate::types::Role::User => "user",
                    crate::types::Role::Assistant => "assistant",
                    crate::types::Role::Tool => "tool",
                },
                "content": msg.content,
            });

            if let Some(tool_calls) = &msg.tool_calls {
                let tc_array: Vec<serde_json::Value> = tool_calls
                    .iter()
                    .map(|tc| {
                        serde_json::json!({
                            "id": tc.id,
                            "type": "function",
                            "function": {
                                "name": tc.function.name,
                                "arguments": tc.function.arguments,
                            }
                        })
                    })
                    .collect();
                m["tool_calls"] = serde_json::Value::Array(tc_array);
            }

            if let Some(tool_call_id) = &msg.tool_call_id {
                m["tool_call_id"] = serde_json::Value::String(tool_call_id.clone());
            }

            if let Some(name) = &msg.name {
                m["name"] = serde_json::Value::String(name.clone());
            }

            messages.push(m);
        }

        if !request.tools.is_empty() {
            let tools_array: Vec<serde_json::Value> = request
                .tools
                .iter()
                .map(|tool| {
                    serde_json::json!({
                        "type": "function",
                        "function": {
                            "name": tool.name,
                            "description": tool.description,
                            "parameters": tool.parameters,
                        }
                    })
                })
                .collect();
            body["tools"] = serde_json::Value::Array(tools_array);
        }

        if let Some(temp) = request.temperature {
            body["temperature"] = serde_json::Value::Number(
                serde_json::Number::from_f64(temp)
                    .unwrap_or_else(|| serde_json::Number::from(0)),
            );
        }

        if let Some(max_tokens) = request.max_tokens {
            body["max_tokens"] = serde_json::Value::Number(serde_json::Number::from(max_tokens));
        }

        if request.stream {
            body["stream"] = serde_json::Value::Bool(true);
        }

        Ok(body)
    }

    fn denormalize_response(
        &self,
        response: &serde_json::Value,
    ) -> Result<ChatResponse, TransportError> {
        let content = response["choices"]
            .as_array()
            .and_then(|choices| choices.first())
            .and_then(|choice| choice["message"]["content"].as_str())
            .map(|s| s.to_string());

        let tool_calls = response["choices"]
            .as_array()
            .and_then(|choices| choices.first())
            .and_then(|choice| choice["message"]["tool_calls"].as_array())
            .map(|tcs| {
                tcs.iter()
                    .filter_map(|tc| {
                        let id = tc["id"].as_str()?.to_string();
                        let name = tc["function"]["name"].as_str()?.to_string();
                        let arguments = tc["function"]["arguments"]
                            .as_str()
                            .unwrap_or("{}")
                            .to_string();
                        Some(crate::types::ToolCall {
                            id,
                            function: crate::types::FunctionCall {
                                name,
                                arguments,
                            },
                        })
                    })
                    .collect::<Vec<_>>()
            })
            .filter(|v| !v.is_empty());

        let finish_reason = response["choices"]
            .as_array()
            .and_then(|choices| choices.first())
            .and_then(|choice| choice["finish_reason"].as_str())
            .unwrap_or("stop")
            .to_string();

        let model = response["model"]
            .as_str()
            .unwrap_or("unknown")
            .to_string();

        let usage = response["usage"].as_object().map(|u| {
            crate::streaming::TokenUsage {
                prompt_tokens: u["prompt_tokens"].as_u64().unwrap_or(0) as u32,
                completion_tokens: u["completion_tokens"].as_u64().unwrap_or(0) as u32,
                total_tokens: u["total_tokens"].as_u64().unwrap_or(0) as u32,
            }
        });

        Ok(ChatResponse {
            content,
            tool_calls,
            usage,
            finish_reason,
            model,
        })
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::ApiProtocol;
    use crate::types::{Message, Role, ToolDefinition};
    use std::collections::HashMap;

    #[test]
    fn test_default_base_url() {
        let transport = ChatCompletionsTransport::new();
        assert_eq!(transport.base_url(), "https://api.openai.com/v1");
    }

    #[test]
    fn test_custom_base_url() {
        let transport = ChatCompletionsTransport::with_base_url("http://localhost:11434/v1");
        assert_eq!(transport.base_url(), "http://localhost:11434/v1");
    }

    #[test]
    fn test_api_mode() {
        let transport = ChatCompletionsTransport::new();
        assert_eq!(transport.api_mode(), "chat_completions");
    }

    #[test]
    fn test_extra_headers_default_empty() {
        let transport = ChatCompletionsTransport::new();
        assert!(transport.extra_headers().is_empty());
    }

    #[test]
    fn test_normalize_simple_request() {
        let transport = ChatCompletionsTransport::new();
        let request = ChatRequest {
            messages: vec![
                Message {
                    role: Role::System,
                    content: "You are helpful.".into(),
                    tool_calls: None,
                    tool_call_id: None,
                    name: None,
                },
                Message {
                    role: Role::User,
                    content: "Hello!".into(),
                    tool_calls: None,
                    tool_call_id: None,
                    name: None,
                },
            ],
            tools: vec![],
            model: "gpt-4o".into(),
            temperature: Some(0.7),
            max_tokens: Some(100),
            stream: false,
            resolved: crate::catalog::ResolvedModel {
                canonical_id: "gpt-4o".into(),
                provider: "openai".into(),
                api_key: Some("sk-test".into()),
                base_url: "https://api.openai.com/v1".into(),
                api_protocol: ApiProtocol::OpenAiChat,
                api_model_id: "gpt-4o".into(),
                context_length: 128000,
                provider_specific: HashMap::new(),
            },
        };

        let body = transport.normalize_request(&request).unwrap();
        assert_eq!(body["model"], "gpt-4o");
        assert_eq!(body["temperature"], 0.7);
        assert_eq!(body["max_tokens"], 100);
        assert!(body["stream"].is_null() || body["stream"] == false);

        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0]["role"], "system");
        assert_eq!(messages[0]["content"], "You are helpful.");
        assert_eq!(messages[1]["role"], "user");
        assert_eq!(messages[1]["content"], "Hello!");
    }

    #[test]
    fn test_normalize_request_with_tools() {
        let transport = ChatCompletionsTransport::new();
        let request = ChatRequest {
            messages: vec![Message {
                role: Role::User,
                content: "What's the weather?".into(),
                tool_calls: None,
                tool_call_id: None,
                name: None,
            }],
            tools: vec![ToolDefinition {
                name: "get_weather".into(),
                description: "Get weather".into(),
                parameters: serde_json::json!({"type": "object", "properties": {"city": {"type": "string"}}}),
            }],
            model: "gpt-4o".into(),
            temperature: None,
            max_tokens: None,
            stream: false,
            resolved: crate::catalog::ResolvedModel {
                canonical_id: "gpt-4o".into(),
                provider: "openai".into(),
                api_key: None,
                base_url: "https://api.openai.com/v1".into(),
                api_protocol: ApiProtocol::OpenAiChat,
                api_model_id: "gpt-4o".into(),
                context_length: 128000,
                provider_specific: HashMap::new(),
            },
        };

        let body = transport.normalize_request(&request).unwrap();
        let tools = body["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["type"], "function");
        assert_eq!(tools[0]["function"]["name"], "get_weather");
    }

    #[test]
    fn test_normalize_request_with_streaming() {
        let transport = ChatCompletionsTransport::new();
        let request = ChatRequest {
            messages: vec![Message {
                role: Role::User,
                content: "Hello".into(),
                tool_calls: None,
                tool_call_id: None,
                name: None,
            }],
            tools: vec![],
            model: "gpt-4o".into(),
            temperature: None,
            max_tokens: None,
            stream: true,
            resolved: crate::catalog::ResolvedModel {
                canonical_id: "gpt-4o".into(),
                provider: "openai".into(),
                api_key: None,
                base_url: "https://api.openai.com/v1".into(),
                api_protocol: ApiProtocol::OpenAiChat,
                api_model_id: "gpt-4o".into(),
                context_length: 128000,
                provider_specific: HashMap::new(),
            },
        };

        let body = transport.normalize_request(&request).unwrap();
        assert_eq!(body["stream"], true);
    }

    #[test]
    fn test_denormalize_simple_response() {
        let transport = ChatCompletionsTransport::new();
        let response = serde_json::json!({
            "id": "chatcmpl-123",
            "object": "chat.completion",
            "model": "gpt-4o",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": "Hello! How can I help you?"
                },
                "finish_reason": "stop"
            }],
            "usage": {
                "prompt_tokens": 10,
                "completion_tokens": 8,
                "total_tokens": 18
            }
        });

        let result = transport.denormalize_response(&response).unwrap();
        assert_eq!(result.content.as_deref(), Some("Hello! How can I help you?"));
        assert!(result.tool_calls.is_none());
        assert_eq!(result.finish_reason, "stop");
        assert_eq!(result.model, "gpt-4o");

        let usage = result.usage.unwrap();
        assert_eq!(usage.prompt_tokens, 10);
        assert_eq!(usage.completion_tokens, 8);
        assert_eq!(usage.total_tokens, 18);
    }

    #[test]
    fn test_denormalize_tool_call_response() {
        let transport = ChatCompletionsTransport::new();
        let response = serde_json::json!({
            "id": "chatcmpl-456",
            "object": "chat.completion",
            "model": "gpt-4o",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "call_abc",
                        "type": "function",
                        "function": {
                            "name": "get_weather",
                            "arguments": "{\"city\": \"Tokyo\"}"
                        }
                    }]
                },
                "finish_reason": "tool_calls"
            }]
        });

        let result = transport.denormalize_response(&response).unwrap();
        assert!(result.content.is_none());
        let tcs = result.tool_calls.unwrap();
        assert_eq!(tcs.len(), 1);
        assert_eq!(tcs[0].id, "call_abc");
        assert_eq!(tcs[0].function.name, "get_weather");
        assert_eq!(tcs[0].function.arguments, r#"{"city": "Tokyo"}"#);
        assert_eq!(result.finish_reason, "tool_calls");
    }

    #[test]
    fn test_roundtrip_simple_message() {
        let transport = ChatCompletionsTransport::new();

        let request = ChatRequest {
            messages: vec![Message {
                role: Role::User,
                content: "Hello!".into(),
                tool_calls: None,
                tool_call_id: None,
                name: None,
            }],
            tools: vec![],
            model: "gpt-4o".into(),
            temperature: None,
            max_tokens: None,
            stream: false,
            resolved: crate::catalog::ResolvedModel {
                canonical_id: "gpt-4o".into(),
                provider: "openai".into(),
                api_key: None,
                base_url: "https://api.openai.com/v1".into(),
                api_protocol: ApiProtocol::OpenAiChat,
                api_model_id: "gpt-4o".into(),
                context_length: 128000,
                provider_specific: HashMap::new(),
            },
        };

        let body = transport.normalize_request(&request).unwrap();
        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages[0]["role"], "user");
        assert_eq!(messages[0]["content"], "Hello!");
    }
}
