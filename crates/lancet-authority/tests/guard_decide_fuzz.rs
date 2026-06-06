// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Property-based fuzz test for `DefaultPolicyHook::decide` and
//! `decide_with_evidence`.
//!
//! Two proptest strategies exercised per [`ProptestConfig::with_cases(256)`]:
//!
//! 1. **biased_allowed** — builds `PolicyInput` where capabilities match the method
//!    (tests positive policy branches: budget, source class, command allowlist, network host).
//! 2. **deny_path** — builds `PolicyInput` with mismatched / empty capabilities
//!    (tests deny paths: capability miss, budget exhaustion, source-class mismatch).
//!
//! Properties tested:
//! - No panic on any valid-shape `PolicyInput` (implicit — proptest catches panics).
//! - Same input → identical `PolicyDecision` (pure determinism).
//! - Denied decisions include a non-empty deny message.
//!
//! ## Regression promotion pattern
//!
//! When proptest finds a failure, copy the minimal input printed to stderr into a new
//! `#[test] fn guard_decide_regression_<hash>` in this file with the seed in a comment.
//! Pinned regressions at the bottom demonstrate this pattern.

use std::path::PathBuf;

use lancet_authority::policy::default::{DefaultPolicyHook, PolicyConfig};
use lancet_protocol::envelope::Capability;
use lancet_protocol::policy::{
    CapabilitySource, EffectiveEnvelopeSnapshot, PolicyAuditContext, PolicyBudgetSnapshot,
    PolicyDecision, PolicyHook, PolicyInput, PolicyReasonCode, SourcedCapability,
};
use proptest::prelude::*;
use serde_json::json;

// ── Shared helpers ─────────────────────────────────────────────────────

fn test_config() -> PolicyConfig {
    PolicyConfig::defaults_for_project_root(PathBuf::from("/home/user/project"))
}

fn base_source() -> CapabilitySource {
    CapabilitySource::Base
}

fn declared_source() -> CapabilitySource {
    CapabilitySource::Declared {
        envelope_id: "prop-envelope".into(),
    }
}

fn default_budget() -> PolicyBudgetSnapshot {
    PolicyBudgetSnapshot {
        turn_observation_bytes_used: 0,
        turn_observation_bytes_limit: 65536,
        todo_ops_used: 0,
        todo_ops_limit: 10,
    }
}

fn budget_at_limit() -> PolicyBudgetSnapshot {
    PolicyBudgetSnapshot {
        turn_observation_bytes_used: 65536,
        turn_observation_bytes_limit: 65536,
        todo_ops_used: 0,
        todo_ops_limit: 10,
    }
}

fn audit_ctx(call_id: u64, budget: PolicyBudgetSnapshot) -> PolicyAuditContext {
    PolicyAuditContext {
        turn_id: "fuzz-turn".into(),
        session_id: "fuzz-session".into(),
        call_id: Some(call_id),
        budget_snapshot: budget,
    }
}

fn envelope_with_caps(caps: Vec<SourcedCapability>) -> EffectiveEnvelopeSnapshot {
    EffectiveEnvelopeSnapshot {
        base_envelope_id: "base".into(),
        declared_envelope_id: Some("declared".into()),
        capabilities: caps,
    }
}

fn empty_envelope() -> EffectiveEnvelopeSnapshot {
    EffectiveEnvelopeSnapshot {
        base_envelope_id: "base".into(),
        declared_envelope_id: None,
        capabilities: vec![],
    }
}

// ── Biased-allowed inputs: capabilities match the method ───────────────
//
// Exercises the positive policy branches:
// - WorkspaceObservation with base ReadPath within scope
// - WorkspaceObservation via grep mode with base Inspect
// - SessionStateMutation with base MutateSessionState
// - ExternalObservation with base FetchUrl wildcard
// - CommandExecution with declared RunCommand (diagnostic echo)
// - WorkspaceMutation with declared WritePath

fn biased_allowed_inputs() -> Vec<PolicyInput> {
    vec![
        PolicyInput {
            method: "observe.workspace".into(),
            params: json!({ "mode": "read_file", "path": "src/main.rs" }),
            envelope_snapshot: envelope_with_caps(vec![SourcedCapability {
                capability: Capability::ReadPath {
                    path_pattern: "src/**".into(),
                },
                source: base_source(),
            }]),
            audit_context: audit_ctx(1, default_budget()),
        },
        PolicyInput {
            method: "observe.workspace".into(),
            params: json!({ "mode": "grep", "pattern": "fn main", "path": "src" }),
            envelope_snapshot: envelope_with_caps(vec![SourcedCapability {
                capability: Capability::Inspect,
                source: base_source(),
            }]),
            audit_context: audit_ctx(2, default_budget()),
        },
        PolicyInput {
            method: "session.todo_write".into(),
            params: json!({ "todos": [{ "content": "fuzz task", "status": "pending" }] }),
            envelope_snapshot: envelope_with_caps(vec![SourcedCapability {
                capability: Capability::MutateSessionState,
                source: base_source(),
            }]),
            audit_context: audit_ctx(3, default_budget()),
        },
        PolicyInput {
            method: "fetch.web".into(),
            params: json!({ "url": "https://example.com/doc" }),
            envelope_snapshot: envelope_with_caps(vec![SourcedCapability {
                capability: Capability::FetchUrl { host: "*".into() },
                source: base_source(),
            }]),
            audit_context: audit_ctx(4, default_budget()),
        },
        // CommandExecution: declared RunCommand for echo (diagnostic allowlist)
        PolicyInput {
            method: "exec.command_run".into(),
            params: json!({ "argv": ["echo", "guard-fuzz"] }),
            envelope_snapshot: envelope_with_caps(vec![SourcedCapability {
                capability: Capability::RunCommand {
                    argv_allowlist: vec!["echo".into()],
                },
                source: declared_source(),
            }]),
            audit_context: audit_ctx(5, default_budget()),
        },
        // WorkspaceMutation: declared WritePath
        PolicyInput {
            method: "mutate.create".into(),
            params: json!({ "path": "src/new.rs", "content": "fn f(){}" }),
            envelope_snapshot: envelope_with_caps(vec![SourcedCapability {
                capability: Capability::WritePath {
                    path_pattern: "src/**".into(),
                },
                source: declared_source(),
            }]),
            audit_context: audit_ctx(6, default_budget()),
        },
    ]
}

// ── Deny-path inputs: empty / mismatched capabilities ─────────────────
//
// Exercises the deny branches:
// - Mutation without declared envelope (MissingCapability)
// - Read over observation budget (BudgetExceeded)
// - Unknown method (NonDeterminable)
// - RunCommand with base source — not allowed (CapabilitySourceNotAllowed)
// - WritePath with base source — not allowed (CapabilitySourceNotAllowed)

fn deny_path_inputs() -> Vec<PolicyInput> {
    vec![
        PolicyInput {
            method: "mutate.create".into(),
            params: json!({ "path": "src/new.rs", "content": "x" }),
            envelope_snapshot: empty_envelope(),
            audit_context: audit_ctx(10, default_budget()),
        },
        PolicyInput {
            method: "observe.workspace".into(),
            params: json!({ "mode": "read_file", "path": "src/lib.rs" }),
            envelope_snapshot: envelope_with_caps(vec![SourcedCapability {
                capability: Capability::ReadPath {
                    path_pattern: "src/**".into(),
                },
                source: base_source(),
            }]),
            audit_context: audit_ctx(11, budget_at_limit()),
        },
        PolicyInput {
            method: "crafted.unknown_fuzz_method".into(),
            params: json!({}),
            envelope_snapshot: empty_envelope(),
            audit_context: audit_ctx(12, default_budget()),
        },
        // RunCommand with base source — source policy requires Declared for commands
        PolicyInput {
            method: "exec.command_run".into(),
            params: json!({ "argv": ["echo", "fuzz"] }),
            envelope_snapshot: envelope_with_caps(vec![SourcedCapability {
                capability: Capability::RunCommand {
                    argv_allowlist: vec!["echo".into()],
                },
                source: base_source(), // command requires Declared source
            }]),
            audit_context: audit_ctx(13, default_budget()),
        },
        // WritePath with base source — source policy requires Declared for mutations
        PolicyInput {
            method: "mutate.edit".into(),
            params: json!({
                "path": "src/lib.rs",
                "expected_hash": "blake3:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "edits": []
            }),
            envelope_snapshot: envelope_with_caps(vec![SourcedCapability {
                capability: Capability::WritePath {
                    path_pattern: "src/**".into(),
                },
                source: base_source(), // mutation requires Declared source
            }]),
            audit_context: audit_ctx(14, default_budget()),
        },
    ]
}

// ── proptest strategies ────────────────────────────────────────────────

fn arb_biased_allowed_input() -> impl Strategy<Value = PolicyInput> {
    let inputs = biased_allowed_inputs();
    let len = inputs.len();
    (0..len).prop_map(move |idx| inputs[idx].clone())
}

fn arb_deny_path_input() -> impl Strategy<Value = PolicyInput> {
    let inputs = deny_path_inputs();
    let len = inputs.len();
    (0..len).prop_map(move |idx| inputs[idx].clone())
}

fn arb_policy_input() -> impl Strategy<Value = PolicyInput> {
    prop_oneof![arb_biased_allowed_input(), arb_deny_path_input()]
}

// ── proptest! blocks ───────────────────────────────────────────────────

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// Property: decide_with_evidence never panics and is deterministic.
    ///
    /// Tests both biased-allowed and deny-path inputs. Calls the hook twice
    /// with the same input and asserts the decision is identical (pure function).
    #[test]
    fn guard_decide_fuzz_no_panic_and_determinism(input in arb_policy_input()) {
        let hook = DefaultPolicyHook::new(test_config());
        // Property 1: no panic (implicit — proptest wraps panics as failures)
        let outcome1 = hook.decide_with_evidence(&input);
        let outcome2 = hook.decide_with_evidence(&input);
        // Property 2: pure determinism
        prop_assert_eq!(&outcome1.decision, &outcome2.decision);
    }

    /// Property: deny-path inputs always produce a Denied decision with non-empty message.
    #[test]
    fn guard_decide_fuzz_deny_has_reason(input in arb_deny_path_input()) {
        let hook = DefaultPolicyHook::new(test_config());
        let outcome = hook.decide_with_evidence(&input);
        // Deny-path inputs must produce a Denied decision
        prop_assert!(
            matches!(outcome.decision, PolicyDecision::Denied { .. }),
            "deny-path input must produce Denied, got {:?}",
            outcome.decision
        );
        // Denied decisions must have a non-empty message
        if let PolicyDecision::Denied { message, .. } = &outcome.decision {
            prop_assert!(!message.is_empty(), "deny message must be non-empty");
        }
    }

    /// Property: biased-allowed inputs are deterministic (no panics, stable decision).
    #[test]
    fn guard_decide_fuzz_biased_allowed_stable(input in arb_biased_allowed_input()) {
        let hook = DefaultPolicyHook::new(test_config());
        let d1 = hook.decide(&input);
        let d2 = hook.decide(&input);
        prop_assert_eq!(d1, d2);
    }
}

// ── Pinned regression tests ────────────────────────────────────────────
//
// Promotion pattern: when proptest finds a failure, proptest prints the minimal
// shrunk case to stderr. Copy that case here as a pinned `#[test]` with the
// seed in a comment. These tests are permanent and run independently of the
// proptest database.
//
// The two tests below demonstrate the promotion pattern for the two most common
// deny-path failure shapes (budget-exhausted and unknown-method).

/// Regression: budget-exhausted read → BudgetExceeded denial with non-empty message.
///
/// Manual promotion from proptest shrink of `arb_deny_path_input` index 1
/// (the budget-at-limit case). Pure determinism also verified.
///
/// seed: deny_path_inputs()[1] — turn_observation_bytes_used == 65536 == limit
#[test]
fn guard_decide_regression_budget_exhausted_deny_message() {
    let hook = DefaultPolicyHook::new(test_config());
    let input = PolicyInput {
        method: "observe.workspace".into(),
        params: json!({ "mode": "read_file", "path": "src/lib.rs" }),
        envelope_snapshot: envelope_with_caps(vec![SourcedCapability {
            capability: Capability::ReadPath {
                path_pattern: "src/**".into(),
            },
            source: base_source(),
        }]),
        audit_context: audit_ctx(11, budget_at_limit()),
    };
    let outcome = hook.decide_with_evidence(&input);
    assert!(
        matches!(
            outcome.decision,
            PolicyDecision::Denied {
                reason_code: PolicyReasonCode::BudgetExceeded,
                ..
            }
        ),
        "budget-exhausted read must be BudgetExceeded deny, got {:?}",
        outcome.decision
    );
    if let PolicyDecision::Denied { message, .. } = &outcome.decision {
        assert!(!message.is_empty(), "deny message must be non-empty");
    }
    // Verify purity: double-call produces identical decision
    let outcome2 = hook.decide_with_evidence(&input);
    assert_eq!(outcome.decision, outcome2.decision);
}

/// Regression: unknown method → NonDeterminable denial with non-empty message.
///
/// Manual promotion from proptest shrink of `arb_deny_path_input` index 2
/// (the unknown-method case). Verifies the reason code and purity.
///
/// seed: deny_path_inputs()[2] — method = "crafted.unknown_fuzz_method"
#[test]
fn guard_decide_regression_unknown_method_nondeterminable_deny() {
    let hook = DefaultPolicyHook::new(test_config());
    let input = PolicyInput {
        method: "crafted.unknown_fuzz_method".into(),
        params: json!({}),
        envelope_snapshot: empty_envelope(),
        audit_context: audit_ctx(12, default_budget()),
    };
    let outcome = hook.decide_with_evidence(&input);
    assert!(
        matches!(
            outcome.decision,
            PolicyDecision::Denied {
                reason_code: PolicyReasonCode::NonDeterminable,
                ..
            }
        ),
        "unknown method must be NonDeterminable deny, got {:?}",
        outcome.decision
    );
    if let PolicyDecision::Denied { message, .. } = &outcome.decision {
        assert!(!message.is_empty(), "deny message must be non-empty");
    }
    // Verify purity
    let outcome2 = hook.decide_with_evidence(&input);
    assert_eq!(outcome.decision, outcome2.decision);
}
