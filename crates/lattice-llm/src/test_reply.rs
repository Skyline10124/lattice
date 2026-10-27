// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 解析器回归使用生产交付逻辑，不自行补造完整消息。

use crate::reply::{ProtocolEvent, StreamStop};
use crate::reply_stream::ReplyDelivery;
use crate::{LlmEvent, ReplyOutcome};
use lattice_conversation::{NativeMessage, ProviderOrigin, UnboundToolCall};

pub(crate) struct Parser {
    origin: ProviderOrigin,
    chat: crate::protocol::chat_completions::OpenAiSseParser,
    anthropic: crate::protocol::anthropic_messages::AnthropicSseParser,
    generate: crate::protocol::generate_content::GeminiSseParser,
    delivery: ReplyDelivery,
    pub(crate) raw: Vec<ProtocolEvent>,
}
impl Parser {
    pub(crate) fn new(origin: ProviderOrigin) -> Self {
        Self {
            origin,
            chat: Default::default(),
            anthropic: Default::default(),
            generate: Default::default(),
            delivery: ReplyDelivery::new(Some(origin)),
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
            ProviderOrigin::OpenAiCompatible => self.chat.parse_chunk(event_type, data),
            ProviderOrigin::Anthropic => self.anthropic.parse_chunk(event_type.unwrap_or(""), data),
            ProviderOrigin::Gemini => self.generate.parse_chunk(event_type, data),
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
