//! CEP policy decision seam types.
//!
//! Defines the public contract for policy decisions: `PolicyInput`,
//! `PolicyDecision`, `PolicyHook`, and supporting types. This module
//! is the stable, small public API that external policy engines (future
//! Cedar/OPA/Rego) will implement against, and that the default in-process
//! hook satisfies.
//!
//! **Design constraints:**
//! - Public types carry only `method`, `params` (opaque JSON), envelope
//!   snapshot, and audit context.
//! - No executor-owned normalized paths, argv facts, host facts, handler
//!   AST, or budget tracker internals.
//! - `PolicyHook::decide` is synchronous and deterministic — no async,
//!   no I/O, no time-dependence, no mutable state access.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::envelope::Capability;

/// Source provenance for a capability in the effective envelope snapshot.
///
/// The base envelope is the turn-level observation floor; a declared envelope
/// is the task-level authority extension.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CapabilitySource {
    /// Capability comes from the base (turn-level) envelope only.
    Base,
    /// Capability comes from a declared (task-level) envelope only.
    Declared { envelope_id: String },
    /// Capability is present in both base and declared envelopes.
    Both { envelope_id: String },
}

/// A capability with its source provenance.
///
/// Carried inside `EffectiveEnvelopeSnapshot` so policy decisions can
/// distinguish base-sourced from declared-sourced capabilities.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SourcedCapability {
    /// The capability granted by the envelope.
    pub capability: Capability,
    /// Where this capability originated.
    pub source: CapabilitySource,
}

/// Layered snapshot of the effective envelope for policy evaluation.
///
/// The base envelope is the turn-level observation floor; a declared envelope
/// is the task-level authority extension. When no declared envelope exists, the
/// effective snapshot is the base envelope only.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct EffectiveEnvelopeSnapshot {
    /// Identifier of the base (turn-level) envelope.
    pub base_envelope_id: String,
    /// Identifier of the declared (task-level) envelope, if active.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub declared_envelope_id: Option<String>,
    /// All capabilities in the effective envelope, each tagged with source.
    pub capabilities: Vec<SourcedCapability>,
}

/// Immutable budget snapshot supplied before policy evaluation.
///
/// This is a stable snapshot — not a mutable budget tracker. The executor
/// fills it once before calling the policy hook; the hook cannot mutate
/// counters or read live budget state.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PolicyBudgetSnapshot {
    /// Observation bytes consumed in the current turn.
    pub turn_observation_bytes_used: u64,
    /// Observation byte limit for the current turn.
    pub turn_observation_bytes_limit: u64,
    /// `todo_write` operations consumed in the current turn.
    pub todo_ops_used: u64,
    /// `todo_write` operation limit for the current turn.
    pub todo_ops_limit: u64,
}

/// Audit context carried through the policy seam for traceability.
///
/// Links a policy decision back to the turn, session, and specific call
/// that triggered it. Budget information enters only through
/// `PolicyBudgetSnapshot`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PolicyAuditContext {
    /// Turn identifier.
    pub turn_id: String,
    /// Session identifier.
    pub session_id: String,
    /// Call sequence number within the turn, if available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call_id: Option<u64>,
    /// Immutable budget snapshot for policy evaluation.
    pub budget_snapshot: PolicyBudgetSnapshot,
}

/// The public policy input contract.
///
/// Carries `method`, method `params` as opaque JSON, an envelope snapshot,
/// and audit context. Must NOT expose executor-owned normalized paths,
/// argv facts, host facts, handler AST, or budget tracker internals.
/// Budget information enters only through `PolicyBudgetSnapshot`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PolicyInput {
    /// Full dotted CEP method name (e.g. `observe.workspace`).
    pub method: String,
    /// Method parameters as opaque JSON — policy hook sees the shape
    /// but the executor owns any normalized/derived facts.
    pub params: serde_json::Value,
    /// Layered effective envelope snapshot with capability source provenance.
    pub envelope_snapshot: EffectiveEnvelopeSnapshot,
    /// Audit context for traceability.
    pub audit_context: PolicyAuditContext,
}

/// Stable, coarse-grained reason codes for policy decisions.
///
/// These are protocol-stable and intended for future external policy engines
/// to reproduce. Closed enum with `deny_unknown_fields`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PolicyReasonCode {
    /// No matching capability in the effective envelope.
    MissingCapability,
    /// A matching capability exists but its source is not allowed for this method.
    CapabilitySourceNotAllowed,
    /// Path target is outside the allowed scope pattern.
    PathOutsideScope,
    /// Command execution denied by policy.
    CommandDenied,
    /// Network request denied by policy.
    NetworkDenied,
    /// Budget limit exceeded.
    BudgetExceeded,
    /// An active envelope is required but none exists.
    EnvelopeRequired,
    /// Current envelope is insufficient; replan with wider authority.
    ReplanRequired,
    /// Policy configuration is invalid or unparseable.
    PolicyConfigInvalid,
    /// Cannot determine the result from available facts.
    NonDeterminable,
}

/// Stable action requirements returned with policy denials.
///
/// Tells the agent (or its orchestrator) what corrective action to take.
/// Closed enum with `deny_unknown_fields`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PolicyActionRequired {
    /// Declare an envelope with the required capabilities.
    DeclareEnvelope,
    /// Widen the current envelope's capabilities or scope.
    WidenEnvelope,
    /// Reduce the requested scope to fit within current authority.
    ReduceScope,
    /// Change the command to one allowed by policy.
    ChangeCommand,
    /// Change the URL to one allowed by policy.
    ChangeUrl,
    /// Reduce the read size to fit within budget.
    ReduceReadSize,
    /// Ask a human to make the decision.
    AskHuman,
}

/// The public policy decision verdict.
///
/// A stable summary of the policy evaluation. Must NOT contain
/// `CapabilityMatch`, normalized paths, argv, host, or budget facts —
/// those belong in executor-owned `PolicyEvidence` and audit metadata.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PolicyDecision {
    /// The requested operation is allowed.
    Allowed,
    /// The requested operation is denied.
    Denied {
        /// Why the operation was denied.
        reason_code: PolicyReasonCode,
        /// Human-readable explanation.
        message: String,
        /// What corrective action to take, if known.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        action_required: Option<PolicyActionRequired>,
    },
    /// The decision cannot be made automatically; a human must review.
    HumanReview {
        /// Why human review is needed.
        reason_code: PolicyReasonCode,
        /// Human-readable explanation.
        message: String,
    },
}

/// Policy hook trait for pluggable policy implementations.
///
/// Must be synchronous and deterministic — no I/O, no time-dependence,
/// no mutable state access. This is the future integration point for
/// Cedar/OPA/Rego external policy engines; this crate ships the seam and the
/// default in-process hook lives in `lancet-authority`.
pub trait PolicyHook: Send + Sync {
    /// Evaluate the policy for the given input and return a decision.
    fn decide(&self, input: &PolicyInput) -> PolicyDecision;
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ── PolicyDecision round-trip tests ──────────────────────────────

    #[test]
    fn policy_decision_allowed_round_trips() {
        let decision = PolicyDecision::Allowed;
        let serialized = serde_json::to_string(&decision).unwrap();
        let deserialized: PolicyDecision = serde_json::from_str(&serialized).unwrap();
        assert_eq!(deserialized, decision);
        assert_eq!(serialized, "\"allowed\"");
    }

    #[test]
    fn policy_decision_denied_round_trips() {
        let decision = PolicyDecision::Denied {
            reason_code: PolicyReasonCode::MissingCapability,
            message: "no WritePath capability".into(),
            action_required: Some(PolicyActionRequired::DeclareEnvelope),
        };
        let serialized = serde_json::to_string(&decision).unwrap();
        let deserialized: PolicyDecision = serde_json::from_str(&serialized).unwrap();
        assert_eq!(deserialized, decision);
    }

    #[test]
    fn policy_decision_denied_without_action_round_trips() {
        let decision = PolicyDecision::Denied {
            reason_code: PolicyReasonCode::BudgetExceeded,
            message: "observation budget exhausted".into(),
            action_required: None,
        };
        let serialized = serde_json::to_string(&decision).unwrap();
        let deserialized: PolicyDecision = serde_json::from_str(&serialized).unwrap();
        assert_eq!(deserialized, decision);
    }

    #[test]
    fn policy_decision_human_review_round_trips() {
        let decision = PolicyDecision::HumanReview {
            reason_code: PolicyReasonCode::NonDeterminable,
            message: "ambiguous path pattern".into(),
        };
        let serialized = serde_json::to_string(&decision).unwrap();
        let deserialized: PolicyDecision = serde_json::from_str(&serialized).unwrap();
        assert_eq!(deserialized, decision);
    }

    // ── Closed enum rejection tests ─────────────────────────────────

    #[test]
    fn unknown_policy_reason_code_fails_deserialization() {
        let bad = json!("unknown_reason");
        let result: Result<PolicyReasonCode, _> = serde_json::from_value(bad);
        assert!(result.is_err());
    }

    #[test]
    fn unknown_policy_action_required_fails_deserialization() {
        let bad = json!("unknown_action");
        let result: Result<PolicyActionRequired, _> = serde_json::from_value(bad);
        assert!(result.is_err());
    }

    // ── EffectiveEnvelopeSnapshot round-trip ─────────────────────────

    #[test]
    fn effective_envelope_snapshot_round_trips_mixed_sources() {
        let snapshot = EffectiveEnvelopeSnapshot {
            base_envelope_id: "base-turn-001".into(),
            declared_envelope_id: Some("declared-task-42".into()),
            capabilities: vec![
                SourcedCapability {
                    capability: Capability::ReadPath {
                        path_pattern: "/home/user/project".into(),
                    },
                    source: CapabilitySource::Base,
                },
                SourcedCapability {
                    capability: Capability::WritePath {
                        path_pattern: "/home/user/project/src".into(),
                    },
                    source: CapabilitySource::Declared {
                        envelope_id: "declared-task-42".into(),
                    },
                },
                SourcedCapability {
                    capability: Capability::Inspect,
                    source: CapabilitySource::Both {
                        envelope_id: "declared-task-42".into(),
                    },
                },
            ],
        };
        let serialized = serde_json::to_string(&snapshot).unwrap();
        let deserialized: EffectiveEnvelopeSnapshot = serde_json::from_str(&serialized).unwrap();
        assert_eq!(deserialized, snapshot);
    }

    #[test]
    fn effective_envelope_snapshot_without_declared_round_trips() {
        let snapshot = EffectiveEnvelopeSnapshot {
            base_envelope_id: "base-turn-001".into(),
            declared_envelope_id: None,
            capabilities: vec![SourcedCapability {
                capability: Capability::ReadPath {
                    path_pattern: "/tmp".into(),
                },
                source: CapabilitySource::Base,
            }],
        };
        let serialized = serde_json::to_string(&snapshot).unwrap();
        let deserialized: EffectiveEnvelopeSnapshot = serde_json::from_str(&serialized).unwrap();
        assert_eq!(deserialized, snapshot);
        // Verify declared_envelope_id is absent in JSON
        let json_val: serde_json::Value = serde_json::from_str(&serialized).unwrap();
        assert!(json_val.get("declared_envelope_id").is_none());
    }

    // ── PolicyInput round-trip ───────────────────────────────────────

    #[test]
    fn policy_input_round_trips() {
        let input = PolicyInput {
            method: "observe.workspace".into(),
            params: json!({"mode": "read_file", "path": "/home/user/project/src/main.rs"}),
            envelope_snapshot: EffectiveEnvelopeSnapshot {
                base_envelope_id: "base-001".into(),
                declared_envelope_id: None,
                capabilities: vec![SourcedCapability {
                    capability: Capability::ReadPath {
                        path_pattern: "/home/user/project".into(),
                    },
                    source: CapabilitySource::Base,
                }],
            },
            audit_context: PolicyAuditContext {
                turn_id: "turn-abc".into(),
                session_id: "session-xyz".into(),
                call_id: Some(3),
                budget_snapshot: PolicyBudgetSnapshot {
                    turn_observation_bytes_used: 1024,
                    turn_observation_bytes_limit: 65536,
                    todo_ops_used: 0,
                    todo_ops_limit: 10,
                },
            },
        };
        let serialized = serde_json::to_string(&input).unwrap();
        let deserialized: PolicyInput = serde_json::from_str(&serialized).unwrap();
        assert_eq!(deserialized, input);
    }

    #[test]
    fn policy_input_round_trips_with_empty_params() {
        let input = PolicyInput {
            method: "session.todo_write".into(),
            params: json!({}),
            envelope_snapshot: EffectiveEnvelopeSnapshot {
                base_envelope_id: "base-001".into(),
                declared_envelope_id: None,
                capabilities: vec![],
            },
            audit_context: PolicyAuditContext {
                turn_id: "turn-1".into(),
                session_id: "session-1".into(),
                call_id: None,
                budget_snapshot: PolicyBudgetSnapshot {
                    turn_observation_bytes_used: 0,
                    turn_observation_bytes_limit: 65536,
                    todo_ops_used: 0,
                    todo_ops_limit: 10,
                },
            },
        };
        let serialized = serde_json::to_string(&input).unwrap();
        let deserialized: PolicyInput = serde_json::from_str(&serialized).unwrap();
        assert_eq!(deserialized, input);
    }

    // ── PolicyBudgetSnapshot embedded round-trip ─────────────────────

    #[test]
    fn budget_snapshot_embedded_in_audit_context_round_trips() {
        let ctx = PolicyAuditContext {
            turn_id: "turn-1".into(),
            session_id: "session-1".into(),
            call_id: Some(5),
            budget_snapshot: PolicyBudgetSnapshot {
                turn_observation_bytes_used: 4096,
                turn_observation_bytes_limit: 65536,
                todo_ops_used: 3,
                todo_ops_limit: 10,
            },
        };
        let serialized = serde_json::to_string(&ctx).unwrap();
        let deserialized: PolicyAuditContext = serde_json::from_str(&serialized).unwrap();
        assert_eq!(deserialized, ctx);
        assert_eq!(
            deserialized.budget_snapshot.turn_observation_bytes_used,
            4096
        );
        assert_eq!(deserialized.budget_snapshot.todo_ops_used, 3);
    }

    // ── PolicyReasonCode has expected variant count ──────────────────

    #[test]
    fn policy_reason_code_has_ten_variants() {
        let variants = [
            PolicyReasonCode::MissingCapability,
            PolicyReasonCode::CapabilitySourceNotAllowed,
            PolicyReasonCode::PathOutsideScope,
            PolicyReasonCode::CommandDenied,
            PolicyReasonCode::NetworkDenied,
            PolicyReasonCode::BudgetExceeded,
            PolicyReasonCode::EnvelopeRequired,
            PolicyReasonCode::ReplanRequired,
            PolicyReasonCode::PolicyConfigInvalid,
            PolicyReasonCode::NonDeterminable,
        ];
        assert_eq!(variants.len(), 10);
        for v in &variants {
            let s = serde_json::to_string(v).unwrap();
            let d: PolicyReasonCode = serde_json::from_str(&s).unwrap();
            assert_eq!(&d, v);
        }
    }

    // ── PolicyActionRequired has expected variant count ──────────────

    #[test]
    fn policy_action_required_has_seven_variants() {
        let variants = [
            PolicyActionRequired::DeclareEnvelope,
            PolicyActionRequired::WidenEnvelope,
            PolicyActionRequired::ReduceScope,
            PolicyActionRequired::ChangeCommand,
            PolicyActionRequired::ChangeUrl,
            PolicyActionRequired::ReduceReadSize,
            PolicyActionRequired::AskHuman,
        ];
        assert_eq!(variants.len(), 7);
        for v in &variants {
            let s = serde_json::to_string(v).unwrap();
            let d: PolicyActionRequired = serde_json::from_str(&s).unwrap();
            assert_eq!(&d, v);
        }
    }

    // ── CapabilitySource round-trip ──────────────────────────────────

    #[test]
    fn capability_source_base_round_trips() {
        let src = CapabilitySource::Base;
        let s = serde_json::to_string(&src).unwrap();
        let d: CapabilitySource = serde_json::from_str(&s).unwrap();
        assert_eq!(d, src);
    }

    #[test]
    fn capability_source_declared_round_trips() {
        let src = CapabilitySource::Declared {
            envelope_id: "env-42".into(),
        };
        let s = serde_json::to_string(&src).unwrap();
        let d: CapabilitySource = serde_json::from_str(&s).unwrap();
        assert_eq!(d, src);
    }

    #[test]
    fn capability_source_both_round_trips() {
        let src = CapabilitySource::Both {
            envelope_id: "env-42".into(),
        };
        let s = serde_json::to_string(&src).unwrap();
        let d: CapabilitySource = serde_json::from_str(&s).unwrap();
        assert_eq!(d, src);
    }

    // ── PolicyHook trait compile-time checks ─────────────────────────

    #[test]
    fn policy_hook_is_object_safe() {
        struct AlwaysAllowed;
        impl PolicyHook for AlwaysAllowed {
            fn decide(&self, _input: &PolicyInput) -> PolicyDecision {
                PolicyDecision::Allowed
            }
        }

        let hook: &dyn PolicyHook = &AlwaysAllowed;
        let input = PolicyInput {
            method: "test".into(),
            params: json!({}),
            envelope_snapshot: EffectiveEnvelopeSnapshot {
                base_envelope_id: "base".into(),
                declared_envelope_id: None,
                capabilities: vec![],
            },
            audit_context: PolicyAuditContext {
                turn_id: "t".into(),
                session_id: "s".into(),
                call_id: None,
                budget_snapshot: PolicyBudgetSnapshot {
                    turn_observation_bytes_used: 0,
                    turn_observation_bytes_limit: 0,
                    todo_ops_used: 0,
                    todo_ops_limit: 0,
                },
            },
        };
        assert_eq!(hook.decide(&input), PolicyDecision::Allowed);
    }
}
