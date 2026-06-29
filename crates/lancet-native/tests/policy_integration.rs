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
        call_id: Some(1),
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

// ── End-to-end: observe.workspace read_file allowed ──────────

#[test]
fn integration_read_file_allowed() {
    let hook = DefaultPolicyHook::new(test_config());
    let input = policy_input(
        "observe.workspace",
        json!({"mode": "read_file", "path": "src/main.rs"}),
    );
    let decision = hook.decide(&input);
    // Observation is allowed by default policy.
    assert!(
        matches!(decision, PolicyDecision::Allowed),
        "expected Allowed, got {decision:?}"
    );
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

// ── End-to-end: budget exceeded denies observation ─────────────────────

#[test]
fn integration_budget_exceeded_denies_observation() {
    let hook = DefaultPolicyHook::new(test_config());
    let input = PolicyInput {
        method: "observe.workspace".into(),
        params: json!({"mode": "read_file", "path": "src/main.rs"}),
        authority: PolicyAuthorityInput,
        audit_context: audit_ctx(PolicyBudgetSnapshot {
            turn_observation_bytes_used: 65536,
            turn_observation_bytes_limit: 65536,
            todo_ops_used: 0,
            todo_ops_limit: 10,
        }),
    };
    let decision = hook.decide(&input);
    assert!(matches!(
        decision,
        PolicyDecision::Denied {
            reason_code: PolicyReasonCode::BudgetExceeded,
            ..
        }
    ));
}

// ── End-to-end: todo budget exceeded denies session.todo_write ──────────

#[test]
fn integration_todo_budget_exceeded_denies_todo_write() {
    let hook = DefaultPolicyHook::new(test_config());
    let input = PolicyInput {
        method: "session.todo_write".into(),
        params: json!({"todos": [{"content": "task", "status": "pending"}]}),
        authority: PolicyAuthorityInput,
        audit_context: audit_ctx(PolicyBudgetSnapshot {
            turn_observation_bytes_used: 0,
            turn_observation_bytes_limit: 65536,
            todo_ops_used: 10,
            todo_ops_limit: 10,
        }),
    };
    let decision = hook.decide(&input);
    assert!(matches!(
        decision,
        PolicyDecision::Denied {
            reason_code: PolicyReasonCode::BudgetExceeded,
            ..
        }
    ));
}
