// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Policy integration tests for the post-envelope Direct-mode authority model.
//!
//! Constructs `PolicyInput` values with budget snapshots, runs
//! `DefaultPolicyHook`, and covers command, network, workspace, and budget
//! policy interactions end to end. Authority is sourced via the unit
//! `PolicyAuthorityInput` (the sole execution path).

use std::path::PathBuf;

use lancet_authority::policy::default::{DefaultPolicyHook, PolicyConfig};
use lancet_protocol::policy::{
    PolicyAuditContext, PolicyAuthorityInput, PolicyBudgetSnapshot, PolicyDecision, PolicyHook,
    PolicyInput, PolicyReasonCode,
};
use serde_json::json;

fn test_config() -> PolicyConfig {
    PolicyConfig::defaults_for_project_root(PathBuf::from("/home/user/project"))
}

fn audit_ctx(budget: PolicyBudgetSnapshot) -> PolicyAuditContext {
    PolicyAuditContext {
        turn_id: "turn-1".into(),
        session_id: "session-1".into(),
        invocation_id: Some(
            lancet_protocol::InvocationId::parse("00000000-0000-4000-8000-000000000001")
                .expect("invocation id"),
        ),
        budget_snapshot: budget,
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

fn policy_input(method: &str, params: serde_json::Value) -> PolicyInput {
    PolicyInput {
        method: method.into(),
        params,
        authority: PolicyAuthorityInput,
        audit_context: audit_ctx(default_budget()),
    }
}

// ── End-to-end: a native catalog method is allowed by default policy ────

#[test]
fn integration_native_method_allowed() {
    let mut config = test_config();
    config.network.allow_public_https = true;
    let hook = DefaultPolicyHook::new(config);
    let input = policy_input("fetch.web", json!({"url": "https://example.com/page"}));
    let decision = hook.decide(&input);
    // The remaining native catalog method is allowed by default policy when
    // its network gate is open (#31 retired mutate.create/edit from catalog).
    assert!(
        matches!(decision, PolicyDecision::Allowed),
        "expected Allowed, got {decision:?}"
    );
}

/// A migrated method is not "allowed by default" here — it is not this hook's
/// business at all.
///
/// `DefaultPolicyHook` resolves against `lancet_catalog::catalog()`, and #30/#31
/// removed the Component Tools from it. The refusal is `NonDeterminable`
/// ("unknown catalog method"), not a policy denial, which is the honest answer:
/// a Component Tool's authorization is Cell admission plus the Tool Host's own
/// checks, and a legacy hook that returned `Allowed` for it would be claiming an
/// authority it does not have.
#[test]
fn integration_migrated_method_is_not_resolvable_by_the_legacy_hook() {
    let hook = DefaultPolicyHook::new(test_config());
    for migrated in [
        "observe.workspace",
        "session.todo_write",
        "mutate.create",
        "mutate.edit",
    ] {
        let input = policy_input(
            migrated,
            json!({"mode": "read_file", "path": "src/main.rs"}),
        );
        assert!(
            matches!(
                hook.decide(&input),
                PolicyDecision::Denied {
                    reason_code: PolicyReasonCode::NonDeterminable,
                    ..
                }
            ),
            "{migrated} is a Component Tool and must not resolve through the legacy hook"
        );
    }
}

// ── End-to-end: fetch.web denied by network policy ─────────────────────

#[test]
fn integration_web_fetch_denied_by_network_policy() {
    let mut config = test_config();
    config.network.allowed_hosts = vec!["api.github.com".into()];
    config.network.allow_public_https = false;
    let hook = DefaultPolicyHook::new(config);
    let input = policy_input("fetch.web", json!({"url": "https://evil.example.com/page"}));
    let decision = hook.decide(&input);
    assert!(matches!(
        decision,
        PolicyDecision::Denied {
            reason_code: PolicyReasonCode::NetworkDenied,
            ..
        }
    ));
}

// ── End-to-end: fetch.web allowed for allowed host ─────────────────────

#[test]
fn integration_web_fetch_allowed_for_allowed_host() {
    let mut config = test_config();
    config.network.allow_public_https = true;
    let hook = DefaultPolicyHook::new(config);
    let input = policy_input("fetch.web", json!({"url": "https://example.com/page"}));
    let decision = hook.decide(&input);
    assert!(
        matches!(decision, PolicyDecision::Allowed),
        "expected Allowed, got {decision:?}"
    );
}

// ── End-to-end: budget exceeded denies an observing method ─────────────

/// An exhausted turn observation budget denies a method that observes.
///
/// The subject is the budget gate, and it needs a method this hook can resolve.
/// `observe.workspace` played that part until #30 made it a Component Tool; its
/// budget is still enforced, but now at the point where the observation actually
/// happens — `lancet_runtime::host_workspace` charges the *same shared per-turn
/// tracker* through a closure, which
/// `host_workspace::tests::budget_exhaustion_refuses_the_operation_and_is_recorded`
/// pins. `fetch.web` keeps this hook-level case honest: it is the remaining
/// catalog method whose impact is an observation.
#[test]
fn integration_budget_exceeded_denies_observation() {
    let mut config = test_config();
    config.network.allow_public_https = true;
    let hook = DefaultPolicyHook::new(config);
    let input = PolicyInput {
        method: "fetch.web".into(),
        params: json!({"url": "https://example.com/page"}),
        authority: PolicyAuthorityInput,
        audit_context: audit_ctx(PolicyBudgetSnapshot {
            turn_observation_bytes_used: 65536,
            turn_observation_bytes_limit: 65536,
            todo_ops_used: 0,
            todo_ops_limit: 10,
        }),
    };
    let decision = hook.decide(&input);
    assert!(
        matches!(
            decision,
            PolicyDecision::Denied {
                reason_code: PolicyReasonCode::BudgetExceeded,
                ..
            }
        ),
        "expected BudgetExceeded, got {decision:?}"
    );
}
