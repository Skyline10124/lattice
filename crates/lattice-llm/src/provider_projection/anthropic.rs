// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Anthropic messages assistant wire projection.
//!
//! Wire shape (official response body):
//!
//! ```json
//! {"role": "assistant", "content": [
//!     {"type": "text", "text": "…"},
//!     {"type": "thinking", "thinking": "…", "signature": "…"},
//!     {"type": "tool_use", "id": "toolu_…", "name": "…", "input": {…}}
//! ]}
//! ```
//!
//! The thinking block's `signature` is protocol-required replay state: it is
//! preserved verbatim as part-local opaque state and replayed in place for
//! the same provider. It is never fabricated: a thinking part without replay
//! state cannot be written back to Anthropic wire.
//!
//! `signature` belongs only to `thinking` (and `redacted_thinking`) blocks. A
//! `text` part carrying replay state has nowhere to go on this wire, so the
//! writer rejects it rather than emitting a field the reader would silently
//! drop on the way back.
//!
//! Assembly is metered against the ADR 0005 §10 protocol limits; crossing one
//! fails the whole body closed rather than returning a partial message.

use lattice_conversation::{
    AttemptAssembly, MessagePart, MessageRole, MessageSemantic, NativeMessage, OpaqueProviderState,
    ProviderOrigin, RawToolCall, UnboundToolCall, validate_tool_call_batch,
};
use serde_json::{Value, json};

use super::ProjectionError;

fn limit_exceeded(interruption: lattice_conversation::AttemptInterruption) -> ProjectionError {
    ProjectionError::OutputLimitExceeded {
        provider: ProviderOrigin::Anthropic,
        interruption,
    }
}

fn malformed(detail: impl Into<String>) -> ProjectionError {
    ProjectionError::MalformedWire {
        provider: ProviderOrigin::Anthropic,
        detail: detail.into(),
    }
}

pub fn wire_to_native(wire: &Value) -> Result<NativeMessage<UnboundToolCall>, ProjectionError> {
    let blocks = wire
        .get("content")
        .and_then(Value::as_array)
        .ok_or_else(|| malformed("missing content blocks"))?;

    // Two-phase assembly: parse blocks in wire order, validating ToolCalls as
    // one batch, then splice validated calls back at their wire positions.
    enum Pending {
        Ready(MessagePart<UnboundToolCall>),
        Call(usize),
    }

    let mut pending: Vec<Pending> = Vec::new();
    let mut raw_calls: Vec<RawToolCall> = Vec::new();
    let mut assembly = AttemptAssembly::new();
    for block in blocks {
        let block_type = block
            .get("type")
            .and_then(Value::as_str)
            .ok_or_else(|| malformed("content block missing type"))?;
        match block_type {
            "text" => {
                let text = block
                    .get("text")
                    .and_then(Value::as_str)
                    .ok_or_else(|| malformed("text block missing text"))?;
                assembly.push_assistant(text).map_err(limit_exceeded)?;
                pending.push(Pending::Ready(MessagePart::semantic_only(
                    MessageSemantic::Text {
                        text: text.to_string(),
                    },
                )));
            }
            "thinking" => {
                let thinking = block
                    .get("thinking")
                    .and_then(Value::as_str)
                    .ok_or_else(|| malformed("thinking block missing thinking"))?;
                assembly.push_reasoning(thinking).map_err(limit_exceeded)?;
                let replay_state = match block.get("signature") {
                    Some(Value::String(signature)) => {
                        Some(OpaqueProviderState::new(signature.clone()).map_err(|_| {
                            malformed("thinking signature exceeds replay-state bound")
                        })?)
                    }
                    Some(Value::Null) | None => None,
                    _ => return Err(malformed("thinking signature is not a string")),
                };
                pending.push(Pending::Ready(MessagePart {
                    semantic: MessageSemantic::Reasoning {
                        text: thinking.to_string(),
                    },
                    replay_state,
                }));
            }
            "tool_use" => {
                let name = block
                    .get("name")
                    .and_then(Value::as_str)
                    .ok_or_else(|| malformed("tool_use block missing name"))?;
                let input = block
                    .get("input")
                    .ok_or_else(|| malformed("tool_use block missing input"))?;
                let provider_call_id = match block.get("id") {
                    Some(Value::String(id)) => Some(id.to_string()),
                    Some(Value::Null) | None => None,
                    _ => return Err(malformed("tool_use id is not a string")),
                };
                let arguments = serde_json::to_string(input)
                    .map_err(|_| malformed("tool_use input is not serializable"))?;
                assembly.begin_tool_call();
                assembly
                    .push_tool_arguments(&arguments)
                    .map_err(limit_exceeded)?;
                raw_calls.push(RawToolCall {
                    provider_call_id,
                    name: name.to_string(),
                    arguments,
                });
                pending.push(Pending::Call(raw_calls.len() - 1));
            }
            other => {
                return Err(malformed(format!(
                    "unsupported content block type {other:?}"
                )));
            }
        }
    }

    let validated = validate_tool_call_batch(&raw_calls)?;
    let mut parts: Vec<MessagePart<UnboundToolCall>> = Vec::with_capacity(pending.len());
    for slot in pending {
        match slot {
            Pending::Ready(part) => parts.push(part),
            Pending::Call(index) => {
                parts.push(MessagePart::semantic_only(MessageSemantic::ToolCall {
                    call: validated[index].clone(),
                }))
            }
        }
    }

    Ok(NativeMessage::new(
        MessageRole::Assistant,
        Some(ProviderOrigin::Anthropic),
        parts,
    ))
}

pub fn native_to_wire(message: &NativeMessage<UnboundToolCall>) -> Result<Value, ProjectionError> {
    let mut blocks: Vec<Value> = Vec::new();
    for (ordinal, part) in message.parts.iter().enumerate() {
        match &part.semantic {
            MessageSemantic::Text { text } => {
                // `signature` exists only on thinking blocks. Emitting it on a
                // text block would produce a field the reader drops, silently
                // losing the state on round-trip.
                if part.replay_state.is_some() {
                    return Err(ProjectionError::UnsupportedPart {
                        provider: ProviderOrigin::Anthropic,
                        part_ordinal: ordinal,
                    });
                }
                blocks.push(json!({ "type": "text", "text": text }));
            }
            MessageSemantic::Reasoning { text } => {
                let state =
                    part.replay_state
                        .as_ref()
                        .ok_or(ProjectionError::MissingReplayState {
                            provider: ProviderOrigin::Anthropic,
                            part_ordinal: ordinal,
                        })?;
                blocks.push(json!({
                    "type": "thinking",
                    "thinking": text,
                    "signature": state.as_str(),
                }));
            }
            MessageSemantic::ToolCall { call } => {
                let mut block = json!({
                    "type": "tool_use",
                    "name": call.name,
                    "input": call.arguments,
                });
                if let Some(id) = &call.provider_call_id {
                    block["id"] = json!(id);
                }
                blocks.push(block);
            }
            MessageSemantic::ToolResult { .. } => {
                return Err(ProjectionError::UnsupportedPart {
                    provider: ProviderOrigin::Anthropic,
                    part_ordinal: ordinal,
                });
            }
        }
    }
    Ok(json!({
        "role": "assistant",
        "content": blocks,
    }))
}
