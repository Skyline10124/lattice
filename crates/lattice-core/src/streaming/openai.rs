use indexmap::IndexMap;
use serde_json::Value;

use super::{SseParser, StreamEvent, TokenUsage};

/// Parser for the OpenAI `chat.completion.chunk` SSE format.
///
/// Handles:
/// - `data: [DONE]` sentinel → ignored (transport signal, stream end is detected
///   when the event source returns `None`)
/// - Content delta chunks → [`StreamEvent::Token`]
/// - Tool-call delta chunks → [`StreamEvent::ToolCallStart`] / [`StreamEvent::ToolCallDelta`]
/// - Finish-reason chunks → [`StreamEvent::ToolCallEnd`] + [`StreamEvent::Done`]
/// - API error chunks → [`StreamEvent::Error`]
///
/// ## State
///
/// Tracks tool-call ids per `tool_calls[i].index` because OpenAI omits the `id`
/// field from subsequent delta chunks after the first one.
#[derive(Default)]
pub struct OpenAiSseParser {
    tool_call_ids: IndexMap<u32, String>,
}

impl OpenAiSseParser {
    pub fn new() -> Self {
        Self {
            tool_call_ids: IndexMap::new(),
        }
    }
}

impl SseParser for OpenAiSseParser {
    fn parse_chunk(
        &mut self,
        _event_type: &str,
        data: &str,
    ) -> Result<Vec<StreamEvent>, Box<dyn std::error::Error>> {
        let trimmed = data.trim();
        if trimmed == "[DONE]" {
            return Ok(vec![]);
        }

        let root: Value = serde_json::from_str(trimmed)?;

        if let Some(error) = root.get("error") {
            let msg = error["message"]
                .as_str()
                .unwrap_or("Unknown API error")
                .to_string();
            return Ok(vec![StreamEvent::Error { message: msg }]);
        }

        let mut events = Vec::new();

        if let Some(choices) = root["choices"].as_array() {
            for choice in choices {
                let delta = &choice["delta"];
                let finish_reason = choice["finish_reason"].as_str();

                // Reasoning (thinking) tokens stream before regular content tokens,
                // so check reasoning_content first.
                if let Some(reasoning) = delta["reasoning_content"].as_str() {
                    if !reasoning.is_empty() {
                        events.push(StreamEvent::Reasoning {
                            content: reasoning.to_string(),
                        });
                    }
                }

                if let Some(content) = delta["content"].as_str() {
                    if !content.is_empty() {
                        events.push(StreamEvent::Token {
                            content: content.to_string(),
                        });
                    }
                }

                if let Some(tool_calls) = delta["tool_calls"].as_array() {
                    for tc in tool_calls {
                        let idx = tc["index"].as_u64().unwrap_or(0) as u32;

                        if let Some(id) = tc["id"].as_str() {
                            let name = tc["function"]["name"].as_str().unwrap_or("");
                            self.tool_call_ids.insert(idx, id.to_string());
                            events.push(StreamEvent::ToolCallStart {
                                id: id.to_string(),
                                name: name.to_string(),
                            });
                        }

                        if let Some(args) = tc["function"]["arguments"].as_str() {
                            if !args.is_empty() {
                                if let Some(id) = self.tool_call_ids.get(&idx) {
                                    events.push(StreamEvent::ToolCallDelta {
                                        id: id.clone(),
                                        arguments_delta: args.to_string(),
                                    });
                                }
                            }
                        }
                    }
                }

                if let Some(reason) = finish_reason {
                    if !reason.is_empty() {
                        for id in self.tool_call_ids.drain(..).map(|(_, id)| id) {
                            events.push(StreamEvent::ToolCallEnd { id });
                        }

                        let usage = root["usage"].as_object().map(|u| TokenUsage {
                            prompt_tokens: u["prompt_tokens"].as_u64().unwrap_or(0) as u32,
                            completion_tokens: u["completion_tokens"].as_u64().unwrap_or(0) as u32,
                            total_tokens: u["total_tokens"].as_u64().unwrap_or(0) as u32,
                        });

                        events.push(StreamEvent::Done {
                            finish_reason: reason.to_string(),
                            usage,
                        });
                    }
                }
            }
        }

        Ok(events)
    }
}

#[cfg(test)]
mod tests;
