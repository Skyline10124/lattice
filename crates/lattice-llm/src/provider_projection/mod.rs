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
//! synthesizing an ID. No HTTP, no streaming, no runtime: the production
//! codecs in [`crate::providers`] stay untouched and adopt this seam when
//! the production path switches (#139-B Commit 4).

mod anthropic;
mod gemini;
mod openai;

use std::collections::BTreeMap;

use lattice_conversation::{
    AttemptInterruption, BoundToolCall, InvocationId, MessageSemantic, NativeMessage,
    ProviderOrigin, TokenUsage, ToolCallState, ToolCallValidation, ToolResultOutcome,
    UnboundToolCall,
};
use serde_json::Value;
use thiserror::Error;

/// Why a projection failed. Every failure is fail-closed: no partial
/// message, no fabricated identity, no guessed replay state.
#[derive(Debug, Clone, PartialEq, Error)]
pub enum ProjectionError {
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
        ProviderOrigin::OpenAiCompatible => openai::wire_to_native(wire),
        ProviderOrigin::Anthropic => anthropic::wire_to_native(wire),
        ProviderOrigin::Gemini => gemini::wire_to_native(wire),
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
        ProviderOrigin::OpenAiCompatible => openai::native_to_wire(message),
        ProviderOrigin::Anthropic => anthropic::native_to_wire(message),
        ProviderOrigin::Gemini => gemini::native_to_wire(message),
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

/// One conversation ToolResult closure joined to its closing bound ToolCall.
struct ToolResultClosure {
    invocation_id: InvocationId,
    provider_call_id: Option<String>,
    name: String,
    outcome: ToolResultOutcome,
    content: Option<String>,
}

/// The model-facing wire content of one closure.
///
/// Available material is passed through verbatim. A closure whose material is
/// unavailable carries the true outcome spelled in a self-describing JSON
/// object instead — the outcome stays honest without inventing Tool output.
fn closure_content(outcome: ToolResultOutcome, content: &Option<String>) -> String {
    match content {
        Some(text) => text.clone(),
        None => format!("{{\"outcome\":\"{}\"}}", outcome.as_str()),
    }
}

/// Encode one conversation ToolResult closure message into the origin
/// provider's next-request wire shape (ADR 0005 §7, #139-B Commit 2).
///
/// `assistant` is the bound Tool-bearing message the closures belong to; each
/// closure is joined to its closing ToolCall by canonical `InvocationId`, and
/// the provider call ID required by the OpenAI/Anthropic wire is taken from
/// that call — never synthesized. Providers with no ID-bearing result shape
/// (Gemini `functionResponse`) encode closures whose calls carry no provider
/// ID.
///
/// Returns the request messages to append, in closure order: one message per
/// closure for the OpenAI wire, one merged user turn for the Anthropic and
/// Gemini wires.
pub fn tool_results_to_wire(
    origin: ProviderOrigin,
    assistant: &NativeMessage<BoundToolCall>,
    tool: &NativeMessage<BoundToolCall>,
) -> Result<Vec<Value>, ProjectionError> {
    let mut closing_ids: BTreeMap<InvocationId, Option<String>> = BTreeMap::new();
    for (_, call) in assistant.tool_calls() {
        closing_ids.insert(call.invocation_id, call.provider_call_id.clone());
    }

    let mut closures = Vec::new();
    for (ordinal, part) in tool.parts.iter().enumerate() {
        let MessageSemantic::ToolResult {
            invocation_id,
            name,
            outcome,
            content,
        } = &part.semantic
        else {
            return Err(ProjectionError::MalformedWire {
                provider: origin,
                detail: format!("tool message part ordinal {ordinal} is not a ToolResult closure"),
            });
        };
        // No provider result shape has a slot for part-local replay state.
        if part.replay_state.is_some() {
            return Err(ProjectionError::UnsupportedPart {
                provider: origin,
                part_ordinal: ordinal,
            });
        }
        let Some(provider_call_id) = closing_ids.get(invocation_id).cloned() else {
            return Err(ProjectionError::UnmatchedToolResult {
                provider: origin,
                invocation_id: *invocation_id,
            });
        };
        closures.push(ToolResultClosure {
            invocation_id: *invocation_id,
            provider_call_id,
            name: name.clone(),
            outcome: *outcome,
            content: content.clone(),
        });
    }

    match origin {
        ProviderOrigin::OpenAiCompatible => openai::tool_result_messages(&closures),
        ProviderOrigin::Anthropic => anthropic::tool_result_messages(&closures),
        ProviderOrigin::Gemini => gemini::tool_result_messages(&closures),
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
        ProviderOrigin::OpenAiCompatible => openai::usage(wire),
        ProviderOrigin::Anthropic => anthropic::usage(wire),
        ProviderOrigin::Gemini => gemini::usage(wire),
    }
}
