//! OpenAI-compatible provider: request JSON construction, SSE parsing,
//! and auth headers for the Chat Completions API.
//!
//! Supports OpenAI and OpenAI-compatible endpoints (Ollama, vLLM, LiteLLM, etc.).
//! Uses bounded SSE framing from `crate::sse` and never imports `lattice-core`.

use std::collections::HashMap;

use crate::router::ResolvedModel;
use crate::types::{ChatRequest, LlmEvent, Role, TokenUsage};

/// Build the JSON body for an OpenAI Chat Completions request.
///
/// Maps internal `Message` and `ToolDefinition` types to the OpenAI wire format.
/// Always sets `stream: true` for streaming requests.
pub fn build_request_json(request: &ChatRequest, resolved: &ResolvedModel) -> serde_json::Value {
    let mut body = serde_json::json!({
        "model": resolved.provider_model,
        "messages": [],
        "stream": true,
    });

    let messages = body
        .get_mut("messages")
        .and_then(|m| m.as_array_mut())
        .expect("messages must be an array");

    for msg in &request.messages {
        let mut m = serde_json::json!({
            "role": match msg.role {
                Role::System => "system",
                Role::User => "user",
                Role::Assistant => "assistant",
                Role::Tool => "tool",
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

        if let Some(reasoning) = &msg.reasoning_content {
            m["reasoning_content"] = serde_json::Value::String(reasoning.clone());
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

    if let Some(temperature) = request.temperature {
        body["temperature"] = serde_json::json!(temperature);
    }

    if let Some(max_tokens) = request.max_tokens {
        body["max_tokens"] = serde_json::json!(max_tokens);
    }

    body
}

/// Build the `Authorization` header value for an OpenAI request.
pub fn auth_header_value(api_key: &str) -> String {
    format!("Bearer {}", api_key)
}

/// Stateful parser for OpenAI SSE chunks.
///
/// Tracks tool call IDs per index because OpenAI omits the `id`
/// field from subsequent delta chunks after the first one.
#[derive(Default)]
pub struct OpenAiSseParser {
    tool_call_ids: HashMap<u32, String>,
}

impl OpenAiSseParser {
    /// Create a new parser with no tracked state.
    pub fn new() -> Self {
        Self {
            tool_call_ids: HashMap::new(),
        }
    }

    /// Parse a single SSE data chunk into [`LlmEvent`] values.
    ///
    /// The `event_type` parameter is unused by OpenAI SSE (which uses unnamed
    /// `data:` lines), but is accepted for interface compatibility.
    pub fn parse_chunk(&mut self, _event_type: Option<&str>, data: &str) -> Vec<LlmEvent> {
        let trimmed = data.trim();
        if trimmed.is_empty() {
            return vec![];
        }
        if trimmed == "[DONE]" {
            return vec![];
        }

        let root: serde_json::Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(e) => {
                return vec![LlmEvent::Error {
                    message: format!("SSE JSON parse error: {}", e),
                }];
            }
        };

        // Handle API error
        if let Some(error) = root.get("error") {
            let msg = error
                .get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("Unknown API error");
            return vec![LlmEvent::Error {
                message: msg.to_string(),
            }];
        }

        let mut events = Vec::new();

        if let Some(choices) = root.get("choices").and_then(|c| c.as_array()) {
            for choice in choices {
                let delta = &choice["delta"];
                let finish_reason = choice["finish_reason"].as_str();

                // Reasoning content (streams before regular content)
                if let Some(reasoning) = delta.get("reasoning_content").and_then(|r| r.as_str()) {
                    if !reasoning.is_empty() {
                        events.push(LlmEvent::Reasoning {
                            content: reasoning.to_string(),
                        });
                    }
                }

                // Content tokens
                if let Some(content) = delta.get("content").and_then(|c| c.as_str()) {
                    if !content.is_empty() {
                        events.push(LlmEvent::Token {
                            content: content.to_string(),
                        });
                    }
                }

                // Tool calls
                if let Some(tool_calls) = delta.get("tool_calls").and_then(|t| t.as_array()) {
                    for tc in tool_calls {
                        let idx = tc.get("index").and_then(|i| i.as_u64()).unwrap_or(0) as u32;

                        // ToolCallStart: first chunk with id
                        if let Some(id) = tc.get("id").and_then(|i| i.as_str()) {
                            let name = tc
                                .get("function")
                                .and_then(|f| f.get("name"))
                                .and_then(|n| n.as_str())
                                .unwrap_or("");
                            self.tool_call_ids.insert(idx, id.to_string());
                            events.push(LlmEvent::ToolCallStart {
                                id: id.to_string(),
                                name: name.to_string(),
                            });
                        }

                        // ToolCallDelta: subsequent chunks with arguments
                        if let Some(args) = tc
                            .get("function")
                            .and_then(|f| f.get("arguments"))
                            .and_then(|a| a.as_str())
                        {
                            if !args.is_empty() {
                                if let Some(id) = self.tool_call_ids.get(&idx).cloned() {
                                    events.push(LlmEvent::ToolCallDelta {
                                        id,
                                        arguments_delta: args.to_string(),
                                    });
                                }
                            }
                        }
                    }
                }

                // Finish reason — emits ToolCallEnd for all tracked IDs, then Done
                if let Some(reason) = finish_reason {
                    if !reason.is_empty() {
                        // Emit ToolCallEnd for all pending tool calls
                        for id in self.tool_call_ids.drain().map(|(_, id)| id) {
                            events.push(LlmEvent::ToolCallEnd { id });
                        }

                        let usage = root.get("usage").map(|u| TokenUsage {
                            prompt_tokens: u
                                .get("prompt_tokens")
                                .and_then(|v| v.as_u64())
                                .unwrap_or(0) as u32,
                            completion_tokens: u
                                .get("completion_tokens")
                                .and_then(|v| v.as_u64())
                                .unwrap_or(0) as u32,
                            total_tokens: u
                                .get("total_tokens")
                                .and_then(|v| v.as_u64())
                                .unwrap_or(0) as u32,
                        });

                        events.push(LlmEvent::Done {
                            finish_reason: reason.to_string(),
                            usage,
                        });
                    }
                }
            }
        }

        events
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::router::{CredentialStatus, ProviderKind};
    use crate::types::{ChatRequest, Message, ModelTarget, Role, ToolDefinition};

    fn make_resolved() -> ResolvedModel {
        ResolvedModel {
            alias: "gpt-4o".to_string(),
            provider: ProviderKind::OpenAi,
            provider_model: "gpt-4o".to_string(),
            base_url: "https://api.openai.com/v1".to_string(),
            api_key_env: "OPENAI_API_KEY".to_string(),
            credential_status: CredentialStatus::Present,
        }
    }

    // ── Request JSON building ─────────────────────────────────────────────

    #[test]
    fn build_request_includes_model() {
        let req = ChatRequest::new(vec![Message::user("hello")], ModelTarget::new("gpt-4o"));
        let resolved = make_resolved();
        let json = build_request_json(&req, &resolved);
        assert_eq!(json["model"], "gpt-4o");
    }

    #[test]
    fn build_request_includes_messages_with_roles() {
        let req = ChatRequest {
            messages: vec![Message::system("You are helpful."), Message::user("Hello!")],
            tools: vec![],
            target: ModelTarget::new("gpt-4o"),
            temperature: None,
            max_tokens: None,
            stream: true,
        };
        let resolved = make_resolved();
        let json = build_request_json(&req, &resolved);
        let messages = json["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0]["role"], "system");
        assert_eq!(messages[0]["content"], "You are helpful.");
        assert_eq!(messages[1]["role"], "user");
        assert_eq!(messages[1]["content"], "Hello!");
    }

    #[test]
    fn build_request_includes_stream_true() {
        let req = ChatRequest::new(vec![Message::user("hi")], ModelTarget::new("gpt-4o"));
        let resolved = make_resolved();
        let json = build_request_json(&req, &resolved);
        assert_eq!(json["stream"], true);
    }

    #[test]
    fn build_request_includes_tools() {
        let req = ChatRequest {
            messages: vec![Message::user("weather?")],
            tools: vec![ToolDefinition::new(
                "get_weather",
                "Get weather for a city",
                serde_json::json!({"type": "object", "properties": {"city": {"type": "string"}}}),
            )],
            target: ModelTarget::new("gpt-4o"),
            temperature: None,
            max_tokens: None,
            stream: true,
        };
        let resolved = make_resolved();
        let json = build_request_json(&req, &resolved);
        let tools = json["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["type"], "function");
        assert_eq!(tools[0]["function"]["name"], "get_weather");
        assert_eq!(
            tools[0]["function"]["parameters"]["properties"]["city"]["type"],
            "string"
        );
    }

    #[test]
    fn build_request_includes_temperature_and_max_tokens() {
        let req = ChatRequest {
            messages: vec![Message::user("hi")],
            tools: vec![],
            target: ModelTarget::new("gpt-4o"),
            temperature: Some(0.7),
            max_tokens: Some(1024),
            stream: true,
        };
        let resolved = make_resolved();
        let json = build_request_json(&req, &resolved);
        assert_eq!(json["temperature"], 0.7);
        assert_eq!(json["max_tokens"], 1024);
    }

    #[test]
    fn build_request_omits_optional_fields_when_none() {
        let req = ChatRequest::new(vec![Message::user("hi")], ModelTarget::new("gpt-4o"));
        let resolved = make_resolved();
        let json = build_request_json(&req, &resolved);
        // temperature and max_tokens should be absent when not set
        assert!(json.get("temperature").is_none() || json["temperature"].is_null());
        assert!(json.get("max_tokens").is_none() || json["max_tokens"].is_null());
    }

    #[test]
    fn build_request_includes_assistant_tool_calls() {
        let req = ChatRequest {
            messages: vec![Message {
                role: Role::Assistant,
                content: String::new(),
                reasoning_content: None,
                tool_calls: Some(vec![crate::types::ToolCall::new(
                    "call_1",
                    crate::types::FunctionCall::new("search", r#"{"q":"rust"}"#),
                )]),
                tool_call_id: None,
                name: None,
            }],
            tools: vec![],
            target: ModelTarget::new("gpt-4o"),
            temperature: None,
            max_tokens: None,
            stream: true,
        };
        let resolved = make_resolved();
        let json = build_request_json(&req, &resolved);
        let messages = json["messages"].as_array().unwrap();
        let tc_array = messages[0]["tool_calls"].as_array().unwrap();
        assert_eq!(tc_array.len(), 1);
        assert_eq!(tc_array[0]["id"], "call_1");
        assert_eq!(tc_array[0]["type"], "function");
        assert_eq!(tc_array[0]["function"]["name"], "search");
    }

    #[test]
    fn build_request_includes_tool_result() {
        let req = ChatRequest {
            messages: vec![Message {
                role: Role::Tool,
                content: r#"{"result":"found"}"#.to_string(),
                reasoning_content: None,
                tool_calls: None,
                tool_call_id: Some("call_1".to_string()),
                name: Some("search".to_string()),
            }],
            tools: vec![],
            target: ModelTarget::new("gpt-4o"),
            temperature: None,
            max_tokens: None,
            stream: true,
        };
        let resolved = make_resolved();
        let json = build_request_json(&req, &resolved);
        let messages = json["messages"].as_array().unwrap();
        assert_eq!(messages[0]["role"], "tool");
        assert_eq!(messages[0]["tool_call_id"], "call_1");
    }

    // ── Auth header ───────────────────────────────────────────────────────

    #[test]
    fn auth_header_uses_bearer_prefix() {
        let header = auth_header_value("sk-test-key");
        assert!(
            header.starts_with("Bearer "),
            "auth header should start with 'Bearer ', got: {}",
            header
        );
    }

    #[test]
    fn auth_header_contains_key_value() {
        let header = auth_header_value("sk-abc123");
        assert!(
            header.contains("sk-abc123"),
            "auth header should contain the key value"
        );
    }

    // ── SSE parsing ───────────────────────────────────────────────────────

    #[test]
    fn parse_sse_token_chunk() {
        // Reset parser state between tests — use a fresh parser instance
        let mut parser = OpenAiSseParser::new();
        let data = r#"{"id":"chatcmpl-123","object":"chat.completion.chunk","choices":[{"index":0,"delta":{"content":"Hello"},"finish_reason":null}]}"#;
        let events = parser.parse_chunk(None, data);
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], LlmEvent::Token { content } if content == "Hello"));
    }

    #[test]
    fn parse_sse_reasoning_chunk() {
        let mut parser = OpenAiSseParser::new();
        let data = r#"{"id":"chatcmpl-123","object":"chat.completion.chunk","choices":[{"index":0,"delta":{"reasoning_content":"Let me think..."},"finish_reason":null}]}"#;
        let events = parser.parse_chunk(None, data);
        assert_eq!(events.len(), 1);
        assert!(
            matches!(&events[0], LlmEvent::Reasoning { content } if content == "Let me think...")
        );
    }

    #[test]
    fn parse_sse_tool_call_start() {
        let mut parser = OpenAiSseParser::new();
        let data = r#"{"id":"chatcmpl-123","object":"chat.completion.chunk","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_abc","type":"function","function":{"name":"get_weather","arguments":""}}]},"finish_reason":null}]}"#;
        let events = parser.parse_chunk(None, data);
        let has_start = events.iter().any(|e| matches!(e, LlmEvent::ToolCallStart { id, name } if id == "call_abc" && name == "get_weather"));
        assert!(has_start, "should emit ToolCallStart, got: {:?}", events);
    }

    #[test]
    fn parse_sse_tool_call_delta() {
        let mut parser = OpenAiSseParser::new();
        // Register a tool call first
        let start_data = r#"{"id":"chatcmpl-123","object":"chat.completion.chunk","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_abc","type":"function","function":{"name":"get_weather","arguments":""}}]},"finish_reason":null}]}"#;
        parser.parse_chunk(None, start_data);
        // Now a delta chunk
        let data = r#"{"id":"chatcmpl-123","object":"chat.completion.chunk","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"city\""}}]},"finish_reason":null}]}"#;
        let events = parser.parse_chunk(None, data);
        let has_delta = events.iter().any(|e| matches!(e, LlmEvent::ToolCallDelta { id, arguments_delta } if id == "call_abc" && arguments_delta == r#"{"city""#));
        assert!(has_delta, "should emit ToolCallDelta, got: {:?}", events);
    }

    #[test]
    fn parse_sse_done_with_finish_reason_and_usage() {
        let mut parser = OpenAiSseParser::new();
        let data = r#"{"id":"chatcmpl-123","object":"chat.completion.chunk","choices":[{"index":0,"delta":{},"finish_reason":"stop"}],"usage":{"prompt_tokens":10,"completion_tokens":20,"total_tokens":30}}"#;
        let events = parser.parse_chunk(None, data);
        let has_done = events.iter().any(|e| matches!(e, LlmEvent::Done { finish_reason, usage } if finish_reason == "stop" && usage.as_ref().unwrap().total_tokens == 30));
        assert!(has_done, "should emit Done with usage, got: {:?}", events);
    }

    #[test]
    fn parse_sse_done_sentinel_ignored() {
        let mut parser = OpenAiSseParser::new();
        let events = parser.parse_chunk(None, "[DONE]");
        assert!(events.is_empty(), "[DONE] should produce no events");
    }

    #[test]
    fn parse_sse_error_chunk() {
        let mut parser = OpenAiSseParser::new();
        let data = r#"{"error":{"message":"Rate limit exceeded","type":"rate_limit_error"}}"#;
        let events = parser.parse_chunk(None, data);
        assert_eq!(events.len(), 1);
        assert!(
            matches!(&events[0], LlmEvent::Error { message } if message.contains("Rate limit"))
        );
    }

    #[test]
    fn parse_sse_tool_call_end_on_finish() {
        let mut parser = OpenAiSseParser::new();
        // Register a tool call first
        let start_data = r#"{"id":"chatcmpl-123","object":"chat.completion.chunk","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_xyz","type":"function","function":{"name":"search","arguments":"{}"}}]},"finish_reason":null}]}"#;
        parser.parse_chunk(None, start_data);
        // Now emit finish
        let done_data = r#"{"id":"chatcmpl-123","object":"chat.completion.chunk","choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}"#;
        let events = parser.parse_chunk(None, done_data);
        let has_end = events
            .iter()
            .any(|e| matches!(e, LlmEvent::ToolCallEnd { id } if id == "call_xyz"));
        assert!(
            has_end,
            "finish with tool_calls should emit ToolCallEnd, got: {:?}",
            events
        );
    }

    #[test]
    fn parse_sse_empty_data_returns_empty() {
        let mut parser = OpenAiSseParser::new();
        let events = parser.parse_chunk(None, "");
        assert!(events.is_empty());
    }

    // ── Boundary: no lattice_core imports ──────────────────────────────────

    #[test]
    fn openai_provider_has_no_lattice_core_imports() {
        // This test compiles iff openai.rs does not import lattice_core.
        // Presence of this test file in lattice-llm proves the boundary.
        let json = build_request_json(
            &ChatRequest::new(vec![Message::user("test")], ModelTarget::new("gpt-4o")),
            &make_resolved(),
        );
        assert_eq!(json["model"], "gpt-4o");
    }
}
