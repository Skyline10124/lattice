use std::collections::HashMap;

use serde_json::Value;

use super::{SseParser, StreamEvent, TokenUsage};

/// Maps Anthropic stop reasons to OpenAI-style finish reasons, matching the
/// logic in [`AnthropicTransport::denormalize_response`].
const ANTHROPIC_STOP_REASON_MAP: &[(&str, &str)] = &[
    ("end_turn", "stop"),
    ("tool_use", "tool_calls"),
    ("max_tokens", "length"),
    ("stop_sequence", "stop"),
    ("error", "error"),
];

fn map_stop_reason(reason: &str) -> String {
    ANTHROPIC_STOP_REASON_MAP
        .iter()
        .find(|(k, _)| *k == reason)
        .map(|(_, v)| v.to_string())
        .unwrap_or_else(|| reason.to_string())
}

/// Parser for the Anthropic message-streaming SSE format.
///
/// Anthropic uses named SSE events (`message_start`, `content_block_delta`, …)
/// rather than the OpenAI-style all-in-one JSON chunks.  This parser maps each
/// event type to the corresponding [`StreamEvent`] variant.
///
/// ## Event mapping
///
/// | SSE event              | StreamEvent(s)                                  |
/// |------------------------|-------------------------------------------------|
/// | `message_start`        | _(ignored — metadata)_                          |
/// | `content_block_start`  | `ToolCallStart` (if `type = "tool_use"`)        |
/// | `content_block_delta`  | `Token` / `ToolCallDelta`                       |
/// | `content_block_stop`   | `ToolCallEnd` (if it was a tool_use block)      |
/// | `message_delta`        | `Done`                                          |
/// | `message_stop`         | _(ignored — redundant with `message_delta`)_    |
/// | `ping`                 | _(ignored — keep-alive)_                        |
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
}

impl SseParser for AnthropicSseParser {
    fn parse_chunk(
        &mut self,
        event_type: &str,
        data: &str,
    ) -> Result<Vec<StreamEvent>, Box<dyn std::error::Error>> {
        if data.trim().is_empty() {
            return Ok(vec![]);
        }

        let root: Value = serde_json::from_str(data)?;

        match event_type {
            "message_start" => {
                if let Some(msg) = root.get("message") {
                    self.input_tokens = msg["usage"]["input_tokens"].as_u64().unwrap_or(0) as u32;
                }
                Ok(vec![])
            }
            "content_block_start" => {
                let idx = root["index"].as_u64().unwrap_or(0) as u32;
                let block = &root["content_block"];
                match block["type"].as_str() {
                    Some("tool_use") => {
                        let id = block["id"].as_str().unwrap_or("").to_string();
                        let name = block["name"].as_str().unwrap_or("").to_string();
                        self.tool_call_ids.insert(idx, id.clone());
                        Ok(vec![StreamEvent::ToolCallStart { id, name }])
                    }
                    _ => Ok(vec![]),
                }
            }
            "content_block_delta" => {
                let idx = root["index"].as_u64().unwrap_or(0) as u32;
                let delta = &root["delta"];
                match delta["type"].as_str() {
                    Some("text_delta") => {
                        let text = delta["text"].as_str().unwrap_or("");
                        if text.is_empty() {
                            Ok(vec![])
                        } else {
                            Ok(vec![StreamEvent::Token {
                                content: text.to_string(),
                            }])
                        }
                    }
                    Some("thinking_delta") => {
                        let thinking = delta["thinking"].as_str().unwrap_or("");
                        if thinking.is_empty() {
                            Ok(vec![])
                        } else {
                            Ok(vec![StreamEvent::Reasoning {
                                content: thinking.to_string(),
                            }])
                        }
                    }
                    Some("input_json_delta") => {
                        let partial = delta["partial_json"].as_str().unwrap_or("");
                        if partial.is_empty() {
                            Ok(vec![])
                        } else if let Some(id) = self.tool_call_ids.get(&idx).cloned() {
                            Ok(vec![StreamEvent::ToolCallDelta {
                                id,
                                arguments_delta: partial.to_string(),
                            }])
                        } else {
                            Ok(vec![])
                        }
                    }
                    _ => Ok(vec![]),
                }
            }
            "content_block_stop" => {
                let idx = root["index"].as_u64().unwrap_or(0) as u32;
                if let Some(id) = self.tool_call_ids.remove(&idx) {
                    Ok(vec![StreamEvent::ToolCallEnd { id }])
                } else {
                    Ok(vec![])
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
                Ok(vec![StreamEvent::Done {
                    finish_reason,
                    usage,
                }])
            }
            "message_stop" | "ping" => Ok(vec![]),
            "error" => {
                let msg = root["error"]["message"]
                    .as_str()
                    .or_else(|| root["error"]["type"].as_str())
                    .unwrap_or("Unknown Anthropic streaming error")
                    .to_string();
                Ok(vec![StreamEvent::Error { message: msg }])
            }
            _ => Ok(vec![]),
        }
    }
}

#[cfg(test)]
mod tests;
