// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Gemini generateContent assistant wire projection.
//!
//! Wire shape (official response body):
//!
//! ```json
//! {"candidates": [{"content": {"role": "model", "parts": [
//!     {"text": "…", "thought": true, "thoughtSignature": "…"},
//!     {"text": "…", "thoughtSignature": "…"},
//!     {"functionCall": {"name": "…", "args": {…}, "id": "…"?},
//!      "thoughtSignature": "…"}
//! ]}}]}
//! ```
//!
//! `thoughtSignature` may sit on any part — including an otherwise-empty
//! text part — and is preserved verbatim in place as part-local opaque
//! replay state. A real `functionCall.id` is preserved when present; when
//! absent the call stays `None` and no `tc_*`-style synthetic ID is ever
//! generated.

use lattice_conversation::{
    MessagePart, MessageRole, MessageSemantic, NativeMessage, OpaqueProviderState, ProviderOrigin,
    RawToolCall, UnboundToolCall, validate_tool_call_batch,
};
use serde_json::{Value, json};

use super::ProjectionError;

fn malformed(detail: impl Into<String>) -> ProjectionError {
    ProjectionError::MalformedWire {
        provider: ProviderOrigin::Gemini,
        detail: detail.into(),
    }
}

fn replay_state_of(part: &Value) -> Result<Option<OpaqueProviderState>, ProjectionError> {
    match part.get("thoughtSignature") {
        Some(Value::String(signature)) => Ok(Some(
            OpaqueProviderState::new(signature.clone())
                .map_err(|_| malformed("thoughtSignature exceeds replay-state bound"))?,
        )),
        Some(Value::Null) | None => Ok(None),
        _ => Err(malformed("thoughtSignature is not a string")),
    }
}

pub fn wire_to_native(wire: &Value) -> Result<NativeMessage<UnboundToolCall>, ProjectionError> {
    let wire_parts = wire
        .get("candidates")
        .and_then(Value::as_array)
        .and_then(|candidates| candidates.first())
        .and_then(|candidate| candidate.get("content"))
        .and_then(|content| content.get("parts"))
        .and_then(Value::as_array)
        .ok_or_else(|| malformed("missing candidates[0].content.parts"))?;

    // Two-phase assembly, same as the Anthropic projector: parse in wire
    // order, validate ToolCalls as one batch, splice back at wire positions.
    enum Pending {
        Ready(MessagePart<UnboundToolCall>),
        Call(usize),
        /// A call part carrying its own thoughtSignature.
        SignedCall(usize, OpaqueProviderState),
    }

    let mut pending: Vec<Pending> = Vec::new();
    let mut raw_calls: Vec<RawToolCall> = Vec::new();
    for part in wire_parts {
        let replay_state = replay_state_of(part)?;
        let thought = part.get("thought").and_then(Value::as_bool) == Some(true);

        if let Some(function_call) = part.get("functionCall") {
            let name = function_call
                .get("name")
                .and_then(Value::as_str)
                .ok_or_else(|| malformed("functionCall missing name"))?;
            let arguments = match function_call.get("args") {
                Some(args) => serde_json::to_string(args)
                    .map_err(|_| malformed("functionCall args not serializable"))?,
                // An absent `args` object is not an empty-object spelling;
                // there is no empty-to-`{}` normalization.
                None => return Err(malformed("functionCall missing args")),
            };
            let provider_call_id = match function_call.get("id") {
                Some(Value::String(id)) => Some(id.to_string()),
                Some(Value::Null) | None => None,
                _ => return Err(malformed("functionCall id is not a string")),
            };
            raw_calls.push(RawToolCall {
                provider_call_id,
                name: name.to_string(),
                arguments,
            });
            // A signature on a call part stays attached to that call part.
            let index = raw_calls.len() - 1;
            match replay_state {
                Some(state) => pending.push(Pending::SignedCall(index, state)),
                None => pending.push(Pending::Call(index)),
            }
            continue;
        }

        match part.get("text") {
            Some(Value::String(text)) => {
                let semantic = if thought {
                    MessageSemantic::Reasoning { text: text.clone() }
                } else {
                    MessageSemantic::Text { text: text.clone() }
                };
                // An empty text part with a signature is preserved, not
                // filtered (grill R7).
                pending.push(Pending::Ready(MessagePart {
                    semantic,
                    replay_state,
                }));
            }
            Some(_) => return Err(malformed("text part is not a string")),
            None => return Err(malformed("part has neither text nor functionCall")),
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
            Pending::SignedCall(index, state) => parts.push(MessagePart {
                semantic: MessageSemantic::ToolCall {
                    call: validated[index].clone(),
                },
                replay_state: Some(state),
            }),
        }
    }

    Ok(NativeMessage::new(
        MessageRole::Assistant,
        Some(ProviderOrigin::Gemini),
        parts,
    ))
}

pub fn native_to_wire(message: &NativeMessage<UnboundToolCall>) -> Result<Value, ProjectionError> {
    let mut wire_parts: Vec<Value> = Vec::new();
    for (ordinal, part) in message.parts.iter().enumerate() {
        let thought_signature = part.replay_state.as_ref().map(|state| state.as_str());
        match &part.semantic {
            MessageSemantic::Text { text } => {
                let mut wire_part = json!({ "text": text });
                if let Some(signature) = thought_signature {
                    wire_part["thoughtSignature"] = json!(signature);
                }
                wire_parts.push(wire_part);
            }
            MessageSemantic::Reasoning { text } => {
                let mut wire_part = json!({ "text": text, "thought": true });
                if let Some(signature) = thought_signature {
                    wire_part["thoughtSignature"] = json!(signature);
                }
                wire_parts.push(wire_part);
            }
            MessageSemantic::ToolCall { call } => {
                let mut function_call = json!({
                    "name": call.name,
                    "args": call.arguments,
                });
                if let Some(id) = &call.provider_call_id {
                    function_call["id"] = json!(id);
                }
                let mut wire_part = json!({ "functionCall": function_call });
                if let Some(signature) = thought_signature {
                    wire_part["thoughtSignature"] = json!(signature);
                }
                wire_parts.push(wire_part);
            }
            MessageSemantic::ToolResult { .. } => {
                return Err(ProjectionError::UnsupportedPart {
                    provider: ProviderOrigin::Gemini,
                    part_ordinal: ordinal,
                });
            }
        }
    }
    Ok(json!({
        "role": "model",
        "parts": wire_parts,
    }))
}
