// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 解析器回归使用生产交付逻辑，不自行补造完整消息。

use crate::reply::{ProtocolEvent, StreamStop};
use crate::reply_stream::ReplyDelivery;
use crate::{LlmEvent, ReplyOutcome};
use lattice_conversation::{NativeMessage, Protocol, ReplayOrigin, UnboundToolCall};

pub(crate) struct Parser {
    origin: Protocol,
    chat: crate::protocol::chat_completions::OpenAiSseParser,
    anthropic: crate::protocol::anthropic_messages::AnthropicSseParser,
    generate: crate::protocol::generate_content::GeminiSseParser,
    delivery: ReplyDelivery,
    pub(crate) raw: Vec<ProtocolEvent>,
}
impl Parser {
    pub(crate) fn new(origin: Protocol) -> Self {
        Self {
            origin,
            chat: Default::default(),
            anthropic: Default::default(),
            generate: Default::default(),
            delivery: ReplyDelivery::new(Some(replay_origin(origin))),
            raw: Vec::new(),
        }
    }
    pub(crate) fn parse_chunk<'a>(
        &mut self,
        event_type: impl Into<Option<&'a str>>,
        data: &str,
    ) -> Vec<LlmEvent> {
        let event_type = event_type.into();
        self.raw = match self.origin {
            Protocol::ChatCompletions => self.chat.parse_chunk(event_type, data),
            Protocol::Messages => self.anthropic.parse_chunk(event_type.unwrap_or(""), data),
            Protocol::GenerateContent => self.generate.parse_chunk(event_type, data),
            Protocol::Responses | Protocol::Interactions => {
                unreachable!("test parser requires an implemented protocol")
            }
        };
        self.raw
            .iter()
            .cloned()
            .filter_map(|event| self.delivery.accept(event))
            .collect()
    }
}

pub(crate) fn completed(events: &[LlmEvent]) -> &NativeMessage<UnboundToolCall> {
    let messages: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            LlmEvent::Finished {
                outcome: ReplyOutcome::Complete { message },
                ..
            } => Some(message.as_message()),
            _ => None,
        })
        .collect();
    assert_eq!(messages.len(), 1, "one complete reply: {events:?}");
    messages[0]
}
pub(crate) fn failure(events: &[LlmEvent]) -> Option<String> {
    events.iter().find_map(|event| match event {
        LlmEvent::Finished {
            outcome:
                ReplyOutcome::Interrupted {
                    reason: crate::ReplyStop::Stream(StreamStop::Provider(failure)),
                },
            ..
        } => {
            assert!(!failure.retryable);
            Some(failure.message.clone())
        }
        LlmEvent::Finished {
            outcome:
                ReplyOutcome::Interrupted {
                    reason: crate::ReplyStop::Stream(StreamStop::Protocol(error)),
                },
            ..
        } => Some(error.to_string()),
        _ => None,
    })
}

/// 解析器与完整 wire 夹具共用显式的测试来源。
pub(crate) fn replay_origin(protocol: Protocol) -> ReplayOrigin {
    ReplayOrigin {
        protocol,
        provider_id: "test".into(),
    }
}

pub(crate) fn wire_to_native(
    protocol: Protocol,
    wire: &serde_json::Value,
) -> Result<NativeMessage<UnboundToolCall>, crate::protocol::projection::ProjectionError> {
    crate::protocol::projection::wire_to_native(&replay_origin(protocol), wire)
}

pub(crate) fn bind_replay_step(
    message: lattice_conversation::NativeMessage<lattice_conversation::UnboundToolCall>,
) -> (
    lattice_conversation::NativeMessage<lattice_conversation::BoundToolCall>,
    Vec<lattice_conversation::StoredToolResult>,
) {
    use lattice_conversation::{
        MessagePart, MessageSemantic, ModelMessageAddress, NativeMessage, ResultPayload, SessionId,
        StoredToolResult, SuccessContent, ToolCallAddress, TurnId, canonical_json,
        tool_content_digest,
    };
    let mut parts = Vec::new();
    let mut results = Vec::new();
    let mut index = 0usize;
    for (ordinal, part) in message.parts.into_iter().enumerate() {
        let semantic = match part.semantic {
            MessageSemantic::ToolCall { call } => {
                let invocation_id = lattice_conversation::InvocationId::parse(&format!(
                    "00000000-0000-4000-8000-{index:012x}"
                ))
                .unwrap();
                index += 1;
                let name = call.name.clone();
                let bound = call.bind(
                    invocation_id,
                    ToolCallAddress {
                        message: ModelMessageAddress {
                            session_id: SessionId::parse("3f8b2c1a7d4e4f5a8b9c0d1e2f3a4b5c")
                                .unwrap(),
                            turn_id: TurnId::parse("3f8b2c1a-7d4e-4f5a-8b9c-0d1e2f3a4b5c").unwrap(),
                            model_step_ordinal: 0,
                        },
                        part_ordinal: ordinal as u32,
                    },
                );
                let value = serde_json::json!({"ok": true});
                let bytes = canonical_json(&value);
                results.push(StoredToolResult {
                    invocation_id,
                    name,
                    payload: ResultPayload::Succeeded(SuccessContent::Json {
                        value,
                        digest: tool_content_digest(&bytes),
                        byte_length: bytes.len() as u64,
                    }),
                });
                MessageSemantic::ToolCall { call: bound }
            }
            MessageSemantic::Text { text } => MessageSemantic::Text { text },
            MessageSemantic::Reasoning { text } => MessageSemantic::Reasoning { text },
        };
        parts.push(MessagePart {
            semantic,
            replay_state: part.replay_state,
        });
    }
    (
        NativeMessage::new(message.role, message.provider_origin, parts),
        results,
    )
}

/// 按 JSON Pointer 收集全部断点，连同值比较以检查数量和意外位置。
pub(crate) fn cache_controls(wire: &serde_json::Value) -> Vec<(String, serde_json::Value)> {
    fn collect(wire: &serde_json::Value, path: &str, found: &mut Vec<(String, serde_json::Value)>) {
        match wire {
            serde_json::Value::Object(fields) => {
                for (name, value) in fields {
                    let name_in_path = name.replace('~', "~0").replace('/', "~1");
                    let path = format!("{path}/{name_in_path}");
                    if name == "cache_control" {
                        found.push((path.clone(), value.clone()));
                    }
                    collect(value, &path, found);
                }
            }
            serde_json::Value::Array(values) => {
                for (index, value) in values.iter().enumerate() {
                    collect(value, &format!("{path}/{index}"), found);
                }
            }
            _ => {}
        }
    }
    let mut found = Vec::new();
    collect(wire, "", &mut found);
    found.sort_by(|a, b| a.0.cmp(&b.0));
    found
}
