// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Anthropic provider: request JSON construction, SSE parsing,
//! and auth headers for the Messages API.
//!
//! Uses Anthropic's native message format with `x-api-key` auth,
//! `anthropic-version: 2023-06-01` header, system message extraction,
//! and content-block-based SSE parsing.

use std::collections::HashMap;

use crate::router::ResolvedModel;
use crate::types::{ChatRequest, LlmEvent, Role, TokenUsage};

/// Maps Anthropic stop reasons to OpenAI-style finish reasons.
const STOP_REASON_MAP: &[(&str, &str)] = &[
    ("end_turn", "stop"),
    ("tool_use", "tool_calls"),
    ("max_tokens", "length"),
    ("stop_sequence", "stop"),
    ("error", "error"),
];

fn map_stop_reason(reason: &str) -> String {
    STOP_REASON_MAP
        .iter()
        .find(|(k, _)| *k == reason)
        .map(|(_, v)| v.to_string())
        .unwrap_or_else(|| reason.to_string())
}

/// Build the JSON body for an Anthropic Messages API request.
pub fn build_request_json(request: &ChatRequest, resolved: &ResolvedModel) -> serde_json::Value {
    let mut system: Option<String> = None;
    let mut messages: Vec<serde_json::Value> = Vec::new();

    for msg in &request.messages {
        match msg.role {
            Role::System => super::push_system_text(&mut system, &msg.content),
            Role::User => {
                let content = if msg.content.is_empty() {
                    serde_json::json!([{"type": "text", "text": "(empty message)"}])
                } else {
                    serde_json::json!([{"type": "text", "text": msg.content}])
                };
                messages.push(serde_json::json!({"role": "user", "content": content}));
            }
            Role::Assistant => {
                let mut blocks: Vec<serde_json::Value> = Vec::new();
                if !msg.content.is_empty() {
                    blocks.push(serde_json::json!({"type": "text", "text": msg.content}));
                }
                if let Some(tool_calls) = &msg.tool_calls {
                    for tc in tool_calls {
                        let input: serde_json::Value = serde_json::from_str(&tc.function.arguments)
                            .unwrap_or(serde_json::json!({}));
                        blocks.push(serde_json::json!({
                            "type": "tool_use",
                            "id": tc.id,
                            "name": tc.function.name,
                            "input": input,
                        }));
                    }
                }
                if blocks.is_empty() {
                    blocks.push(serde_json::json!({"type": "text", "text": "(empty)"}));
                }
                messages.push(serde_json::json!({"role": "assistant", "content": blocks}));
            }
            Role::Tool => {
                let tool_use_id = msg.tool_call_id.clone().unwrap_or_default();
                let content_val = if msg.content.is_empty() {
                    "(no output)".to_string()
                } else {
                    msg.content.clone()
                };
                let tool_result = serde_json::json!({
                    "type": "tool_result",
                    "tool_use_id": tool_use_id,
                    "content": content_val,
                });
                // Merge consecutive tool results into one user message
                if let Some(last) = messages.last_mut() {
                    if last["role"] == "user" {
                        if let Some(content_arr) =
                            last.get_mut("content").and_then(|c| c.as_array_mut())
                        {
                            if content_arr
                                .first()
                                .and_then(|b| b.get("type"))
                                .and_then(|t| t.as_str())
                                == Some("tool_result")
                            {
                                content_arr.push(tool_result);
                                continue;
                            }
                        }
                    }
                }
                messages.push(serde_json::json!({"role": "user", "content": [tool_result]}));
            }
        }
    }

    let max_tokens = request.max_tokens.unwrap_or(4096);

    let mut body = serde_json::json!({
        "model": resolved.provider_model,
        "messages": messages,
        "max_tokens": max_tokens,
        "stream": true,
    });

    if let Some(sys) = system {
        body["system"] = serde_json::Value::String(sys);
    }

    if !request.tools.is_empty() {
        let tools: Vec<serde_json::Value> = request
            .tools
            .iter()
            .map(|t| {
                serde_json::json!({
                    "name": t.name,
                    "description": t.description,
                    "input_schema": t.parameters,
                })
            })
            .collect();
        body["tools"] = serde_json::Value::Array(tools);
    }

    if let Some(temperature) = request.temperature {
        body["temperature"] = serde_json::json!(temperature);
    }

    body
}

/// Build the Anthropic auth header value (just the API key itself).
pub fn auth_header_value(api_key: &str) -> String {
    api_key.to_string()
}

pub const ANTHROPIC_VERSION: &str = "2023-06-01";

/// Stateful parser for Anthropic message-streaming SSE.
#[derive(Default)]
pub struct AnthropicSseParser {
    tool_call_ids: HashMap<u32, String>,
    input_tokens: u32,
}

impl AnthropicSseParser {
    pub fn new() -> Self {
        Self {
            tool_call_ids: HashMap::new(),
            input_tokens: 0,
        }
    }

    pub fn parse_chunk(&mut self, event_type: &str, data: &str) -> Vec<LlmEvent> {
        let root = match super::parse_json_data("Anthropic", data) {
            Ok(Some(v)) => v,
            Ok(None) => return vec![],
            Err(e) => return vec![e],
        };

        match event_type {
            "message_start" => {
                if let Some(msg) = root.get("message") {
                    self.input_tokens = msg["usage"]["input_tokens"].as_u64().unwrap_or(0) as u32;
                }
                vec![]
            }
            "content_block_start" => {
                let idx = root["index"].as_u64().unwrap_or(0) as u32;
                let block = &root["content_block"];
                match block["type"].as_str() {
                    Some("tool_use") => {
                        let id = block["id"].as_str().unwrap_or("").to_string();
                        let name = block["name"].as_str().unwrap_or("").to_string();
                        self.tool_call_ids.insert(idx, id.clone());
                        vec![LlmEvent::ToolCallStart { id, name }]
                    }
                    _ => vec![],
                }
            }
            "content_block_delta" => {
                let idx = root["index"].as_u64().unwrap_or(0) as u32;
                let delta = &root["delta"];
                match delta["type"].as_str() {
                    Some("text_delta") => {
                        let text = delta["text"].as_str().unwrap_or("");
                        if text.is_empty() {
                            vec![]
                        } else {
                            vec![LlmEvent::Token {
                                content: text.to_string(),
                            }]
                        }
                    }
                    Some("thinking_delta") => {
                        let thinking = delta["thinking"].as_str().unwrap_or("");
                        if thinking.is_empty() {
                            vec![]
                        } else {
                            vec![LlmEvent::Reasoning {
                                content: thinking.to_string(),
                            }]
                        }
                    }
                    Some("input_json_delta") => {
                        let partial = delta["partial_json"].as_str().unwrap_or("");
                        if partial.is_empty() {
                            vec![]
                        } else if let Some(id) = self.tool_call_ids.get(&idx).cloned() {
                            vec![LlmEvent::ToolCallDelta {
                                id,
                                arguments_delta: partial.to_string(),
                            }]
                        } else {
                            vec![]
                        }
                    }
                    Some("citation_delta") => {
                        let cite_text = delta["cite_text"].as_str().unwrap_or("");
                        if cite_text.is_empty() {
                            vec![]
                        } else {
                            vec![LlmEvent::Citation {
                                index: idx as usize,
                                cite_text: cite_text.to_string(),
                            }]
                        }
                    }
                    Some("signature_delta") => {
                        let signature = delta["signature"].as_str().unwrap_or("");
                        if signature.is_empty() {
                            vec![]
                        } else {
                            vec![LlmEvent::Signature {
                                signature: signature.to_string(),
                            }]
                        }
                    }
                    _ => vec![],
                }
            }
            "content_block_stop" => {
                let idx = root["index"].as_u64().unwrap_or(0) as u32;
                if let Some(id) = self.tool_call_ids.remove(&idx) {
                    vec![LlmEvent::ToolCallEnd { id }]
                } else {
                    vec![]
                }
            }
            "message_delta" => {
                let stop_reason = root["delta"]["stop_reason"].as_str().unwrap_or("end_turn");
                let finish_reason = map_stop_reason(stop_reason);
                let output_tokens = root["usage"]["output_tokens"].as_u64().unwrap_or(0) as u32;
                let total_tokens = self.input_tokens + output_tokens;
                let usage = root["usage"].as_object().map(|_| TokenUsage {
                    prompt_tokens: self.input_tokens,
                    completion_tokens: output_tokens,
                    total_tokens,
                });
                vec![LlmEvent::Done {
                    finish_reason,
                    usage,
                }]
            }
            "message_stop" | "ping" => vec![],
            "error" => {
                let msg = root["error"]["message"]
                    .as_str()
                    .or_else(|| root["error"]["type"].as_str())
                    .unwrap_or("Unknown Anthropic streaming error")
                    .to_string();
                vec![LlmEvent::Error { message: msg }]
            }
            _ => vec![],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::endpoints::ANTHROPIC_BASE_URL;
    use crate::router::{CredentialStatus, ProviderKind};
    use crate::types::{ChatRequest, Message, ModelTarget, Role, ToolDefinition};

    fn make_resolved() -> ResolvedModel {
        ResolvedModel {
            alias: "sonnet".to_string(),
            provider: ProviderKind::Anthropic,
            provider_model: "claude-sonnet-4-6".to_string(),
            base_url: ANTHROPIC_BASE_URL.to_string(),
            api_key_env: "ANTHROPIC_API_KEY".to_string(),
            credential_status: CredentialStatus::Present,
        }
    }

    #[test]
    fn build_request_includes_model_and_max_tokens() {
        let req = ChatRequest::new(vec![Message::user("hello")], ModelTarget::new("sonnet"));
        let resolved = make_resolved();
        let json = build_request_json(&req, &resolved);
        assert_eq!(json["model"], "claude-sonnet-4-6");
        assert_eq!(json["max_tokens"], 4096);
    }

    #[test]
    fn build_request_extracts_system_message() {
        let req = ChatRequest {
            messages: vec![Message::system("You are helpful."), Message::user("Hello!")],
            tools: vec![],
            target: ModelTarget::new("sonnet"),
            temperature: None,
            max_tokens: None,
            max_completion_tokens: None,
        };
        let resolved = make_resolved();
        let json = build_request_json(&req, &resolved);
        assert_eq!(json["system"], "You are helpful.");
        let messages = json["messages"].as_array().unwrap();
        assert!(!messages.iter().any(|m| m["role"] == "system"));
    }

    #[test]
    fn build_request_merges_multiple_system_messages() {
        let req = ChatRequest {
            messages: vec![
                Message::system("First prompt."),
                Message::system("Second prompt."),
                Message::user("Hello"),
            ],
            tools: vec![],
            target: ModelTarget::new("sonnet"),
            temperature: None,
            max_tokens: None,
            max_completion_tokens: None,
        };
        let resolved = make_resolved();
        let json = build_request_json(&req, &resolved);
        assert_eq!(json["system"], "First prompt.\n\nSecond prompt.");
    }

    #[test]
    fn build_request_user_message_content_blocks() {
        let req = ChatRequest {
            messages: vec![Message::user("Hello, world!")],
            tools: vec![],
            target: ModelTarget::new("sonnet"),
            temperature: None,
            max_tokens: None,
            max_completion_tokens: None,
        };
        let resolved = make_resolved();
        let json = build_request_json(&req, &resolved);
        let messages = json["messages"].as_array().unwrap();
        let msg = &messages[0];
        assert_eq!(msg["role"], "user");
        let content = msg["content"].as_array().unwrap();
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[0]["text"], "Hello, world!");
    }

    #[test]
    fn build_request_assistant_with_tool_use() {
        let req = ChatRequest {
            messages: vec![Message {
                role: Role::Assistant,
                content: "Let me check.".to_string(),
                reasoning_content: None,
                tool_calls: Some(vec![crate::types::ToolCall::new(
                    "toolu_123",
                    crate::types::FunctionCall::new("get_weather", r#"{"city":"Paris"}"#),
                )]),
                tool_call_id: None,
                name: None,
            }],
            tools: vec![],
            target: ModelTarget::new("sonnet"),
            temperature: None,
            max_tokens: None,
            max_completion_tokens: None,
        };
        let resolved = make_resolved();
        let json = build_request_json(&req, &resolved);
        let messages = json["messages"].as_array().unwrap();
        let msg = &messages[0];
        assert_eq!(msg["role"], "assistant");
        let content = msg["content"].as_array().unwrap();
        assert_eq!(content.len(), 2);
        assert_eq!(content[1]["type"], "tool_use");
        assert_eq!(content[1]["id"], "toolu_123");
        assert_eq!(content[1]["name"], "get_weather");
    }

    #[test]
    fn build_request_tool_result() {
        let req = ChatRequest {
            messages: vec![Message {
                role: Role::Tool,
                content: r#"{"temp": 22}"#.to_string(),
                reasoning_content: None,
                tool_calls: None,
                tool_call_id: Some("toolu_123".to_string()),
                name: Some("get_weather".to_string()),
            }],
            tools: vec![],
            target: ModelTarget::new("sonnet"),
            temperature: None,
            max_tokens: None,
            max_completion_tokens: None,
        };
        let resolved = make_resolved();
        let json = build_request_json(&req, &resolved);
        let messages = json["messages"].as_array().unwrap();
        let msg = &messages[0];
        assert_eq!(msg["role"], "user");
        let content = msg["content"].as_array().unwrap();
        assert_eq!(content[0]["type"], "tool_result");
        assert_eq!(content[0]["tool_use_id"], "toolu_123");
    }

    #[test]
    fn build_request_tools_use_input_schema() {
        let req = ChatRequest {
            messages: vec![Message::user("weather?")],
            tools: vec![ToolDefinition::new(
                "get_weather",
                "Get weather",
                serde_json::json!({"type": "object", "properties": {"city": {"type": "string"}}}),
            )],
            target: ModelTarget::new("sonnet"),
            temperature: None,
            max_tokens: None,
            max_completion_tokens: None,
        };
        let resolved = make_resolved();
        let json = build_request_json(&req, &resolved);
        let tools = json["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["name"], "get_weather");
        assert!(tools[0].get("input_schema").is_some());
        assert!(tools[0].get("parameters").is_none());
    }

    #[test]
    fn build_request_always_streams() {
        let req = ChatRequest {
            messages: vec![Message::user("hi")],
            tools: vec![],
            target: ModelTarget::new("sonnet"),
            temperature: None,
            max_tokens: None,
            max_completion_tokens: None,
        };
        let resolved = make_resolved();
        let json = build_request_json(&req, &resolved);
        assert_eq!(
            json["stream"], true,
            "Anthropic request body must always set stream=true"
        );
    }

    #[test]
    fn build_request_uses_explicit_max_tokens() {
        let req = ChatRequest {
            messages: vec![Message::user("hi")],
            tools: vec![],
            target: ModelTarget::new("sonnet"),
            temperature: None,
            max_tokens: Some(2048),
            max_completion_tokens: None,
        };
        let resolved = make_resolved();
        let json = build_request_json(&req, &resolved);
        assert_eq!(json["max_tokens"], 2048);
    }

    #[test]
    fn auth_header_is_api_key_value() {
        let header = auth_header_value("sk-ant-test");
        assert_eq!(header, "sk-ant-test");
    }

    #[test]
    fn anthropic_version_constant() {
        assert_eq!(ANTHROPIC_VERSION, "2023-06-01");
    }

    #[test]
    fn parse_sse_content_block_delta_text() {
        let mut parser = AnthropicSseParser::new();
        let data = r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hello"}}"#;
        let events = parser.parse_chunk("content_block_delta", data);
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], LlmEvent::Token { content } if content == "Hello"));
    }

    #[test]
    fn parse_sse_content_block_start_tool_use() {
        let mut parser = AnthropicSseParser::new();
        let data = r#"{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"toolu_001","name":"get_weather"}}"#;
        let events = parser.parse_chunk("content_block_start", data);
        let has_start = events.iter().any(|e| matches!(e, LlmEvent::ToolCallStart { id, name } if id == "toolu_001" && name == "get_weather"));
        assert!(has_start, "should emit ToolCallStart, got: {:?}", events);
    }

    #[test]
    fn parse_sse_content_block_delta_json() {
        let mut parser = AnthropicSseParser::new();
        parser.parse_chunk("content_block_start", r#"{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"toolu_001","name":"get_weather"}}"#);
        let data = r#"{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"city\""}}"#;
        let events = parser.parse_chunk("content_block_delta", data);
        let has_delta = events
            .iter()
            .any(|e| matches!(e, LlmEvent::ToolCallDelta { id, .. } if id == "toolu_001"));
        assert!(has_delta, "should emit ToolCallDelta, got: {:?}", events);
    }

    #[test]
    fn parse_sse_content_block_stop_tool_use() {
        let mut parser = AnthropicSseParser::new();
        parser.parse_chunk("content_block_start", r#"{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"toolu_001","name":"search"}}"#);
        let data = r#"{"type":"content_block_stop","index":0}"#;
        let events = parser.parse_chunk("content_block_stop", data);
        let has_end = events
            .iter()
            .any(|e| matches!(e, LlmEvent::ToolCallEnd { id } if id == "toolu_001"));
        assert!(has_end, "should emit ToolCallEnd, got: {:?}", events);
    }

    #[test]
    fn parse_sse_message_delta_done() {
        let mut parser = AnthropicSseParser::new();
        let data = r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":25}}"#;
        let events = parser.parse_chunk("message_delta", data);
        let has_done = events
            .iter()
            .any(|e| matches!(e, LlmEvent::Done { finish_reason, .. } if finish_reason == "stop"));
        assert!(
            has_done,
            "should emit Done with stop reason, got: {:?}",
            events
        );
    }

    #[test]
    fn parse_sse_message_start_tracks_input_tokens() {
        let mut parser = AnthropicSseParser::new();
        let data = r#"{"type":"message_start","message":{"id":"msg_001","type":"message","role":"assistant","model":"claude-sonnet-4-6","usage":{"input_tokens":42}}}"#;
        let events = parser.parse_chunk("message_start", data);
        assert!(events.is_empty(), "message_start should not emit events");
    }

    #[test]
    fn parse_sse_message_stop_ignored() {
        let mut parser = AnthropicSseParser::new();
        let events = parser.parse_chunk("message_stop", r#"{"type":"message_stop"}"#);
        assert!(events.is_empty());
    }

    #[test]
    fn parse_sse_ping_ignored() {
        let mut parser = AnthropicSseParser::new();
        let events = parser.parse_chunk("ping", r#"{"type":"ping"}"#);
        assert!(events.is_empty());
    }

    #[test]
    fn parse_sse_error_event() {
        let mut parser = AnthropicSseParser::new();
        let data = r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#;
        let events = parser.parse_chunk("error", data);
        assert_eq!(events.len(), 1);
        assert!(
            matches!(&events[0], LlmEvent::Error { message } if message.contains("Overloaded"))
        );
    }

    #[test]
    fn parse_sse_thinking_delta() {
        let mut parser = AnthropicSseParser::new();
        let data = r#"{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"Let me analyze..."}}"#;
        let events = parser.parse_chunk("content_block_delta", data);
        let has_reasoning = events.iter().any(
            |e| matches!(e, LlmEvent::Reasoning { content } if content == "Let me analyze..."),
        );
        assert!(
            has_reasoning,
            "should emit Reasoning for thinking_delta, got: {:?}",
            events
        );
    }

    #[test]
    fn anthropic_provider_has_no_lattice_core_imports() {
        let json = build_request_json(
            &ChatRequest::new(vec![Message::user("test")], ModelTarget::new("sonnet")),
            &make_resolved(),
        );
        assert_eq!(json["model"], "claude-sonnet-4-6");
    }
}
