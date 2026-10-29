// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Messages 的请求构造与 SSE 解析。

use lattice_conversation::ProviderOrigin;
use std::collections::BTreeMap;

use crate::protocol::projection::{ProjectionError, request_messages_to_wire, wire_usage};
use crate::reply::{
    PartDelta, PartKind, ProtocolEvent, ReplyProtocolError, StreamStop, StringPatch,
};
use crate::types::ChatRequest;

/// Messages 必须发送输出上限；元数据没有给出时使用此值。
const DEFAULT_MAX_OUTPUT_TOKENS: u64 = 32_768;

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

/// 按模型名构造 Messages 流式请求。
pub fn build_request_json(
    request: &ChatRequest,
    model: &str,
    output_limit: Option<u64>,
) -> Result<serde_json::Value, ProjectionError> {
    let projected = request_messages_to_wire(ProviderOrigin::Anthropic, &request.messages)?;
    let mut body = serde_json::json!({
        "model": model,
        "messages": projected.messages,
        "max_tokens": output_limit.unwrap_or(DEFAULT_MAX_OUTPUT_TOKENS),
        "stream": true,
    });

    if let Some(system) = projected.system {
        body["system"] = serde_json::Value::String(system);
    }

    if !request.tools.is_empty() {
        let tools: Vec<serde_json::Value> = request
            .tools
            .iter()
            .map(|tool| {
                serde_json::json!({
                    "name": tool.name,
                    "description": tool.description,
                    "input_schema": tool.parameters,
                })
            })
            .collect();
        body["tools"] = serde_json::Value::Array(tools);
    }

    Ok(body)
}

pub const ANTHROPIC_VERSION: &str = "2023-06-01";

struct AnthropicBlock {
    kind: PartKind,
    initial_arguments: bool,
    argument_deltas_started: bool,
}

#[derive(Default)]
pub struct AnthropicSseParser {
    blocks: BTreeMap<usize, AnthropicBlock>,
    input_tokens: Option<u64>,
    completed: bool,
}

impl AnthropicSseParser {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn parse_chunk(&mut self, event_type: &str, data: &str) -> Vec<ProtocolEvent> {
        if self.completed {
            return Vec::new();
        }
        let root = match super::common::parse_json_data("Anthropic Messages", data) {
            Ok(Some(root)) => root,
            Ok(None) => return vec![],
            Err(error) => return vec![error],
        };
        let mut events = Vec::new();
        if event_type.starts_with("content_block_") {
            let Some(index) = root["index"]
                .as_u64()
                .and_then(|index| usize::try_from(index).ok())
            else {
                return vec![ProtocolEvent::error(
                    "malformed Anthropic Messages wire: content index is missing or too large",
                    false,
                )];
            };
            match event_type {
                "content_block_start" => {
                    let block = &root["content_block"];
                    let kind = match block["type"].as_str() {
                        Some("text") => PartKind::Text,
                        Some("thinking") => PartKind::Reasoning,
                        Some("tool_use") => PartKind::ToolCall,
                        other => {
                            return vec![ProtocolEvent::unsupported(
                                "anthropic-messages",
                                index,
                                other.unwrap_or("<missing>"),
                            )];
                        }
                    };
                    if self.blocks.contains_key(&index) {
                        return vec![ProtocolEvent::Failed(StreamStop::Protocol(
                            ReplyProtocolError::InvalidSequence {
                                index,
                                detail: "duplicate content block start",
                            },
                        ))];
                    }
                    events.push(ProtocolEvent::Part(PartDelta::Begin { index, kind }));
                    let mut state = AnthropicBlock {
                        kind,
                        initial_arguments: false,
                        argument_deltas_started: false,
                    };
                    match kind {
                        PartKind::Text => {
                            let text = block["text"].as_str().unwrap_or("");
                            events.push(ProtocolEvent::Part(PartDelta::Text {
                                index,
                                delta: text.into(),
                            }));
                        }
                        PartKind::Reasoning => {
                            let text = block["thinking"].as_str().unwrap_or("");
                            events.push(ProtocolEvent::Part(PartDelta::Reasoning {
                                index,
                                delta: text.into(),
                            }));
                            if let Some(signature) = block["signature"].as_str() {
                                events.push(ProtocolEvent::Part(PartDelta::ReplayState {
                                    index,
                                    patch: StringPatch::Set(signature.into()),
                                }));
                            }
                        }
                        PartKind::ToolCall => {
                            events.push(ProtocolEvent::Part(PartDelta::ToolMetadata {
                                index,
                                name: block["name"]
                                    .as_str()
                                    .map(|name| StringPatch::Set(name.into())),
                                provider_call_id: block["id"]
                                    .as_str()
                                    .map(|id| StringPatch::Set(id.into())),
                            }));
                            if let Some(input) = block.get("input").filter(|value| {
                                !value.as_object().is_some_and(serde_json::Map::is_empty)
                            }) {
                                state.initial_arguments = true;
                                events.push(ProtocolEvent::Part(PartDelta::ToolArguments {
                                    index,
                                    delta: input.to_string(),
                                }));
                            }
                        }
                    }
                    self.blocks.insert(index, state);
                }
                "content_block_delta" => {
                    let delta = &root["delta"];
                    let block_type = delta["type"].as_str().unwrap_or("<missing>");
                    let translated = match block_type {
                        "text_delta" => PartDelta::Text {
                            index,
                            delta: delta["text"].as_str().unwrap_or("").into(),
                        },
                        "thinking_delta" => PartDelta::Reasoning {
                            index,
                            delta: delta["thinking"].as_str().unwrap_or("").into(),
                        },
                        "signature_delta" => PartDelta::ReplayState {
                            index,
                            patch: StringPatch::Append(
                                delta["signature"].as_str().unwrap_or("").into(),
                            ),
                        },
                        "input_json_delta" => {
                            let Some(state) = self
                                .blocks
                                .get_mut(&index)
                                .filter(|state| state.kind == PartKind::ToolCall)
                            else {
                                return vec![ProtocolEvent::error(
                                    "malformed Anthropic Messages wire: argument delta has no Tool block",
                                    false,
                                )];
                            };
                            if state.initial_arguments {
                                return vec![ProtocolEvent::error(
                                    "malformed Anthropic Messages wire: Tool input is present both at start and in deltas",
                                    false,
                                )];
                            }
                            state.argument_deltas_started = true;
                            PartDelta::ToolArguments {
                                index,
                                delta: delta["partial_json"].as_str().unwrap_or("").into(),
                            }
                        }
                        // 已识别的可选展示元数据没有语义或回放要求。
                        "citation_delta" | "citations_delta" => return events,
                        other => {
                            return vec![ProtocolEvent::unsupported(
                                "anthropic-messages",
                                index,
                                other,
                            )];
                        }
                    };
                    events.push(ProtocolEvent::Part(translated));
                }
                "content_block_stop" => {
                    if let Some(state) = self.blocks.get(&index)
                        && state.kind == PartKind::ToolCall
                        && !state.initial_arguments
                        && !state.argument_deltas_started
                    {
                        events.push(ProtocolEvent::Part(PartDelta::ToolArguments {
                            index,
                            delta: "{}".into(),
                        }));
                    }
                    events.push(ProtocolEvent::Part(PartDelta::End { index }));
                }
                _ => {}
            }
            return events;
        }
        match event_type {
            "message_start" => {
                let message = &root["message"];
                self.input_tokens = message["usage"]["input_tokens"].as_u64();
                if message["usage"].get("output_tokens").is_some() {
                    match wire_usage(ProviderOrigin::Anthropic, message) {
                        Ok(Some(usage)) => events.push(ProtocolEvent::Usage(usage)),
                        Ok(None) => {}
                        Err(error) => events.push(ProtocolEvent::error(error.to_string(), false)),
                    }
                }
            }
            "message_delta" => {
                let usage_wire = serde_json::json!({"usage": {
                    "input_tokens": self.input_tokens,
                    "output_tokens": root["usage"].get("output_tokens"),
                }});
                match wire_usage(ProviderOrigin::Anthropic, &usage_wire) {
                    Ok(Some(usage)) => events.push(ProtocolEvent::Usage(usage)),
                    Ok(None) => {}
                    Err(error) => return vec![ProtocolEvent::error(error.to_string(), false)],
                }
                if let Some(stop_reason) = root["delta"]["stop_reason"].as_str() {
                    self.completed = true;
                    events.push(ProtocolEvent::Done {
                        finish_reason: map_stop_reason(stop_reason),
                    });
                }
            }
            "error" => events.push(
                super::common::extract_api_error(ProviderOrigin::Anthropic, &root).unwrap_or_else(
                    || ProtocolEvent::error("unknown Anthropic streaming error", false),
                ),
            ),
            "message_stop" | "ping" => {}
            _ => {}
        }
        events
    }
}
#[cfg(test)]
mod tests {
    use lattice_conversation::{
        BoundToolCall, MessagePart, MessageRole, MessageSemantic, NativeMessage, ToolCallValidation,
    };

    use super::*;
    use crate::test_reply::Parser;
    use crate::types::{ChatRequest, ModelTarget, ToolDefinition};
    use crate::{LlmEvent, ReplyOutcome, ReplyStop, StreamStop};

    #[test]
    fn unknown_blocks_and_deltas_fail_with_bounded_typed_protocol_errors() {
        for (event_type, payload) in [
            (
                "content_block_start",
                serde_json::json!({"index":9,"content_block":{"type":"redacted_thinking","data":"private-payload"}}),
            ),
            (
                "content_block_start",
                serde_json::json!({"index":9,"content_block":{"type":"unknown-block","data":"private-payload"}}),
            ),
            (
                "content_block_delta",
                serde_json::json!({"index":9,"delta":{"type":"unknown-delta","data":"private-payload"}}),
            ),
            (
                "content_block_delta",
                serde_json::json!({"index":9,"delta":{"type":"x".repeat(4096),"data":"private-payload"}}),
            ),
        ] {
            let mut parser = Parser::new(ProviderOrigin::Anthropic);
            let events = parser.parse_chunk(event_type, &payload.to_string());
            let [
                LlmEvent::Finished {
                    outcome:
                        ReplyOutcome::Interrupted {
                            reason:
                                ReplyStop::Stream(StreamStop::Protocol(
                                    ReplyProtocolError::UnsupportedContentBlock {
                                        protocol,
                                        index,
                                        block_type,
                                    },
                                )),
                        },
                    ..
                },
            ] = &events[..]
            else {
                panic!("typed protocol error: {events:?}");
            };
            assert_eq!(protocol, "anthropic-messages");
            assert_eq!(*index, 9);
            assert!(block_type.chars().count() <= 64);
            assert!(!format!("{events:?}").contains("private-payload"));
            assert!(
                parser
                    .parse_chunk(
                        "message_delta",
                        r#"{"delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":1}}"#
                    )
                    .is_empty()
            );
        }
        assert!(matches!(
            crate::protocol::projection::wire_to_native(
                ProviderOrigin::Anthropic,
                &serde_json::json!({"content":[{"type":"redacted_thinking","data":"private-payload"}]})
            ),
            Err(ProjectionError::Protocol(
                ReplyProtocolError::UnsupportedContentBlock { index: 0, .. }
            ))
        ));
    }

    #[test]
    fn thinking_signature_fragments_share_one_part_and_keep_the_next_signature_separate() {
        let mut parser = Parser::new(ProviderOrigin::Anthropic);
        start_message(&mut parser);
        for index in [0, 1] {
            parser.parse_chunk("content_block_start",&serde_json::json!({"index":index,"content_block":{"type":"thinking","thinking":"r","signature":""}}).to_string());
            for signature in ["first-", "second"] {
                parser.parse_chunk("content_block_delta",&serde_json::json!({"index":index,"delta":{"type":"signature_delta","signature":signature}}).to_string());
            }
            parser.parse_chunk(
                "content_block_stop",
                &serde_json::json!({"index":index}).to_string(),
            );
        }
        let events = parser.parse_chunk(
            "message_delta",
            r#"{"delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":1}}"#,
        );
        let message = crate::test_reply::completed(&events);
        assert_eq!(message.parts.len(), 2);
        for part in &message.parts {
            assert_eq!(part.replay_state.as_ref().unwrap().as_str(), "first-second");
        }
    }
    fn text(role: MessageRole, content: &str) -> lattice_conversation::ConversationItem {
        lattice_conversation::ConversationItem::Message(NativeMessage::<BoundToolCall>::new(
            role,
            None,
            vec![MessagePart::semantic_only(MessageSemantic::Text {
                text: content.into(),
            })],
        ))
    }

    fn start_message(parser: &mut Parser) {
        assert!(
            parser
                .parse_chunk(
                    "message_start",
                    r#"{"message":{"usage":{"input_tokens":1}}}"#,
                )
                .is_empty()
        );
    }

    #[test]
    fn request_envelope_uses_typed_projection() {
        let request = ChatRequest {
            messages: vec![
                text(MessageRole::System, "First prompt."),
                text(MessageRole::System, "Second prompt."),
                text(MessageRole::User, "Hello"),
            ],
            tools: vec![ToolDefinition::new(
                "get_weather",
                "Get weather",
                serde_json::json!({"type": "object"}),
            )],
            target: ModelTarget::new("anthropic", "claude-sonnet-4-6"),
            client_request_id: None,
        };
        let body = build_request_json(&request, "claude-sonnet-4-6", None).expect("projection");
        assert_eq!(body["model"], "claude-sonnet-4-6");
        assert_eq!(body["system"], "First prompt.\n\nSecond prompt.");
        assert_eq!(body["messages"][0]["content"][0]["text"], "Hello");
        assert_eq!(body["tools"][0]["input_schema"]["type"], "object");
        assert_eq!(body["stream"], true);
        assert_eq!(body["max_tokens"], 32_768);
    }

    #[test]
    fn parser_persists_signature_and_real_tool_id() {
        let mut parser = Parser::new(ProviderOrigin::Anthropic);
        start_message(&mut parser);
        parser.parse_chunk(
            "content_block_start",
            r#"{"index":0,"content_block":{"type":"thinking","thinking":"","signature":""}}"#,
        );
        parser.parse_chunk(
            "content_block_delta",
            r#"{"index":0,"delta":{"type":"thinking_delta","thinking":"reason"}}"#,
        );
        parser.parse_chunk(
            "content_block_delta",
            r#"{"index":0,"delta":{"type":"signature_delta","signature":"sig"}}"#,
        );
        parser.parse_chunk(
            "content_block_start",
            r#"{"index":1,"content_block":{"type":"tool_use","id":"toolu_1","name":"observe__workspace","input":{}}}"#,
        );
        parser.parse_chunk(
            "content_block_delta",
            r#"{"index":1,"delta":{"type":"input_json_delta","partial_json":"{}"}}"#,
        );
        parser.parse_chunk("content_block_stop", r#"{"index":0}"#);
        parser.parse_chunk("content_block_stop", r#"{"index":1}"#);
        let events = parser.parse_chunk(
            "message_delta",
            r#"{"delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":2}}"#,
        );
        let message = events
            .iter()
            .find_map(|event| match event {
                LlmEvent::Finished {
                    outcome: ReplyOutcome::Complete { message },
                    ..
                } => Some(message.as_message()),
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
            Some("toolu_1")
        );
    }

    #[test]
    fn parser_preserves_missing_tool_id() {
        let mut parser = Parser::new(ProviderOrigin::Anthropic);
        start_message(&mut parser);
        let started = parser.parse_chunk(
            "content_block_start",
            r#"{"index":0,"content_block":{"type":"tool_use","name":"observe__workspace","input":{}}}"#,
        );
        assert!(started.is_empty());
        assert!(parser.raw.iter().any(|event| matches!(event,
            ProtocolEvent::Part(PartDelta::ToolMetadata { provider_call_id: None, name: Some(StringPatch::Set(name)), .. }) if name == "observe__workspace"
        )));
        parser.parse_chunk("content_block_stop", r#"{"index":0}"#);
        let events = parser.parse_chunk(
            "message_delta",
            r#"{"delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":1}}"#,
        );
        let message = events
            .iter()
            .find_map(|event| match event {
                LlmEvent::Finished {
                    outcome: ReplyOutcome::Complete { message },
                    ..
                } => Some(message.as_message()),
                _ => None,
            })
            .expect("complete message");
        assert_eq!(
            message
                .tool_calls()
                .next()
                .expect("ToolCall")
                .1
                .provider_call_id,
            None
        );
        assert_eq!(
            message.tool_calls().next().expect("ToolCall").1.arguments,
            serde_json::json!({}),
            "no argument delta preserves the provider's legal empty input object",
        );
        assert!(
            parser
                .parse_chunk(
                    "message_delta",
                    r#"{"delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":1}}"#,
                )
                .is_empty()
        );
    }

    #[test]
    fn malformed_argument_delta_rejects_the_whole_message() {
        let mut parser = Parser::new(ProviderOrigin::Anthropic);
        start_message(&mut parser);
        parser.parse_chunk(
            "content_block_start",
            r#"{"index":0,"content_block":{"type":"tool_use","id":"toolu_1","name":"observe__workspace","input":{}}}"#,
        );
        parser.parse_chunk(
            "content_block_delta",
            r#"{"index":0,"delta":{"type":"input_json_delta","partial_json":"{\"path\":\"unterminated"}}"#,
        );
        parser.parse_chunk("content_block_stop", r#"{"index":0}"#);
        let events = parser.parse_chunk(
            "message_delta",
            r#"{"delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":1}}"#,
        );
        assert!(events.iter().any(|event| matches!(
            event,
            LlmEvent::Finished {
                outcome: ReplyOutcome::Rejected {
                    rejection: lattice_conversation::AttemptRejection::InvalidToolCall {
                        validation: ToolCallValidation::MalformedArguments { call_ordinal: 0 },
                    }
                },
                ..
            }
        )));
        assert!(!events.iter().any(|event| matches!(
            event,
            LlmEvent::Finished {
                outcome: ReplyOutcome::Complete { .. },
                ..
            }
        )));

        let mut parser = Parser::new(ProviderOrigin::Anthropic);
        start_message(&mut parser);
        parser.parse_chunk(
            "content_block_start",
            r#"{"index":0,"content_block":{"type":"tool_use","id":"toolu_1","name":"observe__workspace","input":{"path":"already complete"}}}"#,
        );
        let events = parser.parse_chunk(
            "content_block_delta",
            r#"{"index":0,"delta":{"type":"input_json_delta","partial_json":"{}"}}"#,
        );
        assert!(events.iter().any(|event| matches!(
            event,
            LlmEvent::Finished { outcome: ReplyOutcome::Interrupted { reason: ReplyStop::Stream(StreamStop::Provider(failure)) }, .. } if !failure.retryable && failure.message.contains("both at start and in deltas")
        )));
    }
}
