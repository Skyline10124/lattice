// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! OpenAI-compatible provider: request JSON construction, SSE parsing,
//! and auth headers for the Chat Completions API.
//!
//! Supports OpenAI and OpenAI-compatible endpoints (Ollama, vLLM, LiteLLM, etc.).
//! Uses bounded SSE framing from `crate::sse` and never imports `lattice-core`.

use std::collections::BTreeMap;

use lattice_conversation::{ProviderOrigin, RawToolCall, TokenUsage};

use crate::provider_projection::{
    OpenAiPart, ProjectionError, request_messages_to_wire, wire_usage,
};
use crate::router::ResolvedModel;
use crate::types::{ChatRequest, LlmEvent};

/// Encode/decode CEP dotted Tool names at the OpenAI wire boundary.
pub(crate) fn wire_tool_name(canonical: &str) -> String {
    crate::provider_projection::openai_wire_tool_name(canonical)
}

pub(crate) fn canonical_tool_name(wire: &str) -> String {
    crate::provider_projection::openai_canonical_tool_name(wire)
}

/// Build the JSON body for an OpenAI Chat Completions request.
///
/// LATTICE's provider response path is SSE-only — `dispatch_stream` parses
/// Server-Sent Events and never decodes a non-stream JSON body. The request
/// is therefore always built as a streaming request, with `stream_options`
/// asking the provider to include the final usage chunk.
pub fn build_request_json(
    request: &ChatRequest,
    resolved: &ResolvedModel,
) -> Result<serde_json::Value, ProjectionError> {
    let projected = request_messages_to_wire(ProviderOrigin::OpenAiCompatible, &request.messages)?;
    let mut messages = Vec::new();
    if let Some(system) = projected.system {
        messages.push(serde_json::json!({"role": "system", "content": system}));
    }
    messages.extend(projected.messages);

    let mut body = serde_json::json!({
        "model": resolved.provider_model,
        "messages": messages,
        "stream": true,
        "stream_options": {"include_usage": true},
    });

    if !request.tools.is_empty() {
        let tools_array: Vec<serde_json::Value> = request
            .tools
            .iter()
            .map(|tool| {
                serde_json::json!({
                    "type": "function",
                    "function": {
                        "name": wire_tool_name(&tool.name),
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

    if let Some(max_comp_tokens) = request.max_completion_tokens {
        body["max_completion_tokens"] = serde_json::json!(max_comp_tokens);
    } else if let Some(max_tokens) = request.max_tokens {
        body["max_tokens"] = serde_json::json!(max_tokens);
    }

    Ok(body)
}

pub fn auth_header_value(api_key: &str) -> String {
    format!("Bearer {}", api_key)
}

struct OpenAiToolCall {
    part_ordinal: usize,
    started: bool,
}

#[derive(Default)]
pub struct OpenAiSseParser {
    parts: Vec<OpenAiPart>,
    tool_calls: BTreeMap<u32, OpenAiToolCall>,
    pending_finish_reason: Option<String>,
    completed: bool,
}

impl OpenAiSseParser {
    pub fn new() -> Self {
        Self::default()
    }

    fn complete_message(
        &mut self,
    ) -> Result<
        lattice_conversation::NativeMessage<lattice_conversation::UnboundToolCall>,
        ProjectionError,
    > {
        crate::provider_projection::openai_parts_to_native(std::mem::take(&mut self.parts))
    }
    fn finish(&mut self, usage: Option<TokenUsage>) -> Vec<LlmEvent> {
        self.completed = true;
        let finish_reason = self
            .pending_finish_reason
            .take()
            .unwrap_or_else(|| "stop".to_string());
        let mut events = self
            .tool_calls
            .keys()
            .map(|index| LlmEvent::ToolCallEnd {
                index: *index as usize,
            })
            .collect::<Vec<_>>();
        let completion = super::projection_result_event(self.complete_message());
        let failed = matches!(completion, LlmEvent::Error { .. });
        events.push(completion);
        if !failed {
            events.push(LlmEvent::Done {
                finish_reason,
                usage,
            });
        }
        events
    }

    /// Parse a single SSE data chunk into [`LlmEvent`] values.
    pub fn parse_chunk(&mut self, _event_type: Option<&str>, data: &str) -> Vec<LlmEvent> {
        if self.completed {
            return Vec::new();
        }
        if data.trim() == "[DONE]" {
            return if self.pending_finish_reason.is_some() {
                self.finish(None)
            } else {
                Vec::new()
            };
        }
        let root = match super::parse_json_data("OpenAI", data) {
            Ok(Some(v)) => v,
            Ok(None) => return vec![],
            Err(e) => return vec![e],
        };

        if let Some(error_event) = super::extract_api_error(&root) {
            return vec![error_event];
        }

        let mut events = Vec::new();

        if let Some(choices) = root.get("choices").and_then(|c| c.as_array()) {
            for choice in choices {
                let delta = &choice["delta"];
                let finish_reason = choice["finish_reason"].as_str();

                if let Some(reasoning) = delta
                    .get("reasoning_content")
                    .and_then(|value| value.as_str())
                    && !reasoning.is_empty()
                {
                    if let Some(OpenAiPart::Reasoning(text)) = self.parts.last_mut() {
                        text.push_str(reasoning);
                    } else {
                        self.parts
                            .push(OpenAiPart::Reasoning(reasoning.to_string()));
                    }
                    events.push(LlmEvent::Reasoning {
                        content: reasoning.to_string(),
                    });
                }

                if let Some(content) = delta.get("content").and_then(|value| value.as_str())
                    && !content.is_empty()
                {
                    if let Some(OpenAiPart::Text(text)) = self.parts.last_mut() {
                        text.push_str(content);
                    } else {
                        self.parts.push(OpenAiPart::Text(content.to_string()));
                    }
                    events.push(LlmEvent::Token {
                        content: content.to_string(),
                    });
                }

                if let Some(tool_calls) = delta.get("tool_calls").and_then(|value| value.as_array())
                {
                    for tool_call in tool_calls {
                        let Some(index) = tool_call
                            .get("index")
                            .and_then(|value| value.as_u64())
                            .and_then(|value| u32::try_from(value).ok())
                        else {
                            return vec![LlmEvent::Error {
                                message:
                                    "malformed OpenAI wire: tool call index is missing or too large"
                                        .to_string(),
                            }];
                        };
                        let slot = self.tool_calls.entry(index).or_insert_with(|| {
                            let part_ordinal = self.parts.len();
                            self.parts.push(OpenAiPart::ToolCall(RawToolCall {
                                provider_call_id: None,
                                name: String::new(),
                                arguments: String::new(),
                            }));
                            OpenAiToolCall {
                                part_ordinal,
                                started: false,
                            }
                        });
                        let OpenAiPart::ToolCall(call) = &mut self.parts[slot.part_ordinal] else {
                            unreachable!("tool index always points to its original call part");
                        };
                        if let Some(id) = tool_call.get("id").and_then(|value| value.as_str()) {
                            call.provider_call_id = Some(id.to_string());
                        }
                        if let Some(name) = tool_call
                            .get("function")
                            .and_then(|function| function.get("name"))
                            .and_then(|value| value.as_str())
                            && !name.is_empty()
                        {
                            call.name = canonical_tool_name(name);
                        }
                        if !slot.started && !call.name.is_empty() {
                            slot.started = true;
                            events.push(LlmEvent::ToolCallStart {
                                index: index as usize,
                                provider_call_id: call.provider_call_id.clone(),
                                name: call.name.clone(),
                            });
                        }
                        if let Some(arguments) = tool_call
                            .get("function")
                            .and_then(|function| function.get("arguments"))
                            .and_then(|value| value.as_str())
                            && !arguments.is_empty()
                        {
                            call.arguments.push_str(arguments);
                            events.push(LlmEvent::ToolCallDelta {
                                index: index as usize,
                                arguments_delta: arguments.to_string(),
                            });
                        }
                    }
                }

                if let Some(reason) = finish_reason
                    && !reason.is_empty()
                {
                    self.pending_finish_reason = Some(reason.to_string());
                }
            }
        }

        if self.pending_finish_reason.is_some()
            && root.get("usage").is_some_and(|usage| !usage.is_null())
        {
            match wire_usage(ProviderOrigin::OpenAiCompatible, &root) {
                Ok(usage) => events.extend(self.finish(usage)),
                Err(error) => {
                    self.completed = true;
                    events.push(LlmEvent::Error {
                        message: error.to_string(),
                    });
                }
            }
        }

        events
    }
}

#[cfg(test)]
mod tests {
    use lattice_conversation::{
        BoundToolCall, MessagePart, MessageRole, MessageSemantic, NativeMessage,
    };

    use super::*;
    use crate::endpoints::OPENAI_BASE_URL;
    use crate::router::{CredentialStatus, ProviderKind};
    use crate::types::{ChatRequest, ModelTarget, ToolDefinition};

    fn make_resolved() -> ResolvedModel {
        ResolvedModel {
            alias: "gpt-4o".to_string(),
            provider: ProviderKind::OpenAi,
            provider_model: "gpt-4o".to_string(),
            base_url: OPENAI_BASE_URL.to_string(),
            api_key_env: "OPENAI_API_KEY".to_string(),
            credential_status: CredentialStatus::Present,
        }
    }

    fn text(role: MessageRole, content: &str) -> NativeMessage<BoundToolCall> {
        NativeMessage::new(
            role,
            None,
            vec![MessagePart::semantic_only(MessageSemantic::Text {
                text: content.to_string(),
            })],
        )
    }

    #[test]
    fn request_envelope_uses_typed_projection() {
        let request = ChatRequest {
            messages: vec![
                text(MessageRole::System, "You are helpful."),
                text(MessageRole::User, "Hello!"),
            ],
            tools: vec![ToolDefinition::new(
                "observe.workspace",
                "Observe",
                serde_json::json!({"type": "object"}),
            )],
            target: ModelTarget::new("gpt-4o"),
            temperature: Some(0.7),
            max_tokens: Some(1024),
            max_completion_tokens: None,
        };
        let body = build_request_json(&request, &make_resolved()).expect("projection");
        assert_eq!(body["model"], "gpt-4o");
        assert_eq!(body["messages"][0]["role"], "system");
        assert_eq!(body["messages"][1]["content"], "Hello!");
        assert_eq!(body["tools"][0]["function"]["name"], "observe__workspace");
        assert_eq!(body["stream"], true);
        assert_eq!(body["stream_options"]["include_usage"], true);
        assert_eq!(body["temperature"], 0.7);
        assert_eq!(body["max_tokens"], 1024);
    }

    #[test]
    fn parser_preserves_real_and_missing_ids_without_synthesis() {
        for (id_fragment, expected_id) in [(",\"id\":\"call_abc\"", Some("call_abc")), ("", None)] {
            let mut parser = OpenAiSseParser::new();
            let chunk = format!(
                r#"{{"choices":[{{"delta":{{"tool_calls":[{{"index":0{id_fragment},"type":"function","function":{{"name":"observe__workspace","arguments":"{{}}"}}}}]}},"finish_reason":"tool_calls"}}]}}"#
            );
            let mut events = parser.parse_chunk(None, &chunk);
            events.extend(parser.parse_chunk(None, "[DONE]"));
            assert!(events.iter().any(|event| matches!(
                event,
                LlmEvent::ToolCallStart {
                    index: 0,
                    provider_call_id,
                    name,
                } if provider_call_id.as_deref() == expected_id && name == "observe.workspace"
            )));
            let message = events
                .iter()
                .find_map(|event| match event {
                    LlmEvent::MessageComplete { message } => Some(message),
                    _ => None,
                })
                .expect("complete message");
            let call = message.tool_calls().next().expect("ToolCall").1;
            assert_eq!(call.provider_call_id.as_deref(), expected_id);
            assert!(!serde_json::to_string(message).unwrap().contains("tc_"));
        }
    }

    #[test]
    fn parser_completes_text_and_usage() {
        let mut parser = OpenAiSseParser::new();
        let finish_events = parser.parse_chunk(
            None,
            r#"{"choices":[{"delta":{"content":"hello"},"finish_reason":"stop"}],"usage":null}"#,
        );
        assert!(!finish_events.iter().any(|event| matches!(
            event,
            LlmEvent::MessageComplete { .. } | LlmEvent::Done { .. }
        )));

        let events = parser.parse_chunk(
            None,
            r#"{"choices":[],"usage":{"prompt_tokens":1,"completion_tokens":2,"total_tokens":3}}"#,
        );
        assert!(
            events
                .iter()
                .any(|event| matches!(event, LlmEvent::MessageComplete { .. }))
        );
        assert!(events.iter().any(|event| matches!(
            event,
            LlmEvent::Done { usage: Some(usage), .. } if usage.total_tokens == 3
        )));
        assert!(parser.parse_chunk(None, "[DONE]").is_empty());
    }

    #[test]
    fn parser_accumulates_multichunk_name_id_and_arguments() {
        let mut parser = OpenAiSseParser::new();
        let first = parser.parse_chunk(
            None,
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":"observe__workspace","arguments":"{\"path\""}}]},"finish_reason":null}]}"#,
        );
        assert!(first.iter().any(|event| matches!(
            event,
            LlmEvent::ToolCallStart {
                index: 0,
                provider_call_id: Some(id),
                name,
            } if id == "call_1" && name == "observe.workspace"
        )));
        assert!(
            !first
                .iter()
                .any(|event| matches!(event, LlmEvent::MessageComplete { .. }))
        );

        let mut second = parser.parse_chunk(
            None,
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":":\"a\"}"}}]},"finish_reason":"tool_calls"}]}"#,
        );
        second.extend(parser.parse_chunk(None, "[DONE]"));
        let message = second
            .iter()
            .find_map(|event| match event {
                LlmEvent::MessageComplete { message } => Some(message),
                _ => None,
            })
            .expect("complete message");
        let call = message.tool_calls().next().expect("ToolCall").1;
        assert_eq!(call.provider_call_id.as_deref(), Some("call_1"));
        assert_eq!(call.name, "observe.workspace");
        assert_eq!(call.arguments, serde_json::json!({"path": "a"}));
    }

    #[test]
    fn parser_rejects_malformed_usage_and_missing_tool_index() {
        let mut parser = OpenAiSseParser::new();
        let finish = parser.parse_chunk(
            None,
            r#"{"choices":[{"delta":{"content":"x"},"finish_reason":"stop"}],"usage":null}"#,
        );
        assert!(
            !finish
                .iter()
                .any(|event| matches!(event, LlmEvent::MessageComplete { .. }))
        );
        let events = parser.parse_chunk(
            None,
            r#"{"choices":[],"usage":{"prompt_tokens":"bad","completion_tokens":2,"total_tokens":3}}"#,
        );
        assert!(
            events
                .iter()
                .any(|event| matches!(event, LlmEvent::Error { .. }))
        );
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, LlmEvent::MessageComplete { .. }))
        );

        let mut parser = OpenAiSseParser::new();
        let events = parser.parse_chunk(
            None,
            r#"{"choices":[{"delta":{"tool_calls":[{"function":{"name":"observe__workspace","arguments":"{}"}}]},"finish_reason":null}]}"#,
        );
        assert!(events.iter().any(|event| matches!(
            event,
            LlmEvent::Error { message } if message.contains("index")
        )));
    }

    fn delta(parser: &mut OpenAiSseParser, delta: serde_json::Value) -> Vec<LlmEvent> {
        parser.parse_chunk(
            None,
            &serde_json::json!({"choices": [{"delta": delta}]}).to_string(),
        )
    }

    fn finish(parser: &mut OpenAiSseParser) -> Vec<LlmEvent> {
        let mut events =
            parser.parse_chunk(None, r#"{"choices":[{"delta":{},"finish_reason":"stop"}]}"#);
        events.extend(parser.parse_chunk(None, "[DONE]"));
        events
    }

    fn completed(events: &[LlmEvent]) -> &NativeMessage<lattice_conversation::UnboundToolCall> {
        assert_eq!(
            events
                .iter()
                .filter(|e| matches!(e, LlmEvent::MessageComplete { .. }))
                .count(),
            1,
        );
        events
            .iter()
            .find_map(|e| match e {
                LlmEvent::MessageComplete { message } => Some(message),
                _ => None,
            })
            .unwrap()
    }

    fn labels(message: &NativeMessage<lattice_conversation::UnboundToolCall>) -> Vec<String> {
        message
            .parts
            .iter()
            .map(|p| match &p.semantic {
                MessageSemantic::Text { text } => format!("T:{text}"),
                MessageSemantic::Reasoning { text } => format!("R:{text}"),
                MessageSemantic::ToolCall { call } => format!("C:{}", call.name),
                _ => panic!("unexpected result"),
            })
            .collect()
    }

    fn call_delta(index: u32, name: &str, arguments: &str) -> serde_json::Value {
        serde_json::json!({"tool_calls": [{"index": index, "function": {
            "name": name, "arguments": arguments
        }}]})
    }

    #[test]
    fn order_f01_reasoning_text_reasoning_matches_live_segments() {
        let mut parser = OpenAiSseParser::new();
        let mut live = Vec::new();
        for (field, value) in [
            ("reasoning_content", "想"),
            ("content", "答"),
            ("reasoning_content", "再想"),
        ] {
            live.extend(delta(&mut parser, serde_json::json!({field: value})));
        }
        let live: Vec<_> = live
            .iter()
            .map(|e| match e {
                LlmEvent::Reasoning { content } => format!("R:{content}"),
                LlmEvent::Token { content } => format!("T:{content}"),
                _ => panic!("unexpected event"),
            })
            .collect();
        assert_eq!(live, ["R:想", "T:答", "R:再想"]);
        assert_eq!(labels(completed(&finish(&mut parser))), live);
    }

    #[test]
    fn order_f02_same_chunk_follows_emitted_order() {
        let mut parser = OpenAiSseParser::new();
        delta(&mut parser, serde_json::json!({"content": "A"}));
        let events = delta(
            &mut parser,
            serde_json::json!({
                "content": "B", "reasoning_content": "R",
                "tool_calls": [{"index": 8, "function": {"name": "t", "arguments": "{}"}}]
            }),
        );
        assert!(matches!(
            &events[..],
            [
                LlmEvent::Reasoning { .. },
                LlmEvent::Token { .. },
                LlmEvent::ToolCallStart { index: 8, .. },
                LlmEvent::ToolCallDelta { index: 8, .. }
            ]
        ));
        assert_eq!(
            labels(completed(&finish(&mut parser))),
            ["T:A", "R:R", "T:B", "C:t"]
        );
    }

    #[test]
    fn order_f03_tool_separates_text_parts() {
        let mut parser = OpenAiSseParser::new();
        delta(&mut parser, serde_json::json!({"content": "A"}));
        delta(&mut parser, call_delta(0, "t", "{}"));
        delta(&mut parser, serde_json::json!({"content": "B"}));
        assert_eq!(
            labels(completed(&finish(&mut parser))),
            ["T:A", "C:t", "T:B"]
        );
    }

    #[test]
    fn order_f04_sparse_indices_follow_first_arrival() {
        let mut parser = OpenAiSseParser::new();
        for (index, name) in [(u32::MAX, "first"), (7, "second"), (1, "third")] {
            delta(&mut parser, call_delta(index, name, "{}"));
        }
        assert_eq!(
            labels(completed(&finish(&mut parser))),
            ["C:first", "C:second", "C:third"]
        );
    }

    #[test]
    fn order_f05_late_metadata_and_fragments_fill_original_slot() {
        let mut parser = OpenAiSseParser::new();
        let first = delta(
            &mut parser,
            serde_json::json!({"tool_calls": [{
                "index": 9, "function": {"arguments": "{\"path\":"}
            }]}),
        );
        assert!(matches!(
            &first[..],
            [LlmEvent::ToolCallDelta { index: 9, .. }]
        ));
        delta(&mut parser, serde_json::json!({"content": "between"}));
        delta(&mut parser, call_delta(2, "second", "{}"));
        let later = delta(
            &mut parser,
            serde_json::json!({"tool_calls": [{
                "index": 9, "id": "real-id", "function": {"name": "observe__workspace", "arguments": "\"a\"}"}
            }]}),
        );
        assert!(matches!(
            &later[..],
            [
                LlmEvent::ToolCallStart { index: 9, .. },
                LlmEvent::ToolCallDelta { index: 9, .. }
            ]
        ));
        let events = finish(&mut parser);
        let message = completed(&events);
        assert_eq!(
            labels(message),
            ["C:observe.workspace", "T:between", "C:second"]
        );
        let calls: Vec<_> = message.tool_calls().collect();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].1.provider_call_id.as_deref(), Some("real-id"));
        assert_eq!(calls[0].1.arguments, serde_json::json!({"path": "a"}));
        assert_eq!(calls[1].1.arguments, serde_json::json!({}));
    }

    #[test]
    fn order_f06_usage_and_done_complete_once() {
        for mode in 0..3 {
            let mut parser = OpenAiSseParser::new();
            let mut chunk = serde_json::json!({"choices": [{"delta": {"content": "A"}, "finish_reason": "stop"}]});
            let usage =
                serde_json::json!({"prompt_tokens": 1, "completion_tokens": 2, "total_tokens": 3});
            if mode == 0 {
                chunk["usage"] = usage.clone();
            }
            let mut events = parser.parse_chunk(None, &chunk.to_string());
            if mode != 0 {
                assert!(!events.iter().any(|e| matches!(
                    e,
                    LlmEvent::MessageComplete { .. } | LlmEvent::Done { .. }
                )));
            }
            if mode == 1 {
                events.extend(parser.parse_chunk(
                    None,
                    &serde_json::json!({"choices": [], "usage": usage}).to_string(),
                ));
            }
            events.extend(parser.parse_chunk(None, "[DONE]"));
            assert_eq!(labels(completed(&events)), ["T:A"]);
            let done: Vec<_> = events
                .iter()
                .filter_map(|e| match e {
                    LlmEvent::Done {
                        finish_reason,
                        usage,
                    } => Some((finish_reason.as_str(), *usage)),
                    _ => None,
                })
                .collect();
            assert_eq!(
                done,
                vec![(
                    "stop",
                    if mode == 2 {
                        None
                    } else {
                        Some(TokenUsage {
                            prompt_tokens: 1,
                            completion_tokens: 2,
                            total_tokens: 3,
                        })
                    }
                )]
            );
            assert!(parser.parse_chunk(None, "[DONE]").is_empty());
            assert!(delta(&mut parser, serde_json::json!({"content": "late"})).is_empty());
        }
    }

    #[test]
    fn order_f07_empty_fragments_do_not_split_adjacent_parts() {
        let mut parser = OpenAiSseParser::new();
        assert!(
            delta(
                &mut parser,
                serde_json::json!({"content": "", "reasoning_content": ""})
            )
            .is_empty()
        );
        for (field, value) in [
            ("content", "A"),
            ("reasoning_content", ""),
            ("content", "B"),
            ("reasoning_content", "R"),
            ("content", ""),
            ("reasoning_content", "S"),
        ] {
            delta(&mut parser, serde_json::json!({field: value}));
        }
        assert_eq!(labels(completed(&finish(&mut parser))), ["T:AB", "R:RS"]);
        let mut empty = OpenAiSseParser::new();
        assert!(completed(&finish(&mut empty)).parts.is_empty());
    }

    fn assert_limit(events: &[LlmEvent], kind: &str) {
        assert!(
            events
                .iter()
                .any(|e| matches!(e, LlmEvent::Error { message } if message.contains(kind))),
            "expected limit {kind}"
        );
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, LlmEvent::MessageComplete { .. } | LlmEvent::Done { .. }))
        );
    }

    #[test]
    fn order_f08_text_and_reasoning_limits_span_parts() {
        use lattice_conversation::{MAX_ASSISTANT_CONTENT_BYTES, MAX_REASONING_CONTENT_BYTES};
        for (field, other, limit, kind) in [
            (
                "content",
                "reasoning_content",
                MAX_ASSISTANT_CONTENT_BYTES,
                "AssistantContent",
            ),
            (
                "reasoning_content",
                "content",
                MAX_REASONING_CONTENT_BYTES,
                "ReasoningContent",
            ),
        ] {
            for extra in ["", "!"] {
                let mut parser = OpenAiSseParser::new();
                delta(
                    &mut parser,
                    serde_json::json!({field: "é".repeat(limit / 4)}),
                );
                delta(&mut parser, serde_json::json!({other: "separator"}));
                delta(
                    &mut parser,
                    serde_json::json!({field: format!("{}{extra}", "é".repeat(limit / 4))}),
                );
                let events = finish(&mut parser);
                if extra.is_empty() {
                    completed(&events);
                } else {
                    assert_limit(&events, kind);
                }
            }
        }
    }

    #[test]
    fn order_f09_interleaved_call_and_total_limits() {
        use lattice_conversation::{MAX_MODEL_ATTEMPT_BYTES, MAX_TOOL_CALL_ARGUMENTS_BYTES};
        let limit = MAX_TOOL_CALL_ARGUMENTS_BYTES;
        let args = format!("{{\"x\":\"{}\"}}", "z".repeat(limit - 8));
        assert_eq!(args.len(), limit);
        for extra in ["", " "] {
            let mut parser = OpenAiSseParser::new();
            delta(&mut parser, call_delta(9, "first", &args[..limit / 2]));
            delta(&mut parser, call_delta(2, "second", "{}"));
            delta(
                &mut parser,
                call_delta(9, "", &format!("{}{extra}", &args[limit / 2..])),
            );
            let events = finish(&mut parser);
            if extra.is_empty() {
                completed(&events);
            } else {
                assert_limit(&events, "ToolCallArguments");
            }
        }
        for extra in [false, true] {
            let mut parser = OpenAiSseParser::new();
            for index in 0..(MAX_MODEL_ATTEMPT_BYTES / limit) as u32 {
                delta(&mut parser, call_delta(index, "t", &args));
            }
            if extra {
                delta(&mut parser, serde_json::json!({"content": "!"}));
            }
            let events = finish(&mut parser);
            if extra {
                assert_limit(&events, "ModelAttemptTotal");
            } else {
                completed(&events);
            }
        }
    }

    #[test]
    fn order_f10_invalid_batches_never_publish_partial_messages() {
        use lattice_conversation::MAX_TOOL_CALLS_PER_BATCH;
        for case in 0..3 {
            let mut parser = OpenAiSseParser::new();
            delta(&mut parser, serde_json::json!({"content": "prefix"}));
            let count = if case == 2 {
                MAX_TOOL_CALLS_PER_BATCH + 1
            } else {
                2
            };
            for index in 0..count {
                let mut call = call_delta(
                    index as u32,
                    "t",
                    if case == 0 && index == 1 { "{" } else { "{}" },
                );
                if case == 1 {
                    call["tool_calls"][0]["id"] = serde_json::json!("duplicate");
                }
                delta(&mut parser, call);
            }
            let events = finish(&mut parser);
            assert!(
                events
                    .iter()
                    .any(|e| matches!(e, LlmEvent::MessageRejected { .. })),
                "case {case}"
            );
            assert!(
                !events
                    .iter()
                    .any(|e| matches!(e, LlmEvent::MessageComplete { .. }))
            );
        }
    }

    fn ordered_native() -> NativeMessage<lattice_conversation::UnboundToolCall> {
        use lattice_conversation::UnboundToolCall;
        NativeMessage::new(
            MessageRole::Assistant,
            Some(ProviderOrigin::OpenAiCompatible),
            vec![
                MessagePart::semantic_only(MessageSemantic::Text { text: "A".into() }),
                MessagePart::semantic_only(MessageSemantic::Reasoning { text: "R".into() }),
                MessagePart::semantic_only(MessageSemantic::ToolCall {
                    call: UnboundToolCall {
                        provider_call_id: Some("real".into()),
                        name: "observe.workspace".into(),
                        arguments: serde_json::json!({"z": 1, "a": 2}),
                    },
                }),
                MessagePart::semantic_only(MessageSemantic::Text { text: "B".into() }),
                MessagePart::semantic_only(MessageSemantic::Reasoning { text: "S".into() }),
            ],
        )
    }

    #[test]
    fn order_f11_request_merges_without_changing_wire_shape() {
        let message = ordered_native();
        let wire =
            crate::provider_projection::native_to_wire(ProviderOrigin::OpenAiCompatible, &message)
                .unwrap();
        assert_eq!(
            wire,
            serde_json::json!({
                "role": "assistant", "content": "AB", "reasoning_content": "RS",
                "tool_calls": [{"id": "real", "type": "function", "function": {"name": "observe__workspace", "arguments": "{\"a\":2,\"z\":1}"}}]
            })
        );
    }

    #[test]
    fn order_f12_cross_provider_preserves_remaining_order() {
        let source = ordered_native();
        let original = source.clone();
        let projected =
            crate::provider_projection::project_cross_provider(&source, ProviderOrigin::Gemini);
        assert_eq!(labels(&projected), ["T:A", "C:observe.workspace", "T:B"]);
        assert!(
            projected
                .tool_calls()
                .next()
                .unwrap()
                .1
                .provider_call_id
                .is_none()
        );
        assert_eq!(source, original);
    }

    #[test]
    fn auth_and_name_mapping_stay_at_the_wire_boundary() {
        assert_eq!(auth_header_value("sk-test"), "Bearer sk-test");
        assert_eq!(wire_tool_name("session.todo_write"), "session__todo_write");
        assert_eq!(
            canonical_tool_name("session__todo_write"),
            "session.todo_write"
        );
    }
}
