use crate::transport::TransportBase;
use serde_json::{json, Value};
use std::collections::HashMap;

use crate::provider::{ChatRequest, ChatResponse};
use crate::streaming::{AnthropicSseParser, SseParser, TokenUsage};
use crate::transport::chat_completions::TransportError;
use crate::transport::{NormalizedMessages, Transport};
use crate::types::{FunctionCall, Message, Role, ToolCall, ToolDefinition};

pub struct AnthropicTransport {
    base: TransportBase,
}

const STOP_REASON_MAP: &[(&str, &str)] = &[
    ("end_turn", "stop"),
    ("tool_use", "tool_calls"),
    ("max_tokens", "length"),
    ("stop_sequence", "stop"),
];

fn map_stop_reason(reason: &str) -> String {
    STOP_REASON_MAP
        .iter()
        .find(|(k, _)| *k == reason)
        .map(|(_, v)| v.to_string())
        .unwrap_or_else(|| "stop".to_string())
}

impl AnthropicTransport {
    pub fn new() -> Self {
        Self {
            base: TransportBase::new("https://api.anthropic.com"),
        }
    }
}

impl Default for AnthropicTransport {
    fn default() -> Self {
        Self::new()
    }
}

impl Transport for AnthropicTransport {
    fn base_url(&self) -> &str {
        self.base.base_url()
    }

    fn extra_headers(&self) -> &HashMap<String, String> {
        self.base.extra_headers()
    }

    fn api_mode(&self) -> &str {
        "anthropic"
    }

    fn normalize_request(&self, request: &ChatRequest) -> Result<Value, TransportError> {
        let normalized = self.normalize_messages(&request.messages);
        let mut body = serde_json::json!({
            "model": request.model,
            "messages": normalized.messages,
            "max_tokens": request.max_tokens.unwrap_or(4096),
        });

        if let Some(system) = normalized.system {
            body["system"] = serde_json::Value::String(system);
        }

        if !request.tools.is_empty() {
            let tools = self.normalize_tools(&request.tools);
            body["tools"] = serde_json::Value::Array(tools);
        }

        self.apply_temperature(&mut body, request.temperature);
        self.set_stream_flag(&mut body, request.stream);

        Ok(body)
    }

    fn denormalize_response(&self, response: &Value) -> Result<ChatResponse, TransportError> {
        let mut text_parts: Vec<String> = Vec::new();
        let mut tool_calls: Vec<ToolCall> = Vec::new();

        if let Some(content) = response.get("content").and_then(|c| c.as_array()) {
            for block in content {
                let block_type = block.get("type").and_then(|t| t.as_str()).unwrap_or("");
                match block_type {
                    "text" => {
                        if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                            text_parts.push(text.to_string());
                        }
                    }
                    "thinking" => {
                        // Reasoning discarded in unified transport; extracted at
                        // the provider layer if needed.
                    }
                    "tool_use" => {
                        let id = block
                            .get("id")
                            .and_then(|i| i.as_str())
                            .unwrap_or("")
                            .to_string();
                        let name = block
                            .get("name")
                            .and_then(|n| n.as_str())
                            .unwrap_or("")
                            .to_string();
                        let input = block.get("input").cloned().unwrap_or(json!({}));
                        let arguments = serde_json::to_string(&input).unwrap_or_default();
                        tool_calls.push(ToolCall {
                            id,
                            function: FunctionCall { name, arguments },
                        });
                    }
                    _ => {}
                }
            }
        }

        let stop_reason = response
            .get("stop_reason")
            .and_then(|r| r.as_str())
            .unwrap_or("end_turn");
        let finish_reason = map_stop_reason(stop_reason);

        let model = response
            .get("model")
            .and_then(|m| m.as_str())
            .unwrap_or("")
            .to_string();

        let usage = response.get("usage").map(|u| TokenUsage {
            prompt_tokens: u["input_tokens"].as_u64().unwrap_or(0) as u32,
            completion_tokens: u["output_tokens"].as_u64().unwrap_or(0) as u32,
            total_tokens: (u["input_tokens"].as_u64().unwrap_or(0)
                + u["output_tokens"].as_u64().unwrap_or(0)) as u32,
        });

        Ok(ChatResponse {
            content: if text_parts.is_empty() {
                None
            } else {
                Some(text_parts.join(""))
            },
            reasoning_content: None,
            tool_calls: if tool_calls.is_empty() {
                None
            } else {
                Some(tool_calls)
            },
            usage,
            finish_reason,
            model,
        })
    }

    fn normalize_messages(&self, messages: &[Message]) -> NormalizedMessages {
        let mut system: Option<String> = None;
        let mut result: Vec<Value> = Vec::new();

        for m in messages {
            match m.role {
                Role::System => match system {
                    Some(ref mut existing) => {
                        existing.push_str("\n\n");
                        existing.push_str(&m.content);
                    }
                    None => system = Some(m.content.clone()),
                },
                Role::User => {
                    let content = if m.content.is_empty() {
                        json!([{"type": "text", "text": "(empty message)"}])
                    } else {
                        json!([{"type": "text", "text": m.content}])
                    };
                    result.push(json!({"role": "user", "content": content}));
                }
                Role::Assistant => {
                    let mut blocks: Vec<Value> = Vec::new();
                    if !m.content.is_empty() {
                        blocks.push(json!({"type": "text", "text": m.content}));
                    }
                    if let Some(tool_calls) = &m.tool_calls {
                        for tc in tool_calls {
                            let input: Value =
                                serde_json::from_str(&tc.function.arguments).unwrap_or(json!({}));
                            blocks.push(json!({
                                "type": "tool_use",
                                "id": tc.id,
                                "name": tc.function.name,
                                "input": input,
                            }));
                        }
                    }
                    if blocks.is_empty() {
                        blocks.push(json!({"type": "text", "text": "(empty)"}));
                    }
                    result.push(json!({"role": "assistant", "content": blocks}));
                }
                Role::Tool => {
                    let tool_use_id = m.tool_call_id.clone().unwrap_or_default();
                    let content_val = if m.content.is_empty() {
                        "(no output)".to_string()
                    } else {
                        m.content.clone()
                    };
                    let tool_result = json!({
                        "type": "tool_result",
                        "tool_use_id": tool_use_id,
                        "content": content_val,
                    });
                    // Merge consecutive tool results into one user message
                    if let Some(last) = result.last_mut() {
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
                    result.push(json!({"role": "user", "content": [tool_result]}));
                }
            }
        }

        NormalizedMessages {
            system,
            messages: result,
        }
    }

    fn normalize_tools(&self, tools: &[ToolDefinition]) -> Vec<Value> {
        tools
            .iter()
            .map(|t| {
                json!({
                    "name": t.name,
                    "description": t.description,
                    "input_schema": t.parameters,
                })
            })
            .collect()
    }

    fn chat_endpoint(&self) -> &str {
        "/v1/messages"
    }

    fn auth_header_name(&self) -> &str {
        "x-api-key"
    }

    fn auth_header_value(&self, api_key: &str) -> String {
        api_key.to_string()
    }

    fn create_sse_parser(&self) -> Box<dyn SseParser> {
        Box::new(AnthropicSseParser::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Role;

    fn make_message(role: Role, content: &str) -> Message {
        Message {
            role,
            content: content.to_string(),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: None,
            name: None,
        }
    }

    #[test]
    fn test_system_extraction() {
        let transport = AnthropicTransport::new();
        let messages = vec![
            make_message(Role::System, "You are helpful."),
            make_message(Role::User, "Hello"),
        ];
        let result = transport.normalize_messages(&messages);
        assert_eq!(result.system, Some("You are helpful.".to_string()));
        assert_eq!(result.messages.len(), 1);
        assert_eq!(result.messages[0]["role"], "user");
    }

    #[test]
    fn test_multiple_system_messages_merged() {
        let transport = AnthropicTransport::new();
        let messages = vec![
            make_message(Role::System, "First system prompt."),
            make_message(Role::System, "Second system prompt."),
            make_message(Role::User, "Hello"),
        ];
        let result = transport.normalize_messages(&messages);
        assert_eq!(
            result.system,
            Some("First system prompt.\n\nSecond system prompt.".to_string())
        );
        assert_eq!(result.messages.len(), 1);
        assert_eq!(result.messages[0]["role"], "user");
    }

    #[test]
    fn test_normalize_user_message() {
        let transport = AnthropicTransport::new();
        let messages = vec![make_message(Role::User, "Hello, world!")];
        let result = transport.normalize_messages(&messages);
        assert!(result.system.is_none());
        assert_eq!(result.messages.len(), 1);
        let msg = &result.messages[0];
        assert_eq!(msg["role"], "user");
        let content = msg["content"].as_array().unwrap();
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[0]["text"], "Hello, world!");
    }

    #[test]
    fn test_normalize_assistant_with_tool_use() {
        let transport = AnthropicTransport::new();
        let messages = vec![Message {
            role: Role::Assistant,
            content: "Let me check.".to_string(),
            reasoning_content: None,
            tool_calls: Some(vec![ToolCall {
                id: "toolu_123".to_string(),
                function: FunctionCall {
                    name: "get_weather".to_string(),
                    arguments: r#"{"city":"Paris"}"#.to_string(),
                },
            }]),
            tool_call_id: None,
            name: None,
        }];
        let result = transport.normalize_messages(&messages);
        assert_eq!(result.messages.len(), 1);
        let msg = &result.messages[0];
        assert_eq!(msg["role"], "assistant");
        let content = msg["content"].as_array().unwrap();
        assert_eq!(content.len(), 2);
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[1]["type"], "tool_use");
        assert_eq!(content[1]["id"], "toolu_123");
        assert_eq!(content[1]["name"], "get_weather");
        assert_eq!(content[1]["input"]["city"], "Paris");
    }

    #[test]
    fn test_normalize_tool_result() {
        let transport = AnthropicTransport::new();
        let messages = vec![Message {
            role: Role::Tool,
            content: r##"{"temp": 22}"##.to_string(),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: Some("toolu_123".to_string()),
            name: Some("get_weather".to_string()),
        }];
        let result = transport.normalize_messages(&messages);
        assert_eq!(result.messages.len(), 1);
        let msg = &result.messages[0];
        assert_eq!(msg["role"], "user");
        let content = msg["content"].as_array().unwrap();
        assert_eq!(content[0]["type"], "tool_result");
        assert_eq!(content[0]["tool_use_id"], "toolu_123");
        assert_eq!(content[0]["content"], r##"{"temp": 22}"##);
    }

    #[test]
    fn test_denormalize_text_response() {
        let transport = AnthropicTransport::new();
        let response = json!({
            "content": [{"type": "text", "text": "Hello there!"}],
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 10, "output_tokens": 5},
        });
        let result = transport.denormalize_response(&response).unwrap();
        assert_eq!(result.content.as_deref(), Some("Hello there!"));
        assert!(result.tool_calls.is_none());
        assert_eq!(result.finish_reason, "stop");
        let usage = result.usage.expect("expected usage from response");
        assert_eq!(usage.prompt_tokens, 10);
        assert_eq!(usage.completion_tokens, 5);
        assert_eq!(usage.total_tokens, 15);
        assert_eq!(result.model, "");
    }

    #[test]
    fn test_denormalize_tool_use_response() {
        let transport = AnthropicTransport::new();
        let response = json!({
            "content": [
                {"type": "text", "text": "Checking weather..."},
                {"type": "tool_use", "id": "toolu_abc", "name": "get_weather", "input": {"city": "Tokyo"}},
            ],
            "stop_reason": "tool_use",
        });
        let result = transport.denormalize_response(&response).unwrap();
        assert_eq!(result.content.as_deref(), Some("Checking weather..."));
        let tcs = result.tool_calls.expect("expected tool calls");
        assert_eq!(tcs.len(), 1);
        assert_eq!(tcs[0].id, "toolu_abc");
        assert_eq!(tcs[0].function.name, "get_weather");
        assert_eq!(tcs[0].function.arguments, r#"{"city":"Tokyo"}"#);
        assert_eq!(result.finish_reason, "tool_calls");
    }

    #[test]
    fn test_denormalize_thinking_response() {
        let transport = AnthropicTransport::new();
        let response = json!({
            "content": [
                {"type": "thinking", "thinking": "I should look up the weather."},
                {"type": "text", "text": "Let me check."},
            ],
            "stop_reason": "end_turn",
        });
        let result = transport.denormalize_response(&response).unwrap();
        assert_eq!(result.content.as_deref(), Some("Let me check."));
        // Reasoning is discarded in unified transport
        assert_eq!(result.finish_reason, "stop");
    }

    #[test]
    fn test_normalize_tools_uses_input_schema() {
        let transport = AnthropicTransport::new();
        let tools = vec![ToolDefinition {
            name: "get_weather".to_string(),
            description: "Get weather".to_string(),
            parameters: json!({"type": "object", "properties": {"city": {"type": "string"}}, "required": ["city"]}),
        }];
        let result = transport.normalize_tools(&tools);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0]["name"], "get_weather");
        assert!(result[0].get("input_schema").is_some());
        assert!(result[0].get("parameters").is_none());
    }

    #[test]
    fn test_consecutive_tool_results_merged() {
        let transport = AnthropicTransport::new();
        let messages = vec![
            Message {
                role: Role::Tool,
                content: "sunny".to_string(),
                reasoning_content: None,
                tool_calls: None,
                tool_call_id: Some("toolu_1".to_string()),
                name: None,
            },
            Message {
                role: Role::Tool,
                content: "rainy".to_string(),
                reasoning_content: None,
                tool_calls: None,
                tool_call_id: Some("toolu_2".to_string()),
                name: None,
            },
        ];
        let result = transport.normalize_messages(&messages);
        assert_eq!(result.messages.len(), 1);
        let content = result.messages[0]["content"].as_array().unwrap();
        assert_eq!(content.len(), 2);
        assert_eq!(content[0]["tool_use_id"], "toolu_1");
        assert_eq!(content[1]["tool_use_id"], "toolu_2");
    }

    #[test]
    fn test_empty_assistant_gets_placeholder() {
        let transport = AnthropicTransport::new();
        let messages = vec![Message {
            role: Role::Assistant,
            content: String::new(),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: None,
            name: None,
        }];
        let result = transport.normalize_messages(&messages);
        let content = result.messages[0]["content"].as_array().unwrap();
        assert_eq!(content[0]["text"], "(empty)");
    }

    #[test]
    fn test_max_tokens_stop_reason() {
        let transport = AnthropicTransport::new();
        let response = json!({
            "content": [{"type": "text", "text": "Truncated..."}],
            "stop_reason": "max_tokens",
        });
        let result = transport.denormalize_response(&response).unwrap();
        assert_eq!(result.finish_reason, "length");
    }

    #[test]
    fn test_api_mode() {
        let transport = AnthropicTransport::new();
        assert_eq!(transport.api_mode(), "anthropic");
    }

    #[test]
    fn test_default_base_url() {
        let transport = AnthropicTransport::new();
        assert_eq!(transport.base_url(), "https://api.anthropic.com");
    }

    #[test]
    fn test_extra_headers_default_empty() {
        let transport = AnthropicTransport::new();
        assert!(transport.extra_headers().is_empty());
    }
}
