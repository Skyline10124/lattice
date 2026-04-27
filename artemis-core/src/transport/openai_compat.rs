#![allow(deprecated)]
use std::collections::HashMap;

use crate::provider::{ChatRequest, ChatResponse};
use crate::transport::chat_completions::{ChatCompletionsTransport, Transport, TransportError};

pub struct OpenAICompatTransport {
    base_url: String,
    extra_headers: HashMap<String, String>,
    inner: ChatCompletionsTransport,
}

impl OpenAICompatTransport {
    pub fn new(base_url: impl Into<String>, extra_headers: HashMap<String, String>) -> Self {
        Self {
            base_url: base_url.into(),
            extra_headers,
            inner: ChatCompletionsTransport::new(),
        }
    }

    pub fn ollama() -> Self {
        Self::new("http://localhost:11434/v1", HashMap::new())
    }

    pub fn groq() -> Self {
        Self::new("https://api.groq.com/openai/v1", HashMap::new())
    }

    pub fn xai() -> Self {
        Self::new("https://api.x.ai/v1", HashMap::new())
    }

    pub fn deepseek() -> Self {
        Self::new("https://api.deepseek.com/v1", HashMap::new())
    }

    pub fn mistral() -> Self {
        Self::new("https://api.mistral.ai/v1", HashMap::new())
    }

    pub fn openrouter() -> Self {
        Self::new(
            "https://openrouter.ai/api/v1",
            HashMap::from([
                ("HTTP-Referer".to_string(), "https://github.com/astrin/artemis".to_string()),
                ("X-Title".to_string(), "Artemis".to_string()),
            ]),
        )
    }
}

impl Transport for OpenAICompatTransport {
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
        self.inner.normalize_request(request)
    }

    fn denormalize_response(
        &self,
        response: &serde_json::Value,
    ) -> Result<ChatResponse, TransportError> {
        self.inner.denormalize_response(response)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::{ApiProtocol, ResolvedModel};
    use std::collections::HashMap;

    #[test]
    fn test_default_base_url() {
        let transport = OpenAICompatTransport::new("https://api.openai.com/v1", HashMap::new());
        assert_eq!(transport.base_url(), "https://api.openai.com/v1");
    }

    #[test]
    fn test_custom_base_url() {
        let transport = OpenAICompatTransport::new("http://custom:8080/v1", HashMap::new());
        assert_eq!(transport.base_url(), "http://custom:8080/v1");
    }

    #[test]
    fn test_custom_extra_headers() {
        let headers = HashMap::from([
            ("HTTP-Referer".to_string(), "https://example.com".to_string()),
            ("X-Title".to_string(), "MyApp".to_string()),
        ]);
        let transport = OpenAICompatTransport::new("https://openrouter.ai/api/v1", headers.clone());
        assert_eq!(transport.extra_headers(), &headers);
    }

    #[test]
    fn test_ollama_presets() {
        let transport = OpenAICompatTransport::ollama();
        assert_eq!(transport.base_url(), "http://localhost:11434/v1");
        assert!(transport.extra_headers().is_empty());
        assert_eq!(transport.api_mode(), "chat_completions");
    }

    #[test]
    fn test_groq_presets() {
        let transport = OpenAICompatTransport::groq();
        assert_eq!(transport.base_url(), "https://api.groq.com/openai/v1");
        assert!(transport.extra_headers().is_empty());
    }

    #[test]
    fn test_xai_presets() {
        let transport = OpenAICompatTransport::xai();
        assert_eq!(transport.base_url(), "https://api.x.ai/v1");
        assert!(transport.extra_headers().is_empty());
    }

    #[test]
    fn test_deepseek_presets() {
        let transport = OpenAICompatTransport::deepseek();
        assert_eq!(transport.base_url(), "https://api.deepseek.com/v1");
        assert!(transport.extra_headers().is_empty());
    }

    #[test]
    fn test_mistral_presets() {
        let transport = OpenAICompatTransport::mistral();
        assert_eq!(transport.base_url(), "https://api.mistral.ai/v1");
        assert!(transport.extra_headers().is_empty());
    }

    #[test]
    fn test_openrouter_presets() {
        let transport = OpenAICompatTransport::openrouter();
        assert_eq!(transport.base_url(), "https://openrouter.ai/api/v1");
        let headers = transport.extra_headers();
        assert_eq!(headers.get("HTTP-Referer").unwrap(), "https://github.com/astrin/artemis");
        assert_eq!(headers.get("X-Title").unwrap(), "Artemis");
        assert_eq!(headers.len(), 2);
    }

    #[test]
    fn test_delegates_to_inner() {
        let transport = OpenAICompatTransport::ollama();

        let request = ChatRequest {
            messages: vec![crate::types::Message {
                role: crate::types::Role::User,
                content: "Hello".into(),
                tool_calls: None,
                tool_call_id: None,
                name: None,
            }],
            tools: vec![],
            model: "llama3".into(),
            temperature: Some(0.5),
            max_tokens: None,
            stream: false,
            resolved: ResolvedModel {
                canonical_id: "llama3".into(),
                provider: "ollama".into(),
                api_key: None,
                base_url: "http://localhost:11434/v1".into(),
                api_protocol: ApiProtocol::OpenAiChat,
                api_model_id: "llama3".into(),
                context_length: 131072,
                provider_specific: HashMap::new(),
            },
        };

        let body = transport.normalize_request(&request).unwrap();
        assert_eq!(body["model"], "llama3");
        assert_eq!(body["temperature"], 0.5);
        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages[0]["role"], "user");
        assert_eq!(messages[0]["content"], "Hello");

        let response = serde_json::json!({
            "id": "chatcmpl-1",
            "model": "llama3",
            "choices": [{
                "index": 0,
                "message": {"role": "assistant", "content": "Hi there!"},
                "finish_reason": "stop"
            }],
            "usage": {"prompt_tokens": 5, "completion_tokens": 3, "total_tokens": 8}
        });

        let result = transport.denormalize_response(&response).unwrap();
        assert_eq!(result.content.as_deref(), Some("Hi there!"));
        assert_eq!(result.model, "llama3");
        assert_eq!(result.usage.unwrap().total_tokens, 8);
    }

    #[test]
    fn test_delegates_tool_call_roundtrip() {
        let transport = OpenAICompatTransport::groq();

        let request = ChatRequest {
            messages: vec![crate::types::Message {
                role: crate::types::Role::User,
                content: "What's the weather?".into(),
                tool_calls: None,
                tool_call_id: None,
                name: None,
            }],
            tools: vec![crate::types::ToolDefinition {
                name: "get_weather".into(),
                description: "Get weather".into(),
                parameters: serde_json::json!({"type": "object"}),
            }],
            model: "llama-3".into(),
            temperature: None,
            max_tokens: None,
            stream: true,
            resolved: ResolvedModel {
                canonical_id: "llama-3".into(),
                provider: "groq".into(),
                api_key: Some("gsk_test".into()),
                base_url: "https://api.groq.com/openai/v1".into(),
                api_protocol: ApiProtocol::OpenAiChat,
                api_model_id: "llama-3".into(),
                context_length: 131072,
                provider_specific: HashMap::new(),
            },
        };

        let body = transport.normalize_request(&request).unwrap();
        assert_eq!(body["stream"], true);
        let tools = body["tools"].as_array().unwrap();
        assert_eq!(tools[0]["function"]["name"], "get_weather");

        let response = serde_json::json!({
            "id": "chatcmpl-2",
            "model": "llama-3",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "call_1",
                        "type": "function",
                        "function": {"name": "get_weather", "arguments": "{\"city\": \"SF\"}"}
                    }]
                },
                "finish_reason": "tool_calls"
            }]
        });

        let result = transport.denormalize_response(&response).unwrap();
        let tcs = result.tool_calls.unwrap();
        assert_eq!(tcs[0].function.name, "get_weather");
        assert_eq!(result.finish_reason, "tool_calls");
    }
}
