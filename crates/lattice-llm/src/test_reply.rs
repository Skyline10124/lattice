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
        base_url: "https://provider.example".into(),
    }
}

pub(crate) fn wire_to_native(
    protocol: Protocol,
    wire: &serde_json::Value,
) -> Result<NativeMessage<UnboundToolCall>, crate::protocol::projection::ProjectionError> {
    crate::protocol::projection::wire_to_native(&replay_origin(protocol), wire)
}
