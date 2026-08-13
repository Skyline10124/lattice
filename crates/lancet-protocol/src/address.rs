// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Provider-neutral structured addresses (ADR 0005 §1).
//!
//! Pre-commit conversation addresses are structural rather than random. They
//! identify a model message or one of its ToolCall parts before any commit
//! exists, and they are the address vocabulary shared by conversation binding,
//! InvocationStore reservation tuples, and reconciliation.
//!
//! `PlanKey` has no random `PlanId` and no second aggregate batch digest;
//! exact ordered bindings are the idempotency input.
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

/// Idempotency key of one binding plan: the exact ordered bindings of one
/// model step of one Turn. There is no random `PlanId` (ADR 0005 §1).
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
pub struct PlanKey {
    /// Accepted user Turn the plan belongs to.
    pub turn_id: TurnId,
    /// Zero-based ordinal of the model step inside the Turn.
    pub model_step_ordinal: u32,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::InvocationId;

    fn address_fixture() -> ToolCallAddress {
        ToolCallAddress {
            message: ModelMessageAddress {
                session_id: SessionId::parse("3f8b2c1a7d4e4f5a8b9c0d1e2f3a4b5c")
                    .expect("session id"),
                turn_id: TurnId::parse("3f8b2c1a-7d4e-4f5a-8b9c-0d1e2f3a4b5c").expect("turn id"),
                model_step_ordinal: 2,
            },
            part_ordinal: 1,
        }
    }

    #[test]
    fn tool_call_address_serde_roundtrip() {
        let address = address_fixture();
        let json = serde_json::to_string(&address).expect("serialize");
        let back: ToolCallAddress = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(address, back);
    }

    #[test]
    fn address_uses_typed_identities() {
        let address = address_fixture();
        // The address carries typed values; this assignment does not compile
        // if any field silently degrades to String.
        let session: SessionId = address.message.session_id;
        let turn: TurnId = address.message.turn_id;
        assert_eq!(session.to_string(), "3f8b2c1a7d4e4f5a8b9c0d1e2f3a4b5c");
        assert_eq!(turn.to_string(), "3f8b2c1a-7d4e-4f5a-8b9c-0d1e2f3a4b5c");
        // A TurnId is not an InvocationId; the function below only accepts
        // InvocationId, keeping the two identities unmixable at compile time.
        fn takes_invocation(_: InvocationId) {}
        let _ = takes_invocation;
    }

    #[test]
    fn model_attempt_key_and_plan_key_roundtrip() {
        let turn_id = TurnId::parse("3f8b2c1a-7d4e-4f5a-8b9c-0d1e2f3a4b5c").expect("turn id");
        let attempt = ModelAttemptKey {
            turn_id,
            model_step_ordinal: 0,
            attempt_ordinal: 3,
        };
        let plan = PlanKey {
            turn_id,
            model_step_ordinal: 0,
        };
        for value in [
            serde_json::to_value(attempt).expect("attempt value"),
            serde_json::to_value(plan).expect("plan value"),
        ] {
            assert!(value.get("turn_id").is_some());
        }
        let attempt_back: ModelAttemptKey =
            serde_json::from_value(serde_json::to_value(attempt).expect("attempt value"))
                .expect("attempt deserialize");
        assert_eq!(attempt, attempt_back);
        let plan_back: PlanKey =
            serde_json::from_value(serde_json::to_value(plan).expect("plan value"))
                .expect("plan deserialize");
        assert_eq!(plan, plan_back);
    }

    #[test]
    fn address_schema_is_object_with_typed_fields() {
        let schema = schemars::schema_for!(ToolCallAddress);
        let value = serde_json::to_value(&schema).expect("schema to value");
        assert_eq!(value["type"], "object");
        assert!(value["properties"].get("message").is_some());
        assert!(value["properties"].get("part_ordinal").is_some());
    }
}
