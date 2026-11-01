// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Interactions step 列表与逐片段回放状态的消息 codec。

use std::collections::BTreeMap;

use lattice_conversation::{
    MessageSemantic, NativeMessage, Protocol, ReplayOrigin, TokenUsage, ToolCallState,
    ToolResultOutcome, UnboundToolCall,
};
use serde_json::{Value, json};

use super::{ProjectionError, ToolResultClosure};
use crate::protocol::interactions::{
    Anchor, ContentKind, InteractionsSseParser, PartKey, ReplayState,
};
use crate::reply::{ProtocolEvent, StreamStop};

fn malformed(detail: &str) -> ProjectionError {
    ProjectionError::MalformedWire {
        provider: Protocol::Interactions,
        detail: detail.into(),
    }
}

pub(super) fn wire_to_native(
    origin: &ReplayOrigin,
    wire: &Value,
) -> Result<NativeMessage<UnboundToolCall>, ProjectionError> {
    let events = InteractionsSseParser::full_reply(wire).map_err(|event| match event {
        ProtocolEvent::UnsupportedContentBlock {
            protocol,
            index,
            block_type,
        } => ProjectionError::Protocol(crate::ReplyProtocolError::unsupported(
            protocol,
            index,
            &block_type,
        )),
        ProtocolEvent::Failed(StreamStop::Protocol(error)) => ProjectionError::Protocol(error),
        _ => malformed("invalid interaction step sequence"),
    })?;
    let mut reply = super::WireReply::new(origin);
    for event in events {
        if let ProtocolEvent::Part(delta) = event {
            reply.push(delta)?;
        }
    }
    reply.finish()
}

fn function<S: ToolCallState>(call: &S) -> Value {
    let mut step = json!({"type":"function_call","name":call.name(),"arguments":call.arguments()});
    if let Some(id) = call.provider_call_id() {
        step["id"] = id.into();
    }
    step
}

#[derive(Default)]
struct Group {
    anchor: Option<Anchor>,
    contents: BTreeMap<PartKey, String>,
    call: Option<Value>,
}

pub(super) fn native_to_wire<S: ToolCallState>(
    message: &NativeMessage<S>,
) -> Result<Vec<Value>, ProjectionError> {
    if message.parts.iter().all(|part| part.replay_state.is_none()) {
        return message
            .parts
            .iter()
            .enumerate()
            .map(|(ordinal, part)| match &part.semantic {
                MessageSemantic::Text { text } => {
                    Ok(json!({"type":"model_output","content":[{"type":"text","text":text}]}))
                }
                MessageSemantic::ToolCall { call } => Ok(function(call)),
                _ => Err(ProjectionError::MissingReplayState {
                    provider: Protocol::Interactions,
                    part_ordinal: ordinal,
                }),
            })
            .collect();
    }
    let mut groups = BTreeMap::<usize, Group>::new();
    for (ordinal, part) in message.parts.iter().enumerate() {
        let state = part
            .replay_state
            .as_ref()
            .ok_or(ProjectionError::MissingReplayState {
                provider: Protocol::Interactions,
                part_ordinal: ordinal,
            })?;
        let state: ReplayState =
            serde_json::from_str(state.as_str()).map_err(|_| malformed("invalid replay state"))?;
        let group = groups.entry(state.step).or_default();
        if let Some(anchor) = state.anchor
            && group.anchor.replace(anchor).is_some()
        {
            return Err(malformed("duplicate replay anchor"));
        }
        let anchor = group
            .anchor
            .as_ref()
            .ok_or_else(|| malformed("missing replay anchor"))?;
        match (&part.semantic, state.part.kind, anchor.kind.as_str()) {
            (MessageSemantic::Text { text }, ContentKind::Text, "model_output")
            | (MessageSemantic::Reasoning { text }, ContentKind::Summary, "thought") => {
                group.contents.entry(state.part).or_default().push_str(text);
            }
            (MessageSemantic::Text { text }, ContentKind::Empty, "model_output")
            | (MessageSemantic::Reasoning { text }, ContentKind::Empty, "thought")
                if text.is_empty() && state.part.index == 0 => {}
            (MessageSemantic::ToolCall { call }, ContentKind::FunctionCall, "function_call")
                if state.part.index == 0 =>
            {
                if group.call.replace(function(call)).is_some() {
                    return Err(malformed("duplicate function part"));
                }
            }
            _ => return Err(malformed("replay part does not match step")),
        }
    }
    groups
        .into_values()
        .map(|group| {
            let anchor = group
                .anchor
                .ok_or_else(|| malformed("missing replay anchor"))?;
            if anchor.kind != "thought" && anchor.signature.is_some() {
                return Err(malformed("signature on non-thought step"));
            }
            if anchor.kind == "function_call" {
                if !group.contents.is_empty() {
                    return Err(malformed("content on function step"));
                }
                return group.call.ok_or_else(|| malformed("missing function part"));
            }
            if group.call.is_some() {
                return Err(malformed("function on text step"));
            }
            let mut content = Vec::new();
            for (index, (key, text)) in group.contents.into_iter().enumerate() {
                if key.index != index {
                    return Err(malformed("nonsequential content index"));
                }
                // 子序号由首次接收的完整内容块或 summary delta 分配。
                content.push(json!({"type":"text","text":text}));
            }
            let mut step = json!({"type":anchor.kind});
            if anchor.kind == "model_output" {
                step["content"] = content.into();
            } else if anchor.kind == "thought" {
                if !content.is_empty() {
                    step["summary"] = content.into();
                }
                if let Some(signature) = anchor.signature {
                    step["signature"] = signature.into();
                }
            } else {
                return Err(malformed("unknown replay step type"));
            }
            Ok(step)
        })
        .collect()
}

pub(super) fn tool_result_messages(
    closures: &[ToolResultClosure],
) -> Result<Vec<Value>, ProjectionError> {
    closures.iter().map(|closure| {
        let id = closure.provider_call_id.as_deref().ok_or(ProjectionError::ToolResultWithoutProviderCallId { provider: Protocol::Interactions, invocation_id: closure.invocation_id })?;
        let mut step = json!({"type":"function_result","call_id":id,"name":closure.name,"result":closure.content});
        if closure.outcome == ToolResultOutcome::Failed { step["is_error"] = true.into(); }
        Ok(step)
    }).collect()
}

pub(super) fn usage(wire: &Value) -> Result<Option<TokenUsage>, ProjectionError> {
    let mut snapshot = crate::protocol::usage::UsageSnapshot::default();
    snapshot.update(Protocol::Interactions, wire)?;
    snapshot.finish(Protocol::Interactions)
}
