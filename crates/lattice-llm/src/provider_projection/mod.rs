// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Provider replay/projection seam (GitHub #142, grill §5).
//!
//! Pure data functions mapping official provider wire JSON to and from the
//! provider-neutral [`NativeMessage`] contract. This module owns the
//! provider-boundary rules:
//!
//! 1. validate and preserve provider-native assistant message state
//!    (provider call IDs, signatures, thought state) as part-local opaque
//!    replay state;
//! 2. replay it exactly for the same provider;
//! 3. project it to plain provider-neutral semantics across providers;
//! 4. restore the original native state when switching back.
//!
//! It never generates, encodes, or rewrites a provider Tool Call ID, and
//! never fabricates target-provider identity. No HTTP, no streaming, no
//! runtime: the production codecs in [`crate::providers`] stay untouched and
//! adopt this seam when the production path switches (#139-B).

mod anthropic;
mod gemini;
mod openai;

use lattice_conversation::{NativeMessage, ProviderOrigin, ToolCallValidation, UnboundToolCall};
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
pub fn native_to_wire(
    origin: ProviderOrigin,
    message: &NativeMessage<UnboundToolCall>,
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
/// Only semantics cross: every part-local replay state is dropped and no
/// provider call ID survives, so no source opaque bytes leak into the target
/// provider. Switching back to the source provider must use the retained
/// original message, not this projection.
pub fn project_cross_provider(
    message: &NativeMessage<UnboundToolCall>,
    target: ProviderOrigin,
) -> NativeMessage<UnboundToolCall> {
    let mut projected = message.clone();
    projected.provider_origin = Some(target);
    for part in &mut projected.parts {
        part.replay_state = None;
        if let lattice_conversation::MessageSemantic::ToolCall { call } = &mut part.semantic {
            call.provider_call_id = None;
        }
    }
    projected
}
