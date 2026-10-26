// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Provider replay/projection seam (GitHub #142, grill §5; #139-B Commit 2).
//!
//! Pure data functions mapping official provider wire JSON to and from the
//! provider-neutral [`NativeMessage`] contract. This module owns the
//! provider-boundary rules:
//!
//! 1. validate and preserve provider-native assistant message state
//!    (provider call IDs, signatures, thought state) as part-local opaque
//!    replay state;
//! 2. replay it exactly for the same provider — the persisted closure
//!    vocabulary is `NativeMessage<BoundToolCall>`, so replay and projection
//!    are generic over the [`ToolCallState`] typestate;
//! 3. project it to plain provider-neutral semantics across providers;
//! 4. restore the original native state when switching back;
//! 5. encode conversation ToolResult closures into the next request's wire
//!    shape, joining each closure to its closing bound ToolCall, and extract
//!    provider-reported usage from official response bodies.
//!
//! It never generates, encodes, or rewrites a provider Tool Call ID, and
//! never fabricates target-provider identity. A missing provider call ID is
//! a valid conversation fact: when the target provider cannot encode a
//! recorded closure without one, the projection fails with a typed
//! [`ProjectionError::ToolResultWithoutProviderCallId`] rather than
//! synthesizing an ID. The production adapters in [`crate::protocol`] own
//! HTTP/SSE envelopes but delegate every request-message and completed-response
//! projection to this seam.

mod anthropic_messages;
mod chat_completions;
mod generate_content;

pub(crate) use chat_completions::{OpenAiPart, parts_to_native as openai_parts_to_native};

use std::collections::BTreeSet;

use lattice_conversation::{
    AttemptInterruption, BoundToolCall, InvocationId, MessageRole, MessageSemantic, NativeMessage,
    ProviderOrigin, TokenUsage, ToolCallState, ToolCallValidation, ToolResultOutcome,
    UnboundToolCall,
};
use lattice_conversation::{
    ConversationItem, MAX_TOOL_CONTENT_BYTES, ResultPayload, StoredToolResult, SuccessContent,
    canonical_json, tool_content_digest,
};
use serde_json::Value;
use thiserror::Error;

/// Why a projection failed. Every failure is fail-closed: no partial
/// message, no fabricated identity, no guessed replay state.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ProjectionError {
    #[error("invalid structured step: {detail}")]
    InvalidStep { detail: &'static str },
    /// The wire JSON does not match the provider's official shape.
    #[error("malformed {provider} wire: {detail}")]
    MalformedWire {
        /// The provider being projected.
        provider: ProviderOrigin,
        /// What was missing or mistyped.
        detail: String,
    },
    /// A same-provider replay was requested for a message from another (or
    /// no) origin.
    #[error("cannot replay {expected:?} wire from a message with origin {found:?}")]
    WrongOrigin {
        /// The provider the wire belongs to.
        expected: ProviderOrigin,
        /// The message's recorded origin.
        found: Option<ProviderOrigin>,
    },
    /// A part requires replay state for same-provider replay (e.g. an
    /// Anthropic thinking block's signature) but carries none. The state is
    /// never fabricated.
    #[error("{provider} part ordinal {part_ordinal} is missing required replay state")]
    MissingReplayState {
        /// The provider being projected.
        provider: ProviderOrigin,
        /// Ordinal of the part in the message.
        part_ordinal: usize,
    },
    /// A part kind the provider's assistant wire cannot express.
    #[error("{provider} wire cannot express part ordinal {part_ordinal}")]
    UnsupportedPart {
        /// The provider being projected.
        provider: ProviderOrigin,
        /// Ordinal of the part in the message.
        part_ordinal: usize,
    },
    /// A ToolCall on the wire failed strict validation; the whole message is
    /// invalid (ADR 0005 §7).
    #[error("invalid tool call: {0}")]
    InvalidToolCall(#[from] ToolCallValidation),
    /// Assembling the wire body crossed a protocol hard limit (ADR 0005 §10).
    /// The message is rejected whole; no partial conversation is returned.
    #[error("{provider} wire exceeds a protocol limit: {interruption:?}")]
    OutputLimitExceeded {
        /// The provider being projected.
        provider: ProviderOrigin,
        /// The typed interruption recording which limit was crossed.
        interruption: AttemptInterruption,
    },
    /// A ToolResult closure names an invocation the assistant message never
    /// bound.
    #[error("{provider} tool result closes unknown invocation {invocation_id}")]
    UnmatchedToolResult {
        /// The provider being projected.
        provider: ProviderOrigin,
        /// The invocation the closure claimed to close.
        invocation_id: InvocationId,
    },
    /// The target provider's wire cannot encode a recorded closure without a
    /// provider call ID, and the closing ToolCall legitimately carries none
    /// (ADR 0005 §1: provider IDs are optional facts, never synthesized).
    #[error(
        "{provider} wire cannot encode the tool result for {invocation_id} without a provider call id"
    )]
    ToolResultWithoutProviderCallId {
        /// The provider being projected.
        provider: ProviderOrigin,
        /// The invocation whose closure cannot be encoded.
        invocation_id: InvocationId,
    },
    /// The usage block on an otherwise accepted body is malformed.
    #[error("malformed {provider} usage block: {detail}")]
    MalformedUsage {
        /// The provider being projected.
        provider: ProviderOrigin,
        /// What was missing or mistyped.
        detail: String,
    },
}

/// Extract a provider-neutral unbound message from an official wire
/// response body, preserving part order and part-local replay state.
pub fn wire_to_native(
    origin: ProviderOrigin,
    wire: &Value,
) -> Result<NativeMessage<UnboundToolCall>, ProjectionError> {
    match origin {
        ProviderOrigin::OpenAiCompatible => chat_completions::wire_to_native(wire),
        ProviderOrigin::Anthropic => anthropic_messages::wire_to_native(wire),
        ProviderOrigin::Gemini => generate_content::wire_to_native(wire),
    }
}

/// Replay a native message back to its origin provider's assistant wire
/// message object. Requires `message.provider_origin == Some(origin)` and
/// every protocol-required replay state to be present.
///
/// Generic over the binding typestate: unbound pre-binding messages and the
/// persisted `NativeMessage<BoundToolCall>` closure vocabulary replay
/// through the same rules.
pub fn native_to_wire<S: ToolCallState>(
    origin: ProviderOrigin,
    message: &NativeMessage<S>,
) -> Result<Value, ProjectionError> {
    if message.provider_origin != Some(origin) {
        return Err(ProjectionError::WrongOrigin {
            expected: origin,
            found: message.provider_origin,
        });
    }
    match origin {
        ProviderOrigin::OpenAiCompatible => chat_completions::native_to_wire(message),
        ProviderOrigin::Anthropic => anthropic_messages::native_to_wire(message),
        ProviderOrigin::Gemini => generate_content::native_to_wire(message),
    }
}

/// Project a message to another provider's neutral semantics.
///
/// Only neutral semantics cross. Three classes of source state are dropped:
///
/// - every part-local opaque replay state (signatures, thought state);
/// - every provider call ID;
/// - every [`MessageSemantic::Reasoning`] part.
///
/// Reasoning is dropped because ADR 0005 §7 fixes that "cross-provider
/// projection does not replay source reasoning": a provider's chain of thought
/// is its own protocol state and must never be written into a different
/// provider's reasoning channel. Keeping the reasoning part would survive into
/// [`native_to_wire`] and appear verbatim in the target request body.
///
/// Remaining parts keep their relative order. Switching back to the source
/// provider must use the retained original message, not this projection.
pub fn project_cross_provider<S: ToolCallState>(
    message: &NativeMessage<S>,
    target: ProviderOrigin,
) -> NativeMessage<S> {
    let mut projected = message.clone();
    projected.provider_origin = Some(target);
    projected
        .parts
        .retain(|part| !matches!(part.semantic, MessageSemantic::Reasoning { .. }));
    for part in &mut projected.parts {
        part.replay_state = None;
        if let MessageSemantic::ToolCall { call } = &mut part.semantic {
            call.clear_provider_call_id();
        }
    }
    projected
}

/// Provider request messages projected from durable bound conversation history.
#[derive(Debug, Clone, PartialEq)]
pub struct RequestMessages {
    /// Provider system-instruction text, merged in source order.
    pub system: Option<String>,
    /// Provider-native non-system message objects in conversation order.
    pub messages: Vec<Value>,
}

/// Project durable bound history into one provider's request-message vocabulary.
///
/// This is the only production ToolCall/ToolResult request encoder. Same-provider
/// assistant messages replay their retained native state exactly. A provider
/// switch applies [`project_cross_provider`] once; no caller may flatten the
/// message through a second DTO or invent a missing provider call ID.
pub fn request_messages_to_wire(
    origin: ProviderOrigin,
    history: &[ConversationItem],
) -> Result<RequestMessages, ProjectionError> {
    let mut system = None;
    let mut messages = Vec::new();
    for item in history {
        match item {
            ConversationItem::Message(message) => match message.role {
                MessageRole::System => push_system_text(&mut system, &plain_text(origin, message)?),
                MessageRole::User => {
                    messages.push(user_to_wire(origin, &plain_text(origin, message)?))
                }
                _ => {
                    return Err(ProjectionError::InvalidStep {
                        detail: "standalone message must be system or user",
                    });
                }
            },
            ConversationItem::Step(step) => {
                let projected = if step.assistant.provider_origin == Some(origin) {
                    step.assistant.clone()
                } else {
                    project_cross_provider(&step.assistant, origin)
                };
                // 先核完整配对，再生成 wire；不从相邻历史修补结果。
                let results = tool_results_to_wire(origin, &projected, &step.results)?;
                messages.push(native_to_wire(origin, &projected)?);
                messages.extend(results);
            }
        }
    }
    Ok(RequestMessages { system, messages })
}

fn plain_text(
    origin: ProviderOrigin,
    message: &NativeMessage<BoundToolCall>,
) -> Result<String, ProjectionError> {
    let mut text = String::new();
    for (ordinal, part) in message.parts.iter().enumerate() {
        if part.replay_state.is_some() {
            return Err(ProjectionError::UnsupportedPart {
                provider: origin,
                part_ordinal: ordinal,
            });
        }
        match &part.semantic {
            MessageSemantic::Text { text: delta } => text.push_str(delta),
            _ => {
                return Err(ProjectionError::UnsupportedPart {
                    provider: origin,
                    part_ordinal: ordinal,
                });
            }
        }
    }
    Ok(text)
}

fn push_system_text(system: &mut Option<String>, text: &str) {
    match system {
        Some(existing) => {
            existing.push_str("\n\n");
            existing.push_str(text);
        }
        None => *system = Some(text.to_string()),
    }
}

fn user_to_wire(origin: ProviderOrigin, text: &str) -> Value {
    match origin {
        ProviderOrigin::OpenAiCompatible => serde_json::json!({
            "role": "user",
            "content": text,
        }),
        ProviderOrigin::Anthropic => serde_json::json!({
            "role": "user",
            "content": [{
                "type": "text",
                "text": if text.is_empty() { "(empty message)" } else { text },
            }],
        }),
        ProviderOrigin::Gemini => serde_json::json!({
            "role": "user",
            "parts": [{"text": text}],
        }),
    }
}

/// 已核验配对后的 provider 输入，仅存在于本次投影。
struct ToolResultClosure {
    invocation_id: InvocationId,
    provider_call_id: Option<String>,
    name: String,
    outcome: ToolResultOutcome,
    content: String,
}

fn closure_content(payload: &ResultPayload) -> String {
    use serde_json::json;
    let value = match payload {
        ResultPayload::Succeeded(SuccessContent::Json { value, .. }) => {
            return canonical_json(value);
        }
        ResultPayload::Succeeded(SuccessContent::OutputTooLarge {
            digest,
            byte_length,
        }) => json!({
            "outcome": "succeeded", "content_unavailable": "output_too_large",
            "digest": digest.iter().map(|b| format!("{b:02x}")).collect::<String>(),
            "byte_length": byte_length,
        }),
        ResultPayload::Failed(error) => {
            const BOUNDED_ERROR_CONTENT_BYTES: usize = 4_096;
            let mut end = error.message.len().min(BOUNDED_ERROR_CONTENT_BYTES);
            while !error.message.is_char_boundary(end) {
                end -= 1;
            }
            json!({"error":{"code":error.code.as_i32(), "message":&error.message[..end]}})
        }
        ResultPayload::NotExecuted => json!({"outcome":"not_executed"}),
        ResultPayload::Indeterminate(cause) => json!({"outcome":"indeterminate","cause":cause}),
    };
    canonical_json(&value)
}

/// 按助手调用顺序核验精确结果配对；provider ID 只来自同一个调用。
pub fn tool_results_to_wire(
    origin: ProviderOrigin,
    assistant: &NativeMessage<BoundToolCall>,
    results: &[StoredToolResult],
) -> Result<Vec<Value>, ProjectionError> {
    let invalid = |detail| ProjectionError::InvalidStep { detail };
    if assistant.role != MessageRole::Assistant {
        return Err(invalid(
            "step requires an assistant without embedded results",
        ));
    }
    let calls: Vec<_> = assistant.tool_calls().collect();
    if calls.len() != results.len() {
        return Err(invalid("result count mismatch"));
    }
    let mut ids = BTreeSet::new();
    let mut closures = Vec::with_capacity(results.len());
    for ((_, call), result) in calls.into_iter().zip(results) {
        if call.invocation_id != result.invocation_id
            || call.name != result.name
            || !ids.insert(result.invocation_id)
        {
            return Err(invalid("result identity, name or order mismatch"));
        }
        match &result.payload {
            ResultPayload::Succeeded(SuccessContent::Json {
                value,
                digest,
                byte_length,
            }) => {
                let bytes = canonical_json(value);
                if bytes.len() > MAX_TOOL_CONTENT_BYTES
                    || bytes.len() as u64 != *byte_length
                    || tool_content_digest(&bytes) != *digest
                {
                    return Err(invalid("success content identity mismatch"));
                }
            }
            ResultPayload::Succeeded(SuccessContent::OutputTooLarge { byte_length, .. })
                if *byte_length <= MAX_TOOL_CONTENT_BYTES as u64 =>
            {
                return Err(invalid("invalid oversized result length"));
            }
            _ => {}
        }
        closures.push(ToolResultClosure {
            invocation_id: result.invocation_id,
            provider_call_id: call.provider_call_id.clone(),
            name: result.name.clone(),
            outcome: result.payload.outcome(),
            content: closure_content(&result.payload),
        });
    }
    if closures.is_empty() {
        return Ok(Vec::new());
    }
    match origin {
        ProviderOrigin::OpenAiCompatible => chat_completions::tool_result_messages(&closures),
        ProviderOrigin::Anthropic => anthropic_messages::tool_result_messages(&closures),
        ProviderOrigin::Gemini => generate_content::tool_result_messages(&closures),
    }
}

/// Extract the provider-reported usage from an official response body.
///
/// `Ok(None)` when the body carries no usage block: missing usage is unknown,
/// never zero. A present but malformed block fails closed.
pub fn wire_usage(
    origin: ProviderOrigin,
    wire: &Value,
) -> Result<Option<TokenUsage>, ProjectionError> {
    match origin {
        ProviderOrigin::OpenAiCompatible => chat_completions::usage(wire),
        ProviderOrigin::Anthropic => anthropic_messages::usage(wire),
        ProviderOrigin::Gemini => generate_content::usage(wire),
    }
}
