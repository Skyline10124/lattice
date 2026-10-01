// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Provider-neutral structured addresses (ADR 0005 §1).
//!
//! Pre-commit conversation addresses are structural rather than random. They
//! identify a model message or one of its ToolCall parts before any commit
//! exists, and they are the address vocabulary shared by conversation binding
//! and the Session store's invocation slots.
//!
//! This crate is pure data: no I/O, no runtime, no tokio.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::identity::{SessionId, TurnId};

/// Structural address of one provider-produced model message inside a Turn.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
pub struct ModelMessageAddress {
    /// Session that owns the conversation.
    pub session_id: SessionId,
    /// Accepted user Turn the message belongs to.
    pub turn_id: TurnId,
    /// Zero-based ordinal of the model step inside the Turn.
    pub model_step_ordinal: u32,
}

/// Structural address of one ToolCall part inside a model message.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
pub struct ToolCallAddress {
    /// Address of the containing model message.
    pub message: ModelMessageAddress,
    /// Zero-based ordinal of the ToolCall part inside the message's ordered
    /// part vector.
    pub part_ordinal: u32,
}

/// Identity of one provider attempt of one model step.
///
/// A retry after a pre-stream failure starts a new `attempt_ordinal` without
/// consuming another model step (ADR 0005 §10).
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
pub struct ModelAttemptKey {
    /// Accepted user Turn the attempt belongs to.
    pub turn_id: TurnId,
    /// Zero-based ordinal of the model step inside the Turn.
    pub model_step_ordinal: u32,
    /// Zero-based ordinal of the provider attempt inside the step.
    pub attempt_ordinal: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_attempt_key_roundtrip() {
        let attempt = ModelAttemptKey {
            turn_id: TurnId::parse("3f8b2c1a-7d4e-4f5a-8b9c-0d1e2f3a4b5c").expect("turn id"),
            model_step_ordinal: 0,
            attempt_ordinal: 3,
        };
        let value = serde_json::to_value(attempt).expect("attempt value");
        assert!(value.get("turn_id").is_some());
        let attempt_back: ModelAttemptKey =
            serde_json::from_value(value).expect("attempt deserialize");
        assert_eq!(attempt, attempt_back);
    }
}
