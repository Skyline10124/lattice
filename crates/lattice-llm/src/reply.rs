// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 一次回复的同步组装：实时投影、完整消息、限额与终态共享同一份状态。

use crate::types::ProviderFailure;
use lattice_conversation::{
    AttemptAssembly, AttemptInterruption, AttemptRejection, MAX_OPAQUE_REPLAY_STATE_BYTES,
    MessagePart, MessageRole, MessageSemantic, NativeMessage, OpaqueProviderState, ProviderOrigin,
    RawToolCall, ToolCallValidation, UnboundToolCall, canonical_json, validate_tool_call_batch,
};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PartKind {
    Text,
    Reasoning,
    ToolCall,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StringPatch {
    Set(String),
    Append(String),
}

impl StringPatch {
    fn len_after(&self, previous: usize) -> usize {
        match self {
            Self::Set(value) => value.len(),
            Self::Append(value) => previous.saturating_add(value.len()),
        }
    }
    fn apply(self, target: &mut String) {
        match self {
            Self::Set(value) => *target = value,
            Self::Append(value) => target.push_str(&value),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PartDelta {
    Begin {
        index: usize,
        kind: PartKind,
    },
    Text {
        index: usize,
        delta: String,
    },
    Reasoning {
        index: usize,
        delta: String,
    },
    ToolMetadata {
        index: usize,
        name: Option<StringPatch>,
        provider_call_id: Option<StringPatch>,
    },
    ToolArguments {
        index: usize,
        delta: String,
    },
    ReplayState {
        index: usize,
        patch: StringPatch,
    },
    End {
        index: usize,
    },
}

impl PartDelta {
    fn index(&self) -> usize {
        match self {
            Self::Begin { index, .. }
            | Self::Text { index, .. }
            | Self::Reasoning { index, .. }
            | Self::ToolMetadata { index, .. }
            | Self::ToolArguments { index, .. }
            | Self::ReplayState { index, .. }
            | Self::End { index } => *index,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum ProtocolEvent {
    Part(PartDelta),
    Usage(lattice_conversation::TokenUsage),
    Done {
        finish_reason: String,
    },
    Failed(StreamStop),
    /// 原始类型名只在协议边界内传递，先脱敏再生成有界的公开错误。
    UnsupportedContentBlock {
        protocol: &'static str,
        index: usize,
        block_type: String,
    },
}

impl ProtocolEvent {
    pub(crate) fn unsupported(protocol: &'static str, index: usize, block_type: &str) -> Self {
        Self::UnsupportedContentBlock {
            protocol,
            index,
            block_type: block_type.into(),
        }
    }

    pub(crate) fn error(message: impl Into<String>, retryable: bool) -> Self {
        Self::Failed(StreamStop::Provider(ProviderFailure {
            message: message.into(),
            retryable,
        }))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ReplyProtocolError {
    #[error("{protocol} content at index {index}: unsupported type {block_type}")]
    UnsupportedContentBlock {
        protocol: String,
        index: usize,
        block_type: String,
    },
    #[error("invalid reply sequence at index {index}: {detail}")]
    InvalidSequence { index: usize, detail: &'static str },
    #[error("replay state at index {index} exceeds {limit} bytes: {actual}")]
    ReplayStateTooLarge {
        index: usize,
        limit: usize,
        actual: usize,
    },
}

impl ReplyProtocolError {
    pub(crate) fn unsupported(protocol: &str, index: usize, block_type: &str) -> Self {
        Self::UnsupportedContentBlock {
            protocol: protocol.into(),
            index,
            block_type: block_type
                .chars()
                .take(64)
                .map(|c| if c.is_control() { '?' } else { c })
                .collect(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamStop {
    Provider(ProviderFailure),
    Cancelled,
    Deadline,
    UnexpectedEof,
    Protocol(ReplyProtocolError),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReplyStop {
    Stream(StreamStop),
    OutputLimit(AttemptInterruption),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LiveDelta {
    Text(String),
    Reasoning(String),
}

/// 完整消息只能由成功结束的累积器构造，不能反序列化或从任意消息转换。
#[derive(Debug, Clone, PartialEq)]
pub struct AccumulatedReply(NativeMessage<UnboundToolCall>);
impl AccumulatedReply {
    pub fn into_message(self) -> NativeMessage<UnboundToolCall> {
        self.0
    }
    pub fn as_message(&self) -> &NativeMessage<UnboundToolCall> {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum ReplyOutcome {
    Complete { message: AccumulatedReply },
    Rejected { rejection: AttemptRejection },
    Interrupted { reason: ReplyStop },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Update {
    Accepted { live: Option<LiveDelta> },
    Ended,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AlreadyEnded;

enum PendingData {
    Text(String),
    Reasoning(String),
    Call(RawToolCall),
}
struct PendingPart {
    index: usize,
    data: PendingData,
    replay: Option<String>,
    closed: bool,
}
enum State {
    Open,
    Ended(ReplyOutcome),
    Taken,
}

pub(crate) struct ReplyAccumulator {
    origin: Option<ProviderOrigin>,
    parts: Vec<PendingPart>,
    positions: BTreeMap<usize, usize>,
    current_text: Option<usize>,
    assembly: AttemptAssembly,
    state: State,
}

fn sequence(index: usize, detail: &'static str) -> ReplyStop {
    ReplyStop::Stream(StreamStop::Protocol(ReplyProtocolError::InvalidSequence {
        index,
        detail,
    }))
}

impl ReplyAccumulator {
    pub(crate) fn new(origin: Option<ProviderOrigin>) -> Self {
        Self {
            origin,
            parts: Vec::new(),
            positions: BTreeMap::new(),
            current_text: None,
            assembly: AttemptAssembly::new(),
            state: State::Open,
        }
    }
    pub(crate) fn push(&mut self, delta: PartDelta) -> Result<Update, AlreadyEnded> {
        self.ensure_open()?;
        match self.apply(delta) {
            Ok(live) => Ok(Update::Accepted { live }),
            Err(reason) => Ok(self.end(ReplyOutcome::Interrupted { reason })),
        }
    }
    fn apply(&mut self, delta: PartDelta) -> Result<Option<LiveDelta>, ReplyStop> {
        let index = delta.index();
        if let PartDelta::Begin { kind, .. } = delta {
            if self.positions.contains_key(&index) {
                return Err(sequence(index, "duplicate begin"));
            }
            let data = match kind {
                PartKind::Text => PendingData::Text(String::new()),
                PartKind::Reasoning => PendingData::Reasoning(String::new()),
                PartKind::ToolCall => PendingData::Call(RawToolCall {
                    provider_call_id: None,
                    name: String::new(),
                    arguments: String::new(),
                }),
            };
            self.current_text = (kind != PartKind::ToolCall).then_some(index);
            self.positions.insert(index, self.parts.len());
            self.parts.push(PendingPart {
                index,
                data,
                replay: None,
                closed: false,
            });
            return Ok(None);
        }
        let slot = *self
            .positions
            .get(&index)
            .ok_or_else(|| sequence(index, "part has not begun"))?;
        let part = &mut self.parts[slot];
        if part.closed {
            return Err(sequence(index, "part already ended"));
        }
        match delta {
            PartDelta::Text { delta, .. } => {
                if self.current_text != Some(index) {
                    return Err(sequence(index, "text cannot fill an earlier position"));
                }
                let PendingData::Text(text) = &mut part.data else {
                    return Err(sequence(index, "text has wrong part kind"));
                };
                self.assembly
                    .push_assistant(&delta)
                    .map_err(ReplyStop::OutputLimit)?;
                text.push_str(&delta);
                Ok((!delta.is_empty()).then_some(LiveDelta::Text(delta)))
            }
            PartDelta::Reasoning { delta, .. } => {
                if self.current_text != Some(index) {
                    return Err(sequence(index, "reasoning cannot fill an earlier position"));
                }
                let PendingData::Reasoning(text) = &mut part.data else {
                    return Err(sequence(index, "reasoning has wrong part kind"));
                };
                self.assembly
                    .push_reasoning(&delta)
                    .map_err(ReplyStop::OutputLimit)?;
                text.push_str(&delta);
                Ok((!delta.is_empty()).then_some(LiveDelta::Reasoning(delta)))
            }
            PartDelta::ToolArguments { delta, .. } => {
                let PendingData::Call(call) = &mut part.data else {
                    return Err(sequence(index, "arguments have wrong part kind"));
                };
                self.assembly
                    .push_tool_arguments(index, &delta)
                    .map_err(ReplyStop::OutputLimit)?;
                call.arguments.push_str(&delta);
                Ok(None)
            }
            PartDelta::ToolMetadata {
                name,
                provider_call_id,
                ..
            } => {
                let PendingData::Call(call) = &mut part.data else {
                    return Err(sequence(index, "metadata has wrong part kind"));
                };
                let old_name = call.name.len();
                let old_id = call.provider_call_id.as_ref().map_or(0, String::len);
                let new_name = name
                    .as_ref()
                    .map_or(old_name, |patch| patch.len_after(old_name));
                let new_id = provider_call_id
                    .as_ref()
                    .map_or(old_id, |patch| patch.len_after(old_id));
                self.assembly
                    .replace_metadata(
                        old_name.saturating_add(old_id),
                        new_name.saturating_add(new_id),
                    )
                    .map_err(ReplyStop::OutputLimit)?;
                if let Some(patch) = name {
                    patch.apply(&mut call.name);
                }
                if let Some(patch) = provider_call_id {
                    patch.apply(call.provider_call_id.get_or_insert_with(String::new));
                }
                Ok(None)
            }
            PartDelta::ReplayState { patch, .. } => {
                let previous = part.replay.as_ref().map_or(0, String::len);
                let next = patch.len_after(previous);
                if next > MAX_OPAQUE_REPLAY_STATE_BYTES {
                    return Err(ReplyStop::Stream(StreamStop::Protocol(
                        ReplyProtocolError::ReplayStateTooLarge {
                            index,
                            limit: MAX_OPAQUE_REPLAY_STATE_BYTES,
                            actual: next,
                        },
                    )));
                }
                self.assembly
                    .replace_metadata(previous, next)
                    .map_err(ReplyStop::OutputLimit)?;
                patch.apply(part.replay.get_or_insert_with(String::new));
                Ok(None)
            }
            PartDelta::End { .. } => {
                part.closed = true;
                if self.current_text == Some(index) {
                    self.current_text = None;
                }
                Ok(None)
            }
            PartDelta::Begin { .. } => unreachable!(),
        }
    }
    pub(crate) fn finish(&mut self) -> Result<Update, AlreadyEnded> {
        self.ensure_open()?;
        let outcome = self.complete();
        Ok(self.end(outcome))
    }
    fn complete(&mut self) -> ReplyOutcome {
        let mut call_ordinal = 0;
        for part in &self.parts {
            if !part.closed {
                return match part.data {
                    PendingData::Call(_) => {
                        rejected(ToolCallValidation::MalformedArguments { call_ordinal })
                    }
                    _ => ReplyOutcome::Interrupted {
                        reason: sequence(part.index, "part did not end"),
                    },
                };
            }
            if matches!(part.data, PendingData::Call(_)) {
                call_ordinal += 1;
            }
        }
        // 移出参数原文后校验整批，避免复制整批参数缓冲。
        let mut raw_calls = Vec::new();
        for part in &mut self.parts {
            if let PendingData::Call(call) = &mut part.data {
                raw_calls.push(std::mem::replace(
                    call,
                    RawToolCall {
                        provider_call_id: None,
                        name: String::new(),
                        arguments: String::new(),
                    },
                ));
            }
        }
        let calls = match validate_tool_call_batch(&raw_calls) {
            Ok(calls) => calls,
            Err(validation) => return rejected(validation),
        };
        for (part, call) in self
            .parts
            .iter()
            .filter(|p| matches!(p.data, PendingData::Call(_)))
            .zip(&calls)
        {
            if let Err(interruption) = self
                .assembly
                .finish_tool_arguments(part.index, canonical_json(&call.arguments).len())
            {
                return ReplyOutcome::Interrupted {
                    reason: ReplyStop::OutputLimit(interruption),
                };
            }
        }
        let mut calls = calls.into_iter();
        let mut ready: Vec<MessagePart<UnboundToolCall>> = Vec::new();
        for part in std::mem::take(&mut self.parts) {
            let semantic = match part.data {
                PendingData::Text(text) => MessageSemantic::Text { text },
                PendingData::Reasoning(text) => MessageSemantic::Reasoning { text },
                PendingData::Call(_) => MessageSemantic::ToolCall {
                    call: calls.next().expect("one validated call per part"),
                },
            };
            let replay_state = part
                .replay
                .map(|value| OpaqueProviderState::new(value).expect("checked before appending"));
            let next = MessagePart {
                semantic,
                replay_state,
            };
            if next.replay_state.is_none() {
                match &next.semantic {
                    MessageSemantic::Text { text } | MessageSemantic::Reasoning { text }
                        if text.is_empty() =>
                    {
                        continue;
                    }
                    _ => {}
                }
                if let Some(previous) = ready.last_mut().filter(|p| p.replay_state.is_none()) {
                    match (&mut previous.semantic, &next.semantic) {
                        (MessageSemantic::Text { text }, MessageSemantic::Text { text: added })
                        | (
                            MessageSemantic::Reasoning { text },
                            MessageSemantic::Reasoning { text: added },
                        ) => {
                            text.push_str(added);
                            continue;
                        }
                        _ => {}
                    }
                }
            }
            ready.push(next);
        }
        ReplyOutcome::Complete {
            message: AccumulatedReply(NativeMessage::new(
                MessageRole::Assistant,
                self.origin,
                ready,
            )),
        }
    }
    pub(crate) fn interrupt(&mut self, reason: StreamStop) -> Result<Update, AlreadyEnded> {
        self.ensure_open()?;
        Ok(self.end(ReplyOutcome::Interrupted {
            reason: ReplyStop::Stream(reason),
        }))
    }
    pub(crate) fn take_outcome(&mut self) -> Option<ReplyOutcome> {
        match std::mem::replace(&mut self.state, State::Taken) {
            State::Ended(outcome) => Some(outcome),
            other => {
                self.state = other;
                None
            }
        }
    }
    fn ensure_open(&self) -> Result<(), AlreadyEnded> {
        if self.is_ended() {
            Err(AlreadyEnded)
        } else {
            Ok(())
        }
    }
    pub(crate) fn is_ended(&self) -> bool {
        !matches!(self.state, State::Open)
    }
    fn end(&mut self, outcome: ReplyOutcome) -> Update {
        self.parts.clear();
        self.positions.clear();
        self.state = State::Ended(outcome);
        Update::Ended
    }
}
fn rejected(validation: ToolCallValidation) -> ReplyOutcome {
    ReplyOutcome::Rejected {
        rejection: AttemptRejection::InvalidToolCall { validation },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lattice_conversation::{MessageSemantic, ProviderOrigin};

    fn call(reply: &mut ReplyAccumulator, index: usize, id: Option<&str>, arguments: &str) {
        reply
            .push(PartDelta::Begin {
                index,
                kind: PartKind::ToolCall,
            })
            .unwrap();
        reply
            .push(PartDelta::ToolMetadata {
                index,
                name: Some(StringPatch::Set("f".into())),
                provider_call_id: id.map(|id| StringPatch::Set(id.into())),
            })
            .unwrap();
        reply
            .push(PartDelta::ToolArguments {
                index,
                delta: arguments.into(),
            })
            .unwrap();
        reply.push(PartDelta::End { index }).unwrap();
    }

    fn limit(reply: &mut ReplyAccumulator) -> (lattice_conversation::LimitKind, usize) {
        let Some(ReplyOutcome::Interrupted {
            reason:
                ReplyStop::OutputLimit(AttemptInterruption::OutputLimitExceeded {
                    kind, actual, ..
                }),
        }) = reply.take_outcome()
        else {
            panic!("typed output limit");
        };
        (kind, actual)
    }

    #[test]
    fn metadata_does_not_reset_sparse_call_argument_budget() {
        use lattice_conversation::{LimitKind, MAX_TOOL_CALL_ARGUMENTS_BYTES};
        let index = usize::MAX;
        let mut reply = ReplyAccumulator::new(None);
        reply
            .push(PartDelta::Begin {
                index,
                kind: PartKind::ToolCall,
            })
            .unwrap();
        let arguments = format!(
            "{{\"x\":\"{}\"}}",
            "x".repeat(MAX_TOOL_CALL_ARGUMENTS_BYTES - 8)
        );
        reply
            .push(PartDelta::ToolArguments {
                index,
                delta: arguments,
            })
            .unwrap();
        for _ in 0..2 {
            reply
                .push(PartDelta::ToolMetadata {
                    index,
                    name: Some(StringPatch::Set("f".into())),
                    provider_call_id: Some(StringPatch::Set("id".into())),
                })
                .unwrap();
        }
        assert_eq!(
            reply.push(PartDelta::ToolArguments {
                index,
                delta: " ".into()
            }),
            Ok(Update::Ended)
        );
        assert_eq!(
            limit(&mut reply),
            (
                LimitKind::ToolCallArguments,
                MAX_TOOL_CALL_ARGUMENTS_BYTES + 1
            )
        );
    }

    #[test]
    fn individually_legal_calls_share_one_total() {
        use lattice_conversation::{
            LimitKind, MAX_MODEL_ATTEMPT_BYTES, MAX_TOOL_CALL_ARGUMENTS_BYTES,
        };
        let arguments = format!(
            "{{\"x\":\"{}\"}}",
            "x".repeat(MAX_TOOL_CALL_ARGUMENTS_BYTES - 8)
        );
        let mut reply = ReplyAccumulator::new(None);
        for index in 0..16 {
            reply
                .push(PartDelta::Begin {
                    index,
                    kind: PartKind::ToolCall,
                })
                .unwrap();
            reply
                .push(PartDelta::ToolMetadata {
                    index,
                    name: Some(StringPatch::Set("f".into())),
                    provider_call_id: None,
                })
                .unwrap();
            let update = reply
                .push(PartDelta::ToolArguments {
                    index,
                    delta: arguments.clone(),
                })
                .unwrap();
            if index == 15 {
                assert_eq!(update, Update::Ended);
                assert_eq!(
                    limit(&mut reply),
                    (LimitKind::ModelAttemptTotal, MAX_MODEL_ATTEMPT_BYTES + 16)
                );
                return;
            }
            assert_eq!(update, Update::Accepted { live: None });
            reply.push(PartDelta::End { index }).unwrap();
        }
        panic!("shared total must stop the sixteenth call");
    }

    #[test]
    fn utf8_equal_and_one_byte_over_limits() {
        use lattice_conversation::{
            LimitKind, MAX_ASSISTANT_CONTENT_BYTES, MAX_REASONING_CONTENT_BYTES,
        };
        for (kind, maximum, limit_kind) in [
            (
                PartKind::Text,
                MAX_ASSISTANT_CONTENT_BYTES,
                LimitKind::AssistantContent,
            ),
            (
                PartKind::Reasoning,
                MAX_REASONING_CONTENT_BYTES,
                LimitKind::ReasoningContent,
            ),
        ] {
            for extra in [0, 1] {
                let mut reply = ReplyAccumulator::new(None);
                reply.push(PartDelta::Begin { index: 0, kind }).unwrap();
                let text = "x".repeat(maximum - 2 + extra);
                let make = |delta| {
                    if kind == PartKind::Text {
                        PartDelta::Text { index: 0, delta }
                    } else {
                        PartDelta::Reasoning { index: 0, delta }
                    }
                };
                reply.push(make(text)).unwrap();
                let update = reply.push(make("é".into())).unwrap();
                if extra == 0 {
                    let expected = if kind == PartKind::Text {
                        LiveDelta::Text("é".into())
                    } else {
                        LiveDelta::Reasoning("é".into())
                    };
                    assert_eq!(
                        update,
                        Update::Accepted {
                            live: Some(expected)
                        }
                    );
                    reply.push(PartDelta::End { index: 0 }).unwrap();
                    reply.finish().unwrap();
                    assert!(matches!(
                        reply.take_outcome(),
                        Some(ReplyOutcome::Complete { .. })
                    ));
                } else {
                    assert_eq!(
                        update,
                        Update::Ended,
                        "crossing fragment produces no live delta"
                    );
                    assert_eq!(limit(&mut reply), (limit_kind, maximum + 1));
                }
            }
        }
    }

    #[test]
    fn thirty_two_calls_complete_and_thirty_three_reject_as_one_batch() {
        for count in [32, 33] {
            let mut reply = ReplyAccumulator::new(None);
            for index in 0..count {
                call(&mut reply, index, Some(&format!("id-{index}")), "{}");
            }
            reply.finish().unwrap();
            match reply.take_outcome().unwrap() {
                ReplyOutcome::Complete { message } if count == 32 => {
                    assert_eq!(message.as_message().tool_calls().count(), 32)
                }
                ReplyOutcome::Rejected {
                    rejection:
                        AttemptRejection::InvalidToolCall {
                            validation:
                                ToolCallValidation::TooManyCalls {
                                    limit: 32,
                                    actual: 33,
                                },
                        },
                } if count == 33 => {}
                other => panic!("unexpected batch outcome: {other:?}"),
            }
        }
    }

    #[test]
    fn one_invalid_call_rejects_every_call() {
        for (arguments, id, expected) in [
            (
                "{",
                Some("second"),
                ToolCallValidation::MalformedArguments { call_ordinal: 1 },
            ),
            (
                "[]",
                Some("second"),
                ToolCallValidation::ArgumentsNotObject { call_ordinal: 1 },
            ),
            (
                "{}",
                Some("first"),
                ToolCallValidation::DuplicateToolCallId {
                    first_ordinal: 0,
                    second_ordinal: 1,
                },
            ),
            (
                "",
                None,
                ToolCallValidation::MalformedArguments { call_ordinal: 1 },
            ),
        ] {
            let mut reply = ReplyAccumulator::new(None);
            call(&mut reply, 9, Some("first"), "{}");
            call(&mut reply, 2, id, arguments);
            reply.finish().unwrap();
            assert_eq!(
                reply.take_outcome(),
                Some(ReplyOutcome::Rejected {
                    rejection: AttemptRejection::InvalidToolCall {
                        validation: expected
                    }
                })
            );
        }
    }

    #[test]
    fn terminal_outcome_can_only_be_taken_once() {
        let mut reply = ReplyAccumulator::new(None);
        reply.finish().unwrap();
        assert_eq!(reply.finish(), Err(AlreadyEnded));
        assert_eq!(reply.interrupt(StreamStop::Cancelled), Err(AlreadyEnded));
        assert_eq!(
            reply.push(PartDelta::Begin {
                index: 0,
                kind: PartKind::Text
            }),
            Err(AlreadyEnded)
        );
        assert!(matches!(
            reply.take_outcome(),
            Some(ReplyOutcome::Complete { .. })
        ));
        assert!(reply.take_outcome().is_none());
        assert_eq!(reply.interrupt(StreamStop::Deadline), Err(AlreadyEnded));
    }

    #[test]
    fn replay_fragments_preserve_signed_and_empty_part_boundaries() {
        let mut reply = ReplyAccumulator::new(Some(ProviderOrigin::Gemini));
        for index in [0, 1] {
            reply
                .push(PartDelta::Begin {
                    index,
                    kind: PartKind::Reasoning,
                })
                .unwrap();
            reply
                .push(PartDelta::Reasoning {
                    index,
                    delta: "r".into(),
                })
                .unwrap();
            reply
                .push(PartDelta::ReplayState {
                    index,
                    patch: StringPatch::Append(format!("sig-{index}")),
                })
                .unwrap();
            reply
                .push(PartDelta::ReplayState {
                    index,
                    patch: StringPatch::Append("-tail".into()),
                })
                .unwrap();
            reply.push(PartDelta::End { index }).unwrap();
        }
        reply
            .push(PartDelta::Begin {
                index: 2,
                kind: PartKind::Text,
            })
            .unwrap();
        reply
            .push(PartDelta::ReplayState {
                index: 2,
                patch: StringPatch::Set("empty-sig".into()),
            })
            .unwrap();
        reply.push(PartDelta::End { index: 2 }).unwrap();
        reply.finish().unwrap();
        let ReplyOutcome::Complete { message } = reply.take_outcome().unwrap() else {
            panic!("complete");
        };
        assert_eq!(message.as_message().parts.len(), 3);
        let signatures: Vec<_> = message
            .as_message()
            .parts
            .iter()
            .map(|part| part.replay_state.as_ref().unwrap().as_str())
            .collect();
        assert_eq!(signatures, ["sig-0-tail", "sig-1-tail", "empty-sig"]);
    }

    #[test]
    fn replay_state_exact_limit_accepts_and_next_byte_interrupts() {
        let mut reply = ReplyAccumulator::new(None);
        reply
            .push(PartDelta::Begin {
                index: 9,
                kind: PartKind::Reasoning,
            })
            .unwrap();
        reply
            .push(PartDelta::ReplayState {
                index: 9,
                patch: StringPatch::Set("s".repeat(MAX_OPAQUE_REPLAY_STATE_BYTES)),
            })
            .unwrap();
        assert_eq!(
            reply.push(PartDelta::ReplayState {
                index: 9,
                patch: StringPatch::Append("!".into())
            }),
            Ok(Update::Ended)
        );
        assert_eq!(
            reply.take_outcome(),
            Some(ReplyOutcome::Interrupted {
                reason: ReplyStop::Stream(StreamStop::Protocol(
                    ReplyProtocolError::ReplayStateTooLarge {
                        index: 9,
                        limit: MAX_OPAQUE_REPLAY_STATE_BYTES,
                        actual: MAX_OPAQUE_REPLAY_STATE_BYTES + 1,
                    }
                ))
            })
        );
    }

    #[test]
    fn total_includes_name_id_and_replay_and_repeated_snapshots_are_idempotent() {
        use lattice_conversation::{LimitKind, MAX_MODEL_ATTEMPT_BYTES};
        let mut reply = ReplyAccumulator::new(None);
        reply
            .push(PartDelta::Begin {
                index: 0,
                kind: PartKind::Text,
            })
            .unwrap();
        reply
            .push(PartDelta::ReplayState {
                index: 0,
                patch: StringPatch::Set("s".into()),
            })
            .unwrap();
        reply.push(PartDelta::End { index: 0 }).unwrap();
        reply
            .push(PartDelta::Begin {
                index: 9,
                kind: PartKind::ToolCall,
            })
            .unwrap();
        reply
            .push(PartDelta::ToolArguments {
                index: 9,
                delta: "{}".into(),
            })
            .unwrap();
        let id = "i".repeat(MAX_MODEL_ATTEMPT_BYTES - 4);
        for _ in 0..2 {
            assert_eq!(
                reply.push(PartDelta::ToolMetadata {
                    index: 9,
                    name: Some(StringPatch::Set("f".into())),
                    provider_call_id: Some(StringPatch::Set(id.clone()))
                }),
                Ok(Update::Accepted { live: None })
            );
        }
        reply
            .push(PartDelta::ToolArguments {
                index: 9,
                delta: " ".into(),
            })
            .unwrap();
        assert_eq!(
            limit(&mut reply),
            (LimitKind::ModelAttemptTotal, MAX_MODEL_ATTEMPT_BYTES + 1)
        );
    }

    #[test]
    fn canonical_arguments_growth_is_added_at_finish() {
        use lattice_conversation::{LimitKind, MAX_MODEL_ATTEMPT_BYTES};
        let raw = r#"{"n":1e1}"#;
        let mut reply = ReplyAccumulator::new(None);
        let id = "i".repeat(MAX_MODEL_ATTEMPT_BYTES - raw.len() - 1);
        call(&mut reply, 9, Some(&id), raw);
        reply.finish().unwrap();
        assert_eq!(
            limit(&mut reply),
            (LimitKind::ModelAttemptTotal, MAX_MODEL_ATTEMPT_BYTES + 1)
        );
    }

    #[test]
    fn text_cannot_fill_an_earlier_position_after_another_part_begins() {
        let mut reply = ReplyAccumulator::new(None);
        reply
            .push(PartDelta::Begin {
                index: 0,
                kind: PartKind::Text,
            })
            .unwrap();
        reply
            .push(PartDelta::Begin {
                index: 9,
                kind: PartKind::ToolCall,
            })
            .unwrap();
        assert_eq!(
            reply.push(PartDelta::Text {
                index: 0,
                delta: "late".into()
            }),
            Ok(Update::Ended)
        );
        assert!(matches!(
            reply.take_outcome(),
            Some(ReplyOutcome::Interrupted {
                reason: ReplyStop::Stream(StreamStop::Protocol(
                    ReplyProtocolError::InvalidSequence { index: 0, .. }
                ))
            })
        ));
    }

    #[test]
    fn interleaved_parts_keep_first_positions_and_match_live_projection() {
        let mut reply = ReplyAccumulator::new(Some(ProviderOrigin::OpenAiCompatible));
        let mut live = Vec::new();
        for delta in [
            PartDelta::Begin {
                index: 0,
                kind: PartKind::Text,
            },
            PartDelta::Text {
                index: 0,
                delta: "A".into(),
            },
            PartDelta::End { index: 0 },
            PartDelta::Begin {
                index: 1,
                kind: PartKind::Reasoning,
            },
            PartDelta::Reasoning {
                index: 1,
                delta: "R".into(),
            },
            PartDelta::End { index: 1 },
            PartDelta::Begin {
                index: 9,
                kind: PartKind::ToolCall,
            },
            PartDelta::ToolArguments {
                index: 9,
                delta: "{\"a\":".into(),
            },
            PartDelta::Begin {
                index: 3,
                kind: PartKind::Text,
            },
            PartDelta::Text {
                index: 3,
                delta: "B".into(),
            },
            PartDelta::End { index: 3 },
            PartDelta::Begin {
                index: 2,
                kind: PartKind::ToolCall,
            },
            PartDelta::ToolMetadata {
                index: 2,
                name: Some(StringPatch::Set("second".into())),
                provider_call_id: None,
            },
            PartDelta::ToolArguments {
                index: 2,
                delta: "{}".into(),
            },
            PartDelta::End { index: 2 },
            PartDelta::ToolMetadata {
                index: 9,
                name: Some(StringPatch::Set("first".into())),
                provider_call_id: Some(StringPatch::Set("real-9".into())),
            },
            PartDelta::ToolArguments {
                index: 9,
                delta: "1}".into(),
            },
            PartDelta::End { index: 9 },
        ] {
            if let Update::Accepted { live: Some(delta) } = reply.push(delta).unwrap() {
                live.push(delta);
            }
        }
        reply.finish().unwrap();
        let ReplyOutcome::Complete { message } = reply.take_outcome().unwrap() else {
            panic!("complete");
        };
        let message = message.into_message();
        let labels: Vec<_> = message
            .parts
            .iter()
            .map(|part| match &part.semantic {
                MessageSemantic::Text { text } => format!("T:{text}"),
                MessageSemantic::Reasoning { text } => format!("R:{text}"),
                MessageSemantic::ToolCall { call } => format!("C:{}", call.name),
            })
            .collect();
        assert_eq!(labels, ["T:A", "R:R", "C:first", "T:B", "C:second"]);
        assert_eq!(
            live,
            [
                LiveDelta::Text("A".into()),
                LiveDelta::Reasoning("R".into()),
                LiveDelta::Text("B".into())
            ]
        );
        let calls: Vec<_> = message.tool_calls().map(|(_, call)| call).collect();
        assert_eq!(calls[0].arguments, serde_json::json!({"a": 1}));
        assert_eq!(calls[0].provider_call_id.as_deref(), Some("real-9"));
        assert_eq!(calls[1].arguments, serde_json::json!({}));
    }
}
