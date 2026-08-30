// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Durable PendingTurn journal contract (ADR 0005 §9, GitHub #144).
//!
//! `refs/lattice/sessions/<SessionId>/active-turn` points directly at the
//! current immutable PendingTurn snapshot. There is no second
//! `pending/<TurnId>` ref and no active-Turn field in `SessionState`: ref
//! existence and target are the only authority for "a Turn is pending", and
//! every snapshot points at its predecessor so stage progression is one
//! validated CAS chain.
//!
//! This module owns the pure data contract and the pure reconciliation
//! decision function:
//!
//! - [`contract`]: the versioned [`PendingSnapshot`] envelope, the thirteen
//!   durable [`PendingStage`] states, and pure stage-transition validation;
//! - [`config`]: the frozen request-acceptance facts published by
//!   `TurnStarted`;
//! - [`binding`]: the exact ordered binding plan published atomically as
//!   `BindingPlanned` (there is no Tool-bearing `MessageComplete` gap);
//! - [`draft`]: the cross-store `ResultDraft` material record and its
//!   `Durable | CleanAbsent | Uncertain` publication classification;
//! - [`step`]: the ordered `StepClosing` cursor snapshots and the closed
//!   `StepClosed` sum;
//! - [`outcome`]: the typed `TurnOutcome` / `ClosureOrigin` vocabulary and
//!   the derived per-Turn usage aggregate;
//! - [`facts`]: the synthetic Invocation facts snapshot reconciliation runs
//!   against (the production InvocationStore arrives with #139-A/#139-B);
//! - [`reconcile`]: `reconcile(PendingSnapshot, InvocationFactsSnapshot)`
//!   producing one [`ReconciliationDecision`]: either the closure-only
//!   [`ReconciliationPlan`] or a [`CandidateRevisionRequest`] that must be
//!   satisfied before any closure exists.
//!
//! Persistence lives in [`crate::git_graph`]; nothing in this module performs
//! I/O. `Committed` is an immutable final journal object constructed with the
//! pinned TurnCommit candidate and is never published through `active-turn`:
//! the one final SessionStore ref transaction makes the TurnCommit/
//! `turns/<TurnId>` pin and its referenced `Committed` object reachable while
//! deleting `active-turn`.

pub mod binding;
pub mod config;
pub mod contract;
pub mod draft;
pub mod facts;
pub mod outcome;
pub mod reconcile;
pub mod step;

pub use binding::{BindingPlan, PlannedBinding, binding_digest};
pub use config::{
    FrozenTurnConfig, MAX_CATALOG_SNAPSHOT_BYTES, MAX_MODEL_ATTEMPTS, MAX_MODEL_STEPS,
    MAX_USER_INPUT_BYTES, ToolCatalogEntry, ToolCatalogSnapshot, TurnExecutionLimits,
    TurnStartedFacts,
};
pub use contract::{
    CommittedFacts, PENDING_SNAPSHOT_SCHEMA_VERSION, PendingSnapshot, PendingStage,
    TurnClosingFacts, TurnCommitCandidate, TurnCommitCandidateBasis, validate_transition,
};
pub use draft::{
    DraftClassification, DraftPublication, MAX_TOOL_CONTENT_BYTES, ResultDraft, ToolContentError,
};
pub use facts::{
    ExecutionOutcome, IndeterminateReason, InvocationFactsSnapshot, InvocationLifecycleFact,
    InvocationRecordFact, NotExecutedReason, PlanFacts, PlanValidation, ResultMaterial,
    UnavailableMaterialReason,
};
pub use outcome::{
    ClosureOrigin, LiveControllingCause, ToolBatchControl, TurnCancellation, TurnFailure,
    TurnFinishReason, TurnInterruption, TurnOutcome, TurnUsageAggregate,
    WorkspaceHistoryCheckpoint, derive_tool_batch_control,
};
pub use reconcile::{
    CandidateRevisionRequest, ReconciliationAction, ReconciliationDecision, ReconciliationPlan,
    reconcile,
};
pub use step::{
    ClosedBinding, MaterialAvailability, StepClosedKind, StepClosingState, step_conversation_digest,
};

/// SHA-256 hex serde for fixed 32-byte digests in journal payloads. Same
/// convention as `lattice-conversation`'s digest serde: 64 lowercase hex
/// characters, strictly parsed.
pub(crate) mod hex_digest {
    use serde::{Deserialize, Deserializer, Serializer, de::Error as _};

    pub fn serialize<S: Serializer>(digest: &[u8; 32], serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&super::encode_hex(digest))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<[u8; 32], D::Error> {
        let raw = String::deserialize(deserializer)?;
        super::decode_hex(&raw)
            .ok_or_else(|| D::Error::custom("digest must be 64 lowercase hex characters"))
    }
}

/// `Option<[u8; 32]>` serde: `null` or 64 lowercase hex characters.
pub(crate) mod hex_digest_option {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(
        digest: &Option<[u8; 32]>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        match digest {
            Some(digest) => super::hex_digest::serialize(digest, serializer),
            None => serializer.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<[u8; 32]>, D::Error> {
        match Option::<String>::deserialize(deserializer)? {
            None => Ok(None),
            Some(raw) => super::decode_hex(&raw).map(Some).ok_or_else(|| {
                serde::de::Error::custom("digest must be 64 lowercase hex characters")
            }),
        }
    }
}

pub(crate) fn encode_hex(digest: &[u8; 32]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(64);
    for byte in digest.iter() {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

pub(crate) fn decode_hex(raw: &str) -> Option<[u8; 32]> {
    let bytes = raw.as_bytes();
    if bytes.len() != 64 {
        return None;
    }
    let hex_value = |byte: u8| match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    };
    let mut digest = [0u8; 32];
    for (index, pair) in bytes.chunks_exact(2).enumerate() {
        digest[index] = (hex_value(pair[0])? << 4) | hex_value(pair[1])?;
    }
    Some(digest)
}
/// Pure builders for in-module tests (no store, no I/O).
#[cfg(test)]
pub(crate) mod tests_support {
    use super::TurnOutcome;
    use super::config::{
        FrozenTurnConfig, ToolCatalogEntry, ToolCatalogSnapshot, TurnExecutionLimits,
        TurnStartedFacts,
    };
    use super::contract::TurnCommitCandidate;
    use super::outcome::{ClosureOrigin, TurnFinishReason};
    use lancet_protocol::{SessionId, TurnId};
    use lattice_conversation::{MessagePart, MessageRole, MessageSemantic, NativeMessage};

    pub(crate) fn turn_started_facts() -> TurnStartedFacts {
        TurnStartedFacts {
            turn_id: TurnId::parse("3f8b2c1a-7d4e-4f5a-8b9c-0d1e2f3a4b5c").unwrap(),
            session_id: SessionId::parse("3f8b2c1a7d4e4f5a8b9c0d1e2f3a4b5c").unwrap(),
            project_store_key: crate::ProjectStoreKey::from_canonical_path_bytes(b"/test/project"),
            parent_conversation_tip: None,
            user_message: NativeMessage::new(
                MessageRole::User,
                None,
                vec![MessagePart::semantic_only(MessageSemantic::Text {
                    text: "please continue".to_string(),
                })],
            ),
            config: FrozenTurnConfig {
                agent_name: "coder".to_string(),
                model: "test-model".to_string(),
                provider: "test-provider".to_string(),
                system_prompt: None,
                profile_snapshot_json: "{\"model\":\"test-model\"}".to_string(),
                policy_snapshot_json: "{}".to_string(),
                catalog: ToolCatalogSnapshot::new(
                    "test-build".to_string(),
                    vec![ToolCatalogEntry {
                        name: "observe.workspace".to_string(),
                        descriptor_identity: "d".repeat(64),
                        description: "observe".to_string(),
                        input_schema: serde_json::json!({"type": "object"}),
                    }],
                )
                .unwrap(),
                limits: TurnExecutionLimits {
                    timeout_secs: 60,
                    max_model_steps: super::MAX_MODEL_STEPS,
                    max_model_attempts: 2,
                },
            },
            accepted_at: "2026-08-16T12:00:00Z".to_string(),
        }
    }

    pub(crate) fn completed_candidate() -> TurnCommitCandidate {
        TurnCommitCandidate {
            session_id: "3f8b2c1a7d4e4f5a8b9c0d1e2f3a4b5c".to_string(),
            turn_id: TurnId::parse("3f8b2c1a-7d4e-4f5a-8b9c-0d1e2f3a4b5c").unwrap(),
            side_parent_ids: Vec::new(),
            messages: vec![crate::test_helpers::user_msg("please continue")],
            agent_name: "coder".to_string(),
            profile_snapshot_hash: "test-profile".to_string(),
            profile_snapshot_json: "{}".to_string(),
            model: "test-model".to_string(),
            provider: "test-provider".to_string(),
            outcome: TurnOutcome::Completed {
                finish_reason: TurnFinishReason::Stop,
            },
            origin: ClosureOrigin::Live,
            commit_message: "please continue".to_string(),
            usage_aggregate: None,
            workspace_history_checkpoint: crate::pending::WorkspaceHistoryCheckpoint::Empty,
            created_at: "2026-08-16T12:00:00Z".to_string(),
        }
    }
}
