// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Gemini provider: request JSON construction, SSE parsing,
//! and auth headers for the Gemini `generateContent` API.
//!
//! Uses Gemini-specific wire format: `systemInstruction`, role `"model"`,
//! `functionCall` / `functionResponse`, and `x-goog-api-key` auth.

use lattice_conversation::ProviderOrigin;

use crate::provider_projection::{ProjectionError, request_messages_to_wire, wire_usage};
use crate::router::ResolvedModel;
use crate::types::{ChatRequest, LlmEvent};

/// Build the JSON body for a Gemini `streamGenerateContent` request.
pub fn build_request_json(
    request: &ChatRequest,
    resolved: &ResolvedModel,
) -> Result<serde_json::Value, ProjectionError> {
    let projected = request_messages_to_wire(ProviderOrigin::Gemini, &request.messages)?;
    let mut body = serde_json::json!({
        "contents": projected.messages,
    });

    if let Some(system) = projected.system {
        body["systemInstruction"] = serde_json::json!({
            "parts": [{"text": system}]
        });
    }

    if !request.tools.is_empty() {
        let function_declarations: Vec<serde_json::Value> = request
            .tools
            .iter()
            .map(|tool| {
                // `parameters` 只接受 OpenAPI 子集，会拒绝工具 schema 用到的
                // `additionalProperties`、`const`、`contains` 等关键字；
                // `parametersJsonSchema` 接受完整 JSON Schema。
                serde_json::json!({
                    "name": tool.name,
                    "description": tool.description,
                    "parametersJsonSchema": tool.parameters,
                })
            })
            .collect();
        body["tools"] = serde_json::json!([{
            "functionDeclarations": function_declarations,
        }]);
    }

    let mut generation_config = serde_json::Map::new();
    if let Some(temperature) = request.temperature {
        generation_config.insert("temperature".to_string(), serde_json::json!(temperature));
    }
    if let Some(max_tokens) = request.max_tokens {
        generation_config.insert("maxOutputTokens".to_string(), serde_json::json!(max_tokens));
    }
    if !generation_config.is_empty() {
        body["generationConfig"] = serde_json::Value::Object(generation_config);
    }

    let _ = resolved;
    Ok(body)
}

/// Build the full Gemini streaming endpoint URL.
pub fn build_stream_url(base_url: &str, provider_model: &str) -> String {
    let encoded = urlencoding::encode(provider_model);
    format!(
        "{}/models/{}:streamGenerateContent?alt=sse",
        base_url.trim_end_matches('/'),
        encoded
    )
}

/// Build the Gemini auth header value (just the API key).
pub fn auth_header_value(api_key: &str) -> String {
    api_key.to_string()
}

/// Stateful parser for Gemini `streamGenerateContent` SSE.
#[derive(Default)]
pub struct GeminiSseParser {
    parts: Vec<serde_json::Value>,
    tool_call_indices: Vec<usize>,
    completed: bool,
}

impl GeminiSseParser {
    /// Create a new parser with no tracked state.
    pub fn new() -> Self {
        Self::default()
    }

    fn merge_part(&mut self, part: &serde_json::Value) -> (usize, bool) {
        if let Some(text) = part.get("text").and_then(|value| value.as_str()) {
            let thought = part.get("thought").and_then(|value| value.as_bool()) == Some(true);
            let signature = part.get("thoughtSignature");
            if let Some((index, previous)) = self.parts.iter_mut().enumerate().next_back()
                && previous.get("text").is_some()
                && (previous.get("thought").and_then(|value| value.as_bool()) == Some(true))
                    == thought
                && !(text.is_empty() && signature.is_some())
                && !(signature.is_some() && previous.get("thoughtSignature").is_some())
            {
                let mut merged = previous
                    .get("text")
                    .and_then(|value| value.as_str())
                    .unwrap_or("")
                    .to_string();
                merged.push_str(text);
                previous["text"] = serde_json::Value::String(merged);
                if let Some(signature) = signature {
                    previous["thoughtSignature"] = signature.clone();
                }
                return (index, false);
            }
        }

        let index = self.parts.len();
        self.parts.push(part.clone());
        if part.get("functionCall").is_some() {
            self.tool_call_indices.push(index);
        }
        (index, true)
    }

    fn completion_root(&self, root: &serde_json::Value) -> serde_json::Value {
        let mut complete = root.clone();
        complete["candidates"][0]["content"]["parts"] =
            serde_json::Value::Array(self.parts.clone());
        complete
    }

    /// Map Gemini finish reasons to internal finish-reason strings.
    fn map_finish_reason(reason: &str) -> String {
        match reason.to_uppercase().as_str() {
            "STOP" => "stop".to_string(),
            "MAX_TOKENS" => "length".to_string(),
            "SAFETY" | "RECITATION" => "content_filter".to_string(),
            "OTHER" => "unknown".to_string(),
            _ => "unknown".to_string(),
        }
    }

    /// Parse a Gemini SSE data chunk into [`LlmEvent`] values.
    pub fn parse_chunk(&mut self, _event_type: Option<&str>, data: &str) -> Vec<LlmEvent> {
        if self.completed {
            return Vec::new();
        }
        let root = match super::parse_json_data("Gemini", data) {
            Ok(Some(v)) => v,
            Ok(None) => return vec![],
            Err(e) => return vec![e],
        };

        if let Some(error_event) = super::extract_api_error(ProviderOrigin::Gemini, &root) {
            return vec![error_event];
        }

        let mut events = Vec::new();
        if let Some(cands) = root.get("candidates").and_then(|c| c.as_array())
            && let Some(cand) = cands.first()
        {
            let parts = cand
                .get("content")
                .and_then(|c| c.get("parts"))
                .and_then(|p| p.as_array());

            if let Some(parts_arr) = parts {
                for part in parts_arr {
                    if part.get("text").is_some() && part.get("functionCall").is_some() {
                        return vec![LlmEvent::error(
                            "malformed Gemini wire: one part contains text and functionCall",
                            false,
                        )];
                    }
                    let (index, is_new_part) = self.merge_part(part);
                    if part.get("thought").and_then(|value| value.as_bool()) == Some(true) {
                        if let Some(text) = part.get("text").and_then(|value| value.as_str())
                            && !text.is_empty()
                        {
                            events.push(LlmEvent::Reasoning {
                                content: text.to_string(),
                            });
                        }
                        continue;
                    }

                    if let Some(text) = part.get("text").and_then(|value| value.as_str())
                        && !text.is_empty()
                    {
                        events.push(LlmEvent::Token {
                            content: text.to_string(),
                        });
                    }

                    if is_new_part && let Some(function_call) = part.get("functionCall") {
                        let name = function_call
                            .get("name")
                            .and_then(|value| value.as_str())
                            .unwrap_or("");
                        let arguments = function_call
                            .get("args")
                            .cloned()
                            .unwrap_or_else(|| serde_json::json!({}));
                        let provider_call_id = function_call
                            .get("id")
                            .and_then(|value| value.as_str())
                            .map(str::to_string);
                        let arguments_delta = serde_json::to_string(&arguments).unwrap_or_default();

                        events.push(LlmEvent::ToolCallStart {
                            index,
                            provider_call_id,
                            name: name.to_string(),
                        });
                        events.push(LlmEvent::ToolCallDelta {
                            index,
                            arguments_delta,
                        });
                    }
                }
            }

            // Finish reason
            if let Some(reason) = cand.get("finishReason").and_then(|r| r.as_str()) {
                self.completed = true;
                let had_tool_calls = !self.tool_call_indices.is_empty();
                for index in &self.tool_call_indices {
                    events.push(LlmEvent::ToolCallEnd { index: *index });
                }

                // Safety ratings
                if let Some(ratings) = cand.get("safetyRatings").and_then(|r| r.as_array()) {
                    for rating in ratings {
                        if let (Some(cat), Some(prob)) = (
                            rating.get("category").and_then(|c| c.as_str()),
                            rating.get("probability").and_then(|p| p.as_str()),
                        ) {
                            events.push(LlmEvent::SafetyRating {
                                category: cat.to_string(),
                                probability: prob.to_string(),
                            });
                        }
                    }
                }

                // Grounding metadata (web search / Google Search)
                if let Some(grounding) = cand.get("groundingMetadata")
                    && let Some(chunks) =
                        grounding.get("groundingChunks").and_then(|c| c.as_array())
                {
                    for chunk in chunks {
                        if let Some(web) = chunk.get("web") {
                            let text = web
                                .get("title")
                                .or_else(|| web.get("snippet"))
                                .and_then(|t| t.as_str())
                                .unwrap_or("");
                            let url = web
                                .get("uri")
                                .and_then(|u| u.as_str())
                                .map(|s| s.to_string());
                            if !text.is_empty() || url.is_some() {
                                events.push(LlmEvent::GroundingChunk {
                                    text: text.to_string(),
                                    url,
                                });
                            }
                        }
                    }
                }

                let mapped = if had_tool_calls {
                    "tool_calls".to_string()
                } else {
                    Self::map_finish_reason(reason)
                };

                let usage = match wire_usage(ProviderOrigin::Gemini, &root) {
                    Ok(usage) => usage,
                    Err(error) => {
                        events.push(LlmEvent::error(error.to_string(), false));
                        return events;
                    }
                };

                let complete_root = self.completion_root(&root);
                let completion =
                    super::complete_message_event(ProviderOrigin::Gemini, &complete_root);
                self.parts.clear();
                self.tool_call_indices.clear();
                let failed = matches!(
                    completion,
                    LlmEvent::Error { .. } | LlmEvent::OutputLimit { .. }
                );
                events.push(completion);
                if failed {
                    return events;
                }
                events.push(LlmEvent::Done {
                    finish_reason: mapped,
                    usage,
                });
            }
        }

        events
    }
}

#[cfg(test)]
mod tests {
    use lattice_conversation::MessageSemantic;

    use super::*;

    #[test]
    fn parser_preserves_thought_state_and_optional_ids_without_synthesis() {
        for (id_field, expected_id) in [(",\"id\":\"real-id\"", Some("real-id")), ("", None)] {
            let mut parser = GeminiSseParser::new();
            let chunk = format!(
                r#"{{"candidates":[{{"content":{{"role":"model","parts":[{{"text":"reason","thought":true,"thoughtSignature":"sig"}},{{"functionCall":{{"name":"observe__workspace","args":{{}}{id_field}}}}}]}},"finishReason":"STOP"}}]}}"#
            );
            let events = parser.parse_chunk(None, &chunk);
            let message = events
                .iter()
                .find_map(|event| match event {
                    LlmEvent::MessageComplete { message } => Some(message),
                    _ => None,
                })
                .expect("complete message");
            assert_eq!(
                message.parts[0].replay_state.as_ref().unwrap().as_str(),
                "sig"
            );
            assert_eq!(
                message
                    .tool_calls()
                    .next()
                    .expect("ToolCall")
                    .1
                    .provider_call_id
                    .as_deref(),
                expected_id
            );
            assert!(!serde_json::to_string(message).unwrap().contains("tc_"));
        }
    }

    #[test]
    fn parser_completes_usage() {
        let mut parser = GeminiSseParser::new();
        let events = parser.parse_chunk(
            None,
            r#"{"candidates":[{"content":{"role":"model","parts":[{"text":"Done"}]},"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":10,"candidatesTokenCount":20,"totalTokenCount":30}}"#,
        );
        assert!(
            events
                .iter()
                .any(|event| matches!(event, LlmEvent::MessageComplete { .. }))
        );
        assert!(events.iter().any(|event| matches!(
            event,
            LlmEvent::Done { usage: Some(usage), .. } if usage.total_tokens == 30
        )));
        assert!(parser
            .parse_chunk(
                None,
                r#"{"candidates":[{"content":{"role":"model","parts":[]},"finishReason":"STOP"}]}"#,
            )
            .is_empty());
    }

    #[test]
    fn parser_accumulates_multichunk_reasoning_text_and_signature() {
        let mut parser = GeminiSseParser::new();
        for chunk in [
            r#"{"candidates":[{"content":{"role":"model","parts":[{"text":"rea","thought":true}]}}]}"#,
            r#"{"candidates":[{"content":{"role":"model","parts":[{"text":"son","thought":true,"thoughtSignature":"sig"}]}}]}"#,
            r#"{"candidates":[{"content":{"role":"model","parts":[{"text":"hel"}]}}]}"#,
        ] {
            assert!(
                parser
                    .parse_chunk(None, chunk)
                    .iter()
                    .all(|event| !matches!(event, LlmEvent::MessageComplete { .. }))
            );
        }
        let events = parser.parse_chunk(
            None,
            r#"{"candidates":[{"content":{"role":"model","parts":[{"text":"lo"}]},"finishReason":"STOP"}]}"#,
        );
        let message = events
            .iter()
            .find_map(|event| match event {
                LlmEvent::MessageComplete { message } => Some(message),
                _ => None,
            })
            .expect("complete message");
        assert_eq!(message.parts.len(), 2);
        assert!(matches!(
            &message.parts[0].semantic,
            MessageSemantic::Reasoning { text } if text == "reason"
        ));
        assert_eq!(
            message.parts[0].replay_state.as_ref().unwrap().as_str(),
            "sig"
        );
        assert!(matches!(
            &message.parts[1].semantic,
            MessageSemantic::Text { text } if text == "hello"
        ));
    }

    #[test]
    fn parser_preserves_distinct_signatures_and_rejects_union_parts() {
        let mut parser = GeminiSseParser::new();
        parser.parse_chunk(
            None,
            r#"{"candidates":[{"content":{"role":"model","parts":[{"text":"a","thought":true,"thoughtSignature":"sig-a"}]}}]}"#,
        );
        let events = parser.parse_chunk(
            None,
            r#"{"candidates":[{"content":{"role":"model","parts":[{"text":"b","thought":true,"thoughtSignature":"sig-b"}]},"finishReason":"STOP"}]}"#,
        );
        let message = events
            .iter()
            .find_map(|event| match event {
                LlmEvent::MessageComplete { message } => Some(message),
                _ => None,
            })
            .expect("complete message");
        assert_eq!(message.parts.len(), 2);
        assert_eq!(
            message.parts[0].replay_state.as_ref().unwrap().as_str(),
            "sig-a"
        );
        assert_eq!(
            message.parts[1].replay_state.as_ref().unwrap().as_str(),
            "sig-b"
        );

        let mut parser = GeminiSseParser::new();
        let events = parser.parse_chunk(
            None,
            r#"{"candidates":[{"content":{"role":"model","parts":[{"text":"x","functionCall":{"name":"observe__workspace","args":{}}}]}}]}"#,
        );
        assert!(events.iter().any(|event| matches!(
            event,
            LlmEvent::Error { failure } if !failure.retryable && failure.message.contains("text and functionCall")
        )));
        assert!(!events.iter().any(|event| matches!(
            event,
            LlmEvent::Token { .. } | LlmEvent::ToolCallStart { .. }
        )));
    }
}
