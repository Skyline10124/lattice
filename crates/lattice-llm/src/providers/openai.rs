// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! OpenAI-compatible provider: request JSON construction, SSE parsing,
//! and auth headers for the Chat Completions API.
//!
//! Supports OpenAI and OpenAI-compatible endpoints (Ollama, vLLM, LiteLLM, etc.).
//! Uses bounded SSE framing from `crate::sse` and never imports `lattice-core`.

use std::collections::BTreeMap;

use lattice_conversation::{ProviderOrigin, TokenUsage};

use crate::provider_projection::{ProjectionError, request_messages_to_wire, wire_usage};
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

#[derive(Default)]
struct OpenAiToolCall {
    provider_call_id: Option<String>,
    name: String,
    arguments: String,
    started: bool,
}

#[derive(Default)]
pub struct OpenAiSseParser {
    assistant_content: String,
    reasoning_content: String,
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
        let calls = std::mem::take(&mut self.tool_calls);
        let tool_calls: Vec<serde_json::Value> = calls
            .values()
            .map(|call| {
                let mut wire = serde_json::json!({
                    "type": "function",
                    "function": {
                        "name": wire_tool_name(&call.name),
                        "arguments": call.arguments,
                    }
                });
                if let Some(id) = &call.provider_call_id {
                    wire["id"] = serde_json::json!(id);
                }
                wire
            })
            .collect();
        let mut message = serde_json::json!({
            "role": "assistant",
            "content": if self.assistant_content.is_empty() {
                serde_json::Value::Null
            } else {
                serde_json::Value::String(std::mem::take(&mut self.assistant_content))
            },
        });
        if !self.reasoning_content.is_empty() {
            message["reasoning_content"] =
                serde_json::Value::String(std::mem::take(&mut self.reasoning_content));
        }
        if !tool_calls.is_empty() {
            message["tool_calls"] = serde_json::Value::Array(tool_calls);
        }
        crate::provider_projection::wire_to_native(
            ProviderOrigin::OpenAiCompatible,
            &serde_json::json!({"choices": [{"message": message}]}),
        )
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
                    self.reasoning_content.push_str(reasoning);
                    events.push(LlmEvent::Reasoning {
                        content: reasoning.to_string(),
                    });
                }

                if let Some(content) = delta.get("content").and_then(|value| value.as_str())
                    && !content.is_empty()
                {
                    self.assistant_content.push_str(content);
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
                        let call = self.tool_calls.entry(index).or_default();
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
                        if !call.started && !call.name.is_empty() {
                            call.started = true;
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
