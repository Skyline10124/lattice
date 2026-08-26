// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Provider-neutral, data-only conversation contract (ADR 0005 §7).
//!
//! This crate owns:
//!
//! - ordered [`NativeMessage`] / [`MessagePart`] values with the
//!   [`UnboundToolCall`] / [`BoundToolCall`] typestates;
//! - the provider-neutral Tool result closure ([`ToolResultOutcome`]);
//! - semantic and replay digests ([`digest`]);
//! - per-attempt usage and interruption/rejection records ([`attempt`]);
//! - provider origin and bounded part-local opaque replay state;
//! - the typed durable branch transcript ([`HeadTranscript`]).
//!
//! It depends only on the data-only `lancet-protocol` and neutral data
//! dependencies. It owns no filesystem, store, lock, runtime, provider HTTP
//! client, or ID generator.

pub mod attempt;
pub mod digest;
pub mod graph_types;
pub mod history;
pub mod limits;
pub mod message;
pub mod outcome;
pub mod tool_call;
pub mod usage;

pub use lancet_protocol::canonical_json;
// The conversation contract is phrased over the canonical identity and
// structural-address types (`BoundToolCall::invocation_id`,
// `BoundToolCall::address`), so consumers of the crate can name them without
// reaching for `lancet-protocol` themselves.
pub use lancet_protocol::{
    InvocationId, ModelMessageAddress, SessionId, ToolCallAddress, TurnId, arguments_digest,
};

pub use attempt::{AttemptInterruption, AttemptOutcome, AttemptRejection, ModelAttemptRecord};
pub use digest::{replay_state_digest, semantic_digest};
pub use graph_types::HeadTranscript;
pub use history::ConversationHistory;
pub use limits::{
    AttemptAssembly, LimitKind, MAX_ASSISTANT_CONTENT_BYTES, MAX_MODEL_ATTEMPT_BYTES,
    MAX_REASONING_CONTENT_BYTES, MAX_TOOL_CALL_ARGUMENTS_BYTES, MAX_TOOL_CALLS_PER_BATCH,
};
pub use message::{
    MAX_OPAQUE_REPLAY_STATE_BYTES, MessagePart, MessageRole, MessageSemantic, NativeMessage,
    OpaqueProviderState, OpaqueReplayStateError, ProviderOrigin, ToolCallState,
};
pub use outcome::ToolResultOutcome;
pub use tool_call::{
    ArgumentsDigestMismatch, BoundToolCall, RawToolCall, ToolCallValidation, UnboundToolCall,
    validate_tool_call_batch,
};
pub use usage::TokenUsage;
