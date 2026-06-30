// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Property-based fuzz test for `DefaultPolicyHook::decide` and
//! `decide_with_evidence`.
//!
//! Two proptest strategies exercised per [`ProptestConfig::with_cases(256)`]:
//!
//! 1. **biased_allowed** — builds `PolicyInput` that exercises positive policy
//!    branches (observation, session state, fetch, diagnostic command).
//! 2. **deny_path** — builds `PolicyInput` that exercises deny paths
//!    (budget exhaustion, unknown method, missing write paths).
//!
//! Properties tested:
//! - No panic on any valid-shape `PolicyInput` (implicit — proptest catches panics).
//! - Same input → identical `PolicyDecision` (pure determinism).
//! - Denied decisions include a non-empty deny message.

use std::path::PathBuf;

use lancet_authority::policy::default::{DefaultPolicyHook, PolicyConfig};
use lancet_protocol::policy::{
    PolicyAuditContext, PolicyAuthorityInput, PolicyBudgetSnapshot, PolicyDecision, PolicyHook,
    PolicyInput, PolicyReasonCode,
};
use proptest::prelude::*;
use serde_json::json;

// ── Shared helpers ─────────────────────────────────────────────────────

fn test_config() -> PolicyConfig {
    let mut config = PolicyConfig::defaults_for_project_root(PathBuf::from("/home/user/project"));
    config.workspace.denied_write_patterns = vec!["src/**".into()];
    config
}

#[allow(dead_code)]
fn test_config_no_denied_patterns() -> PolicyConfig {
    let mut config = PolicyConfig::defaults_for_project_root(PathBuf::from("/home/user/project"));
    config.workspace.denied_write_patterns.clear();
    config
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

// ── Biased-allowed inputs ──────────────────────────────────────────────
//
// Exercises the positive policy branches:
// - WorkspaceObservation (read_file)
// - WorkspaceObservation via grep mode
// - SessionStateMutation (todo_write)
// - ExternalObservation (fetch.web with default allowed hosts)
// - CommandExecution (diagnostic echo)

fn biased_allowed_inputs() -> Vec<PolicyInput> {
    vec![
        PolicyInput {
            method: "observe.workspace".into(),
            params: json!({ "mode": "read_file", "path": "src/main.rs" }),
            authority: PolicyAuthorityInput,
            audit_context: audit_ctx(1, default_budget()),
        },
        PolicyInput {
            method: "observe.workspace".into(),
            params: json!({ "mode": "grep", "pattern": "fn main", "path": "src" }),
            authority: PolicyAuthorityInput,
            audit_context: audit_ctx(2, default_budget()),
        },
        PolicyInput {
            method: "session.todo_write".into(),
            params: json!({ "todos": [{ "content": "fuzz task", "status": "pending" }] }),
            authority: PolicyAuthorityInput,
            audit_context: audit_ctx(3, default_budget()),
        },
        PolicyInput {
            method: "fetch.web".into(),
            params: json!({ "url": "https://example.com/doc" }),
            authority: PolicyAuthorityInput,
            audit_context: audit_ctx(4, default_budget()),
        },
        // Retired method: exec.command_run is no longer in the catalog,
        // so it is rejected as NonDeterminable (a Denied variant).
        PolicyInput {
            method: "exec.command_run".into(),
            params: json!({ "argv": ["echo", "guard-fuzz"] }),
            authority: PolicyAuthorityInput,
            audit_context: audit_ctx(5, default_budget()),
        },
        // WorkspaceMutation: with configured write paths
        PolicyInput {
            method: "mutate.create".into(),
            params: json!({ "path": "src/new.rs", "content": "fn f(){}" }),
            authority: PolicyAuthorityInput,
            audit_context: audit_ctx(6, default_budget()),
        },
    ]
}

// ── Deny-path inputs ───────────────────────────────────────────────────
//
// Exercises the deny branches:
// - Mutation without configured write paths (PathOutsideScope)
// - Read over observation budget (BudgetExceeded)
// - Unknown method (NonDeterminable)
// - Retired method exec.command_run (NonDeterminable)

fn deny_path_inputs() -> Vec<PolicyInput> {
    vec![
        // Observation budget exhausted → denied
        PolicyInput {
            method: "observe.workspace".into(),
            params: json!({ "mode": "read_file", "path": "src/lib.rs" }),
            authority: PolicyAuthorityInput,
            audit_context: audit_ctx(11, budget_at_limit()),
        },
        PolicyInput {
            method: "crafted.unknown_fuzz_method".into(),
            params: json!({}),
            authority: PolicyAuthorityInput,
            audit_context: audit_ctx(12, default_budget()),
        },
        // Retired method: exec.command_run is rejected as NonDeterminable
        PolicyInput {
            method: "exec.command_run".into(),
            params: json!({ "argv": ["sh", "-c", "rm -rf /"] }),
            authority: PolicyAuthorityInput,
            audit_context: audit_ctx(13, default_budget()),
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
    #[test]
    fn guard_decide_fuzz_no_panic_and_determinism(input in arb_policy_input()) {
        let hook = DefaultPolicyHook::new(test_config());
        let outcome1 = hook.decide_with_evidence(&input);
        let outcome2 = hook.decide_with_evidence(&input);
        prop_assert_eq!(&outcome1.decision, &outcome2.decision);
    }

    /// Property: deny-path inputs always produce a Denied decision with non-empty message.
    #[test]
    fn guard_decide_fuzz_deny_has_reason(input in arb_deny_path_input()) {
        let hook = DefaultPolicyHook::new(test_config());
        let outcome = hook.decide_with_evidence(&input);
        prop_assert!(
            matches!(outcome.decision, PolicyDecision::Denied { .. }),
            "deny-path input must produce Denied, got {:?}",
            outcome.decision
        );
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

/// Regression: budget-exhausted read → BudgetExceeded denial with non-empty message.
#[test]
fn guard_decide_regression_budget_exhausted_deny_message() {
    let hook = DefaultPolicyHook::new(test_config());
    let input = PolicyInput {
        method: "observe.workspace".into(),
        params: json!({ "mode": "read_file", "path": "src/lib.rs" }),
        authority: PolicyAuthorityInput,
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
    let outcome2 = hook.decide_with_evidence(&input);
    assert_eq!(outcome.decision, outcome2.decision);
}

/// Regression: unknown method → NonDeterminable denial with non-empty message.
#[test]
fn guard_decide_regression_unknown_method_nondeterminable_deny() {
    let hook = DefaultPolicyHook::new(test_config());
    let input = PolicyInput {
        method: "crafted.unknown_fuzz_method".into(),
        params: json!({}),
        authority: PolicyAuthorityInput,
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
    let outcome2 = hook.decide_with_evidence(&input);
    assert_eq!(outcome.decision, outcome2.decision);
}
