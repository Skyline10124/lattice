use indexmap::IndexMap;
use serde_json::Value;

use super::{SseParser, StreamEvent, TokenUsage};

/// Parser for the Gemini `streamGenerateContent` SSE format.
///
/// Gemini SSE events use unnamed `data:` lines (no `event:` field).
/// Each `data:` payload is a JSON `GenerateContentResponse` chunk containing
/// candidates with content parts (text, functionCall, etc).
///
/// ## State
///
/// Tracks tool call IDs per functionCall index because Gemini streaming
/// emits complete functionCall objects in one chunk (not split across deltas
/// like OpenAI). We emit `ToolCallStart` + `ToolCallDelta` together, then
/// `ToolCallEnd` when the stream finishes.
///
/// ## Mapping
///
/// | Gemini chunk content          | StreamEvent(s)                               |
/// |-------------------------------|----------------------------------------------|
/// | `parts[].text`                | `Token` / `Reasoning` (if `thought: true`)  |
/// | `parts[].functionCall`        | `ToolCallStart` + `ToolCallDelta`            |
/// | `finishReason` present        | `ToolCallEnd` (for tracked IDs) + `Done`    |
/// | `usageMetadata` present       | `Done` (embedded)                            |
pub struct GeminiSseParser {
    tool_call_ids: IndexMap<usize, String>,
    tc_counter: usize,
}

impl GeminiSseParser {
    pub fn new() -> Self {
        Self {
            tool_call_ids: IndexMap::new(),
            tc_counter: 0,
        }
    }

    fn map_finish_reason(reason: &str) -> String {
        match reason.to_uppercase().as_str() {
            "STOP" => "stop".to_string(),
            "MAX_TOKENS" => "length".to_string(),
            "SAFETY" | "RECITATION" => "content_filter".to_string(),
            "OTHER" => "unknown".to_string(),
            _ => "unknown".to_string(),
        }
    }
}

impl Default for GeminiSseParser {
    fn default() -> Self {
        Self::new()
    }
}

impl SseParser for GeminiSseParser {
    fn parse_chunk(
        &mut self,
        _event_type: &str,
        data: &str,
    ) -> Result<Vec<StreamEvent>, Box<dyn std::error::Error>> {
        let trimmed = data.trim();
        if trimmed == "[DONE]" || trimmed.is_empty() {
            return Ok(vec![]);
        }

        let root: Value = serde_json::from_str(trimmed)?;

        if let Some(error) = root.get("error") {
            let msg = error["message"]
                .as_str()
                .unwrap_or("Unknown Gemini streaming error")
                .to_string();
            return Ok(vec![StreamEvent::Error { message: msg }]);
        }

        let mut events = Vec::new();
        let mut done_emitted = false;

        if let Some(cands) = root.get("candidates").and_then(|c| c.as_array()) {
            if let Some(cand) = cands.first() {
                let parts = cand
                    .get("content")
                    .and_then(|c| c.get("parts"))
                    .and_then(|p| p.as_array());

                let mut has_tool_calls = false;

                if let Some(parts_arr) = parts {
                    for part in parts_arr {
                        // Thinking/reasoning parts
                        if part.get("thought").and_then(|v| v.as_bool()) == Some(true) {
                            if let Some(text) = part.get("text").and_then(|t| t.as_str()) {
                                if !text.is_empty() {
                                    events.push(StreamEvent::Reasoning {
                                        content: text.to_string(),
                                    });
                                }
                            }
                            continue;
                        }

                        // Text parts
                        if let Some(text) = part.get("text").and_then(|t| t.as_str()) {
                            if !text.is_empty() {
                                events.push(StreamEvent::Token {
                                    content: text.to_string(),
                                });
                            }
                        }

                        // Function call parts — emit Start + Delta together
                        if let Some(fc) = part.get("functionCall") {
                            let name = fc.get("name").and_then(|n| n.as_str()).unwrap_or("");
                            let args: Value = fc
                                .get("args")
                                .cloned()
                                .unwrap_or(Value::Object(serde_json::Map::new()));
                            let arguments = serde_json::to_string(&args).unwrap_or_default();
                            let counter = self.tc_counter;
                            let id = format!("tc_{name}_{counter}");
                            self.tool_call_ids.insert(self.tc_counter, id.clone());
                            self.tc_counter += 1;

                            events.push(StreamEvent::ToolCallStart {
                                id: id.clone(),
                                name: name.to_string(),
                            });
                            events.push(StreamEvent::ToolCallDelta {
                                id,
                                arguments_delta: arguments,
                            });
                            has_tool_calls = true;
                        }
                    }
                }

                // Finish reason — primary Done emission point.
                // Gemini final chunks typically contain both finishReason and
                // usageMetadata; we emit a single Done that includes usage.
                if let Some(reason) = cand.get("finishReason").and_then(|r| r.as_str()) {
                    let had_tool_calls = has_tool_calls || !self.tool_call_ids.is_empty();
                    // Emit ToolCallEnd for all tracked tool calls
                    for (_, id) in self.tool_call_ids.drain(..) {
                        events.push(StreamEvent::ToolCallEnd { id });
                    }

                    let mapped = if had_tool_calls {
                        "tool_calls".to_string()
                    } else {
                        Self::map_finish_reason(reason)
                    };

                    let usage = root.get("usageMetadata").map(|u| TokenUsage {
                        prompt_tokens: u
                            .get("promptTokenCount")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(0) as u32,
                        completion_tokens: u
                            .get("candidatesTokenCount")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(0) as u32,
                        total_tokens: u
                            .get("totalTokenCount")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(0) as u32,
                    });

                    events.push(StreamEvent::Done {
                        finish_reason: mapped,
                        usage,
                    });
                    done_emitted = true;
                }
            }
        }

        // Fallback: usageMetadata without finishReason (intermediate usage chunk).
        // Only fires if finishReason was absent — no double-Done risk.
        if !done_emitted {
            if let Some(u) = root.get("usageMetadata") {
                events.push(StreamEvent::Done {
                    finish_reason: "stop".to_string(),
                    usage: Some(TokenUsage {
                        prompt_tokens: u
                            .get("promptTokenCount")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(0) as u32,
                        completion_tokens: u
                            .get("candidatesTokenCount")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(0) as u32,
                        total_tokens: u
                            .get("totalTokenCount")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(0) as u32,
                    }),
                });
            }
        }

        Ok(events)
    }
}

#[cfg(test)]
mod tests;
