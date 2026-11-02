// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Responses item 列表与逐片段回放状态的唯一消息 codec。

use std::collections::BTreeMap;

use lattice_conversation::{
    MessageSemantic, NativeMessage, Protocol, ReplayOrigin, TokenUsage, ToolCallState,
    UnboundToolCall, canonical_json,
};
use serde_json::{Value, json};

use super::{ProjectionError, ToolResultClosure};
use crate::protocol::responses::{
    Anchor, ContentKind, PartKey, ReplayState, Response, ResponsesSseParser,
};
use crate::reply::{ProtocolEvent, StreamStop};

fn malformed(detail: &str) -> ProjectionError {
    ProjectionError::MalformedWire {
        provider: Protocol::Responses,
        detail: detail.into(),
    }
}

pub(super) fn wire_to_native(
    origin: &ReplayOrigin,
    wire: &Value,
) -> Result<NativeMessage<UnboundToolCall>, ProjectionError> {
    let response: Response =
        serde_json::from_value(wire.clone()).map_err(|_| malformed("invalid response fields"))?;
    let mut events = Vec::new();
    ResponsesSseParser::new()
        .completed(response, &mut events)
        .map_err(projection_error)?;
    let mut reply = super::WireReply::new(origin);
    for event in events {
        if let ProtocolEvent::Part(delta) = event {
            reply.push(delta)?;
        }
    }
    reply.finish()
}

fn projection_error(event: ProtocolEvent) -> ProjectionError {
    match event {
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
        _ => malformed("invalid response item sequence"),
    }
}

#[derive(Default)]
struct Group {
    anchor: Option<Anchor>,
    contents: BTreeMap<PartKey, String>,
    call: Option<Value>,
}

fn function<S: ToolCallState>(call: &S) -> Value {
    let mut item = json!({"type":"function_call", "name":call.name(), "arguments":canonical_json(call.arguments())});
    if let Some(id) = call.provider_call_id() {
        item["call_id"] = id.into();
    }
    item
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
                MessageSemantic::Text { text } => Ok(json!({
                    "type":"message", "role":"assistant",
                    "content":[{"type":"output_text", "text":text, "annotations":[]}],
                })),
                MessageSemantic::ToolCall { call } => Ok(function(call)),
                _ => Err(ProjectionError::MissingReplayState {
                    provider: Protocol::Responses,
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
                provider: Protocol::Responses,
                part_ordinal: ordinal,
            })?;
        let state: ReplayState =
            serde_json::from_str(state.as_str()).map_err(|_| malformed("invalid replay state"))?;
        let group = groups.entry(state.item).or_default();
        if let Some(anchor) = state.anchor
            && group.anchor.replace(anchor).is_some()
        {
            return Err(malformed("duplicate replay anchor"));
        }
        let anchor = group
            .anchor
            .as_ref()
            .ok_or_else(|| malformed("missing replay anchor"))?;
        match (&part.semantic, state.part.kind) {
            (
                MessageSemantic::Text { text },
                ContentKind::OutputText | ContentKind::Refusal | ContentKind::Empty,
            )
            | (
                MessageSemantic::Reasoning { text },
                ContentKind::SummaryText | ContentKind::ReasoningText | ContentKind::Empty,
            ) => {
                if state.part.kind == ContentKind::Empty {
                    let expected_item = match &part.semantic {
                        MessageSemantic::Text { .. } => "message",
                        _ => "reasoning",
                    };
                    if !text.is_empty() || anchor.kind != expected_item {
                        return Err(malformed("invalid boundary part"));
                    }
                }
                group.contents.entry(state.part).or_default().push_str(text);
            }
            (MessageSemantic::ToolCall { call }, ContentKind::FunctionCall) => {
                if group.call.replace(function(call)).is_some() {
                    return Err(malformed("duplicate function part"));
                }
            }
            _ => {
                return Err(ProjectionError::UnsupportedPart {
                    provider: Protocol::Responses,
                    part_ordinal: ordinal,
                });
            }
        }
    }
    let mut output = Vec::with_capacity(groups.len());
    let last_item = groups.last_key_value().map(|(&index, _)| index);
    for (index, group) in groups {
        let anchor = group
            .anchor
            .ok_or_else(|| malformed("missing replay anchor"))?;
        let mut item = match anchor.kind.as_str() {
            "message" => {
                if group.call.is_some() || anchor.encrypted_content.is_some() {
                    return Err(malformed("message replay carries reasoning or call"));
                }
                let mut content = BTreeMap::new();
                for (key, text) in group.contents {
                    let block = match key.kind {
                        ContentKind::OutputText => json!({
                            "type":"output_text", "text":text, "annotations":[], "logprobs":[],
                        }),
                        ContentKind::Refusal => json!({"type":"refusal", "refusal":text}),
                        ContentKind::Empty => continue,
                        _ => return Err(malformed("invalid message replay part")),
                    };
                    if content.insert(key.index, block).is_some() {
                        return Err(malformed("conflicting content index"));
                    }
                }
                let mut item = json!({
                    "type":"message", "role":"assistant", "status":"completed",
                    "content":content.into_values().collect::<Vec<_>>(),
                });
                if let Some(phase) = anchor.phase {
                    item["phase"] = phase.into();
                }
                item
            }
            "reasoning" => {
                if group.call.is_some() || anchor.phase.is_some() {
                    return Err(malformed("invalid reasoning replay metadata"));
                }
                let (mut summary, mut content) = (BTreeMap::new(), BTreeMap::new());
                for (key, text) in group.contents {
                    match key.kind {
                        ContentKind::SummaryText => {
                            summary.insert(key.index, json!({"type":"summary_text", "text":text}));
                        }
                        ContentKind::ReasoningText => {
                            content
                                .insert(key.index, json!({"type":"reasoning_text", "text":text}));
                        }
                        ContentKind::Empty => {}
                        _ => return Err(malformed("invalid reasoning replay part")),
                    }
                }
                if anchor.encrypted_content.is_none() || Some(index) == last_item {
                    continue;
                }
                let mut item = json!({"type":"reasoning", "summary":summary.into_values().collect::<Vec<_>>()});
                if !content.is_empty() {
                    item["content"] = content.into_values().collect();
                }
                if let Some(encrypted) = anchor.encrypted_content {
                    item["encrypted_content"] = encrypted.into();
                }
                item
            }
            "function_call" => {
                if !group.contents.is_empty()
                    || anchor.phase.is_some()
                    || anchor.encrypted_content.is_some()
                {
                    return Err(malformed("invalid function replay metadata"));
                }
                group
                    .call
                    .ok_or_else(|| malformed("missing function replay part"))?
            }
            _ => return Err(malformed("unknown replay item type")),
        };
        if let Some(id) = anchor.id {
            item["id"] = id.into();
        }
        output.push(item);
    }
    Ok(output)
}

pub(super) fn tool_result_messages(
    closures: &[ToolResultClosure],
) -> Result<Vec<Value>, ProjectionError> {
    closures
        .iter()
        .map(|closure| {
            let mut item = json!({"type":"function_call_output", "output":closure.content});
            if let Some(id) = &closure.provider_call_id {
                item["call_id"] = id.clone().into();
            }
            Ok(item)
        })
        .collect()
}

pub(super) fn usage(wire: &Value) -> Result<Option<TokenUsage>, ProjectionError> {
    let mut snapshot = crate::protocol::usage::UsageSnapshot::default();
    snapshot.update(Protocol::Responses, wire)?;
    snapshot.finish(Protocol::Responses)
}
