// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Property-based fuzz test for `DefaultPolicyHook::decide` and
//! `decide_with_evidence`.
//!
//! Two proptest strategies exercised per [`ProptestConfig::with_cases(256)`]:
//!
//! 1. **biased_allowed** — builds `PolicyInput` that exercises the positive
//!    native `fetch.web` policy branch.
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
        invocation_id: Some(
            lancet_protocol::InvocationId::parse(&format!(
                "00000000-0000-4000-8000-{call_id:012x}"
            ))
            .expect("invocation id"),
        ),
        budget_snapshot: budget,
    }
}

// ── Biased-allowed inputs ──────────────────────────────────────────────
//
// `fetch.web` is the only remaining native catalog method. Component Tool
// authority is fuzzed in `lancet-runtime`; this legacy hook must reject migrated
// names rather than treating them as positive paths.

fn biased_allowed_inputs() -> Vec<PolicyInput> {
    vec![
        PolicyInput {
            method: "fetch.web".into(),
            params: json!({ "url": "https://example.com/doc" }),
            authority: PolicyAuthorityInput,
            audit_context: audit_ctx(1, default_budget()),
        },
        PolicyInput {
            method: "fetch.web".into(),
            params: json!({ "url": "https://docs.example.org/guide?part=1" }),
            authority: PolicyAuthorityInput,
            audit_context: audit_ctx(2, default_budget()),
        },
    ]
}

// ── Deny-path inputs ───────────────────────────────────────────────────
//
// Exercises the deny branches:
// - Observation over the per-turn budget (BudgetExceeded)
// - Unknown method (NonDeterminable)
// - Retired method exec.command_run (NonDeterminable)
// - Migrated Component methods (NonDeterminable, #29/#30/#31)

fn deny_path_inputs() -> Vec<PolicyInput> {
    vec![
        // Observation budget exhausted → denied. Carried by `fetch.web` since #30:
        // it is the last native method that spends the per-turn observation
        // budget.
        PolicyInput {
            method: "fetch.web".into(),
            params: json!({ "url": "https://example.com/doc" }),
            authority: PolicyAuthorityInput,
            audit_context: audit_ctx(11, budget_at_limit()),
        },
        // Migrated method: `observe.workspace` is a Component Tool since #30, so
        // the native hook must deny it rather than fall back to a legacy policy.
        PolicyInput {
            method: "observe.workspace".into(),
            params: json!({ "mode": "read_file", "path": "src/lib.rs" }),
            authority: PolicyAuthorityInput,
            audit_context: audit_ctx(14, default_budget()),
        },
        PolicyInput {
            method: "mutate.create".into(),
            params: json!({ "path": "src/new.rs", "content": "x" }),
            authority: PolicyAuthorityInput,
            audit_context: audit_ctx(15, default_budget()),
        },
        PolicyInput {
            method: "mutate.edit".into(),
            params: json!({ "path": "src/lib.rs", "edits": [] }),
            authority: PolicyAuthorityInput,
            audit_context: audit_ctx(16, default_budget()),
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

    /// Property: biased-allowed inputs actually reach the decision the set was
    /// built to exercise.
    ///
    /// The stability property above cannot carry this: `DefaultPolicyHook` is a
    /// pure function (`decide` just unwraps `decide_with_evidence`, and the
    /// struct holds only config plus a resolver), so "same input, same decision"
    /// holds under *any* implementation — including one that denies everything.
    /// Without this property the deny direction was asserted and the allow
    /// direction was not, so a policy regressing to deny-all would leave all 256
    /// cases green.
    ///
    #[test]
    fn guard_decide_fuzz_biased_allowed_reaches_expected_decision(
        input in arb_biased_allowed_input()
    ) {
        let hook = DefaultPolicyHook::new(test_config());
        let decision = hook.decide(&input);
        prop_assert_eq!(
            &decision,
            &PolicyDecision::Allowed,
            "positive-path input {} must be Allowed, got {:?}",
            input.method,
            decision
        );
    }
}

// ── Pinned regression tests ────────────────────────────────────────────

/// Regression: budget-exhausted observation → BudgetExceeded denial with
/// non-empty message. On `fetch.web` since #30 moved `observe.workspace` off the
/// native path.
#[test]
fn guard_decide_regression_budget_exhausted_deny_message() {
    let hook = DefaultPolicyHook::new(test_config());
    let input = PolicyInput {
        method: "fetch.web".into(),
        params: json!({ "url": "https://example.com/doc" }),
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
