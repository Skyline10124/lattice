#![allow(deprecated)]
use serde_json::{json, Value};

use crate::streaming::{StreamEvent, TokenUsage};
use crate::transport::{NormalizedMessages, NormalizedResponse, Transport};
use crate::types::{FunctionCall, Message, Role, ToolCall, ToolDefinition};

pub struct AnthropicTransport;

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

impl Transport for AnthropicTransport {
    fn normalize_messages(&self, messages: &[Message]) -> NormalizedMessages {
        let mut system: Option<String> = None;
        let mut result: Vec<Value> = Vec::new();

        for m in messages {
            match m.role {
                Role::System => {
                    system = Some(m.content.clone());
                }
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

    fn denormalize_response(&self, response: &Value) -> NormalizedResponse {
        let mut text_parts: Vec<String> = Vec::new();
        let mut reasoning_parts: Vec<String> = Vec::new();
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
                        if let Some(thinking) = block.get("thinking").and_then(|t| t.as_str()) {
                            reasoning_parts.push(thinking.to_string());
                        }
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

        NormalizedResponse {
            content: if text_parts.is_empty() {
                None
            } else {
                Some(text_parts.join(""))
            },
            tool_calls: if tool_calls.is_empty() {
                None
            } else {
                Some(tool_calls)
            },
            finish_reason,
            reasoning: if reasoning_parts.is_empty() {
                None
            } else {
                Some(reasoning_parts.join("\n\n"))
            },
        }
    }

    fn denormalize_stream_chunk(&self, event_type: &str, data: &Value) -> Vec<StreamEvent> {
        match event_type {
            "message_start" => vec![],
            "content_block_start" => {
                let block = &data["content_block"];
                match block.get("type").and_then(|t| t.as_str()) {
                    Some("tool_use") => {
                        let id = block["id"].as_str().unwrap_or("").to_string();
                        let name = block["name"].as_str().unwrap_or("").to_string();
                        vec![StreamEvent::ToolCallStart { id, name }]
                    }
                    _ => vec![],
                }
            }
            "content_block_delta" => {
                let delta = &data["delta"];
                match delta.get("type").and_then(|t| t.as_str()) {
                    Some("text_delta") => {
                        let text = delta["text"].as_str().unwrap_or("");
                        if text.is_empty() {
                            vec![]
                        } else {
                            vec![StreamEvent::Token {
                                content: text.to_string(),
                            }]
                        }
                    }
                    Some("input_json_delta") => {
                        let partial = delta["partial_json"].as_str().unwrap_or("");
                        let idx = data["index"].as_u64().unwrap_or(0) as u32;
                        if partial.is_empty() {
                            vec![]
                        } else {
                            // We need to track tool call IDs across chunks.
                            // Since this is a stateless function, we return a delta
                            // with an index-based placeholder. The AnthropicSseParser
                            // (streaming.rs) handles full stateful parsing for live streams.
                            vec![StreamEvent::ToolCallDelta {
                                id: format!("idx_{}", idx),
                                arguments_delta: partial.to_string(),
                            }]
                        }
                    }
                    Some("thinking_delta") => {
                        let thinking = delta["thinking"].as_str().unwrap_or("");
                        if thinking.is_empty() {
                            vec![]
                        } else {
                            vec![StreamEvent::Token {
                                content: thinking.to_string(),
                            }]
                        }
                    }
                    _ => vec![],
                }
            }
            "content_block_stop" => {
                let idx = data["index"].as_u64().unwrap_or(0) as u32;
                vec![StreamEvent::ToolCallEnd {
                    id: format!("idx_{}", idx),
                }]
            }
            "message_delta" => {
                let stop_reason = data["delta"]["stop_reason"].as_str().unwrap_or("end_turn");
                let finish_reason = map_stop_reason(stop_reason);
                let usage = data.get("usage").map(|u| TokenUsage {
                    prompt_tokens: 0,
                    completion_tokens: u["output_tokens"].as_u64().unwrap_or(0) as u32,
                    total_tokens: u["output_tokens"].as_u64().unwrap_or(0) as u32,
                });
                vec![StreamEvent::Done {
                    finish_reason,
                    usage,
                }]
            }
            "message_stop" | "ping" => vec![],
            _ => vec![],
        }
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
            tool_calls: None,
            tool_call_id: None,
            name: None,
        }
    }

    #[test]
    fn test_system_extraction() {
        let transport = AnthropicTransport;
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
    fn test_normalize_user_message() {
        let transport = AnthropicTransport;
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
        let transport = AnthropicTransport;
        let messages = vec![Message {
            role: Role::Assistant,
            content: "Let me check.".to_string(),
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
        let transport = AnthropicTransport;
        let messages = vec![Message {
            role: Role::Tool,
            content: r##"{"temp": 22}"##.to_string(),
            tool_calls: None,
            tool_call_id: Some("toolu_123".to_string()),
            name: Some("get_weather".to_string()),
        }];
        let result = transport.normalize_messages(&messages);
        assert_eq!(result.messages.len(), 1);
        let msg = &result.messages[0];
        // Anthropic wraps tool results in a user message
        assert_eq!(msg["role"], "user");
        let content = msg["content"].as_array().unwrap();
        assert_eq!(content[0]["type"], "tool_result");
        assert_eq!(content[0]["tool_use_id"], "toolu_123");
        assert_eq!(content[0]["content"], r##"{"temp": 22}"##);
    }

    #[test]
    fn test_denormalize_text_response() {
        let transport = AnthropicTransport;
        let response = json!({
            "content": [{"type": "text", "text": "Hello there!"}],
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 10, "output_tokens": 5},
        });
        let result = transport.denormalize_response(&response);
        assert_eq!(result.content, Some("Hello there!".to_string()));
        assert!(result.tool_calls.is_none());
        assert_eq!(result.finish_reason, "stop");
        assert!(result.reasoning.is_none());
    }

    #[test]
    fn test_denormalize_tool_use_response() {
        let transport = AnthropicTransport;
        let response = json!({
            "content": [
                {"type": "text", "text": "Checking weather..."},
                {"type": "tool_use", "id": "toolu_abc", "name": "get_weather", "input": {"city": "Tokyo"}},
            ],
            "stop_reason": "tool_use",
        });
        let result = transport.denormalize_response(&response);
        assert_eq!(result.content, Some("Checking weather...".to_string()));
        let tcs = result.tool_calls.expect("expected tool calls");
        assert_eq!(tcs.len(), 1);
        assert_eq!(tcs[0].id, "toolu_abc");
        assert_eq!(tcs[0].function.name, "get_weather");
        assert_eq!(tcs[0].function.arguments, r#"{"city":"Tokyo"}"#);
        assert_eq!(result.finish_reason, "tool_calls");
    }

    #[test]
    fn test_denormalize_thinking_response() {
        let transport = AnthropicTransport;
        let response = json!({
            "content": [
                {"type": "thinking", "thinking": "I should look up the weather."},
                {"type": "text", "text": "Let me check."},
            ],
            "stop_reason": "end_turn",
        });
        let result = transport.denormalize_response(&response);
        assert_eq!(result.content, Some("Let me check.".to_string()));
        assert_eq!(
            result.reasoning,
            Some("I should look up the weather.".to_string())
        );
        assert_eq!(result.finish_reason, "stop");
    }

    #[test]
    fn test_stream_text_delta() {
        let transport = AnthropicTransport;
        let data = json!({
            "type": "content_block_delta",
            "index": 0,
            "delta": {"type": "text_delta", "text": "Hello"},
        });
        let events = transport.denormalize_stream_chunk("content_block_delta", &data);
        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0],
            StreamEvent::Token {
                content: "Hello".to_string()
            }
        );
    }

    #[test]
    fn test_stream_tool_use_delta() {
        let transport = AnthropicTransport;

        let start_data = json!({
            "type": "content_block_start",
            "index": 0,
            "content_block": {"type": "tool_use", "id": "toolu_1", "name": "search"},
        });
        let events = transport.denormalize_stream_chunk("content_block_start", &start_data);
        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0],
            StreamEvent::ToolCallStart {
                id: "toolu_1".to_string(),
                name: "search".to_string(),
            }
        );

        let delta_data = json!({
            "type": "content_block_delta",
            "index": 0,
            "delta": {"type": "input_json_delta", "partial_json": r#"{"q":"ru"#},
        });
        let events = transport.denormalize_stream_chunk("content_block_delta", &delta_data);
        assert_eq!(events.len(), 1);
        match &events[0] {
            StreamEvent::ToolCallDelta {
                arguments_delta, ..
            } => {
                assert_eq!(arguments_delta, r#"{"q":"ru"#);
            }
            other => panic!("expected ToolCallDelta, got {other:?}"),
        }

        let stop_data = json!({"type": "content_block_stop", "index": 0});
        let events = transport.denormalize_stream_chunk("content_block_stop", &stop_data);
        assert_eq!(events.len(), 1);
        assert!(matches!(events[0], StreamEvent::ToolCallEnd { .. }));

        let msg_delta_data = json!({
            "type": "message_delta",
            "delta": {"stop_reason": "tool_use"},
            "usage": {"output_tokens": 50},
        });
        let events = transport.denormalize_stream_chunk("message_delta", &msg_delta_data);
        assert_eq!(events.len(), 1);
        match &events[0] {
            StreamEvent::Done {
                finish_reason,
                usage,
            } => {
                assert_eq!(finish_reason, "tool_calls");
                let usage = usage.as_ref().expect("expected usage");
                assert_eq!(usage.completion_tokens, 50);
            }
            other => panic!("expected Done, got {other:?}"),
        }
    }

    #[test]
    fn test_stream_ignored_events() {
        let transport = AnthropicTransport;

        let events = transport.denormalize_stream_chunk(
            "message_start",
            &json!({"type": "message_start", "message": {}}),
        );
        assert!(events.is_empty());

        let events = transport.denormalize_stream_chunk("ping", &json!({}));
        assert!(events.is_empty());

        let events = transport.denormalize_stream_chunk("message_stop", &json!({}));
        assert!(events.is_empty());
    }

    #[test]
    fn test_normalize_tools_uses_input_schema() {
        let transport = AnthropicTransport;
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
        let transport = AnthropicTransport;
        let messages = vec![
            Message {
                role: Role::Tool,
                content: "sunny".to_string(),
                tool_calls: None,
                tool_call_id: Some("toolu_1".to_string()),
                name: None,
            },
            Message {
                role: Role::Tool,
                content: "rainy".to_string(),
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
        let transport = AnthropicTransport;
        let messages = vec![Message {
            role: Role::Assistant,
            content: String::new(),
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
        let transport = AnthropicTransport;
        let response = json!({
            "content": [{"type": "text", "text": "Truncated..."}],
            "stop_reason": "max_tokens",
        });
        let result = transport.denormalize_response(&response);
        assert_eq!(result.finish_reason, "length");
    }
}
