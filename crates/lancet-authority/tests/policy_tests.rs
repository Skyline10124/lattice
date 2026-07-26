// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Authority policy semantics: determinism and workspace write-deny (blacklist)
//! behavior.

use std::path::PathBuf;

use lancet_authority::policy::default::{DefaultPolicyHook, PolicyConfig, WorkspacePolicy};
use lancet_protocol::policy::{
    PolicyAuditContext, PolicyAuthorityInput, PolicyBudgetSnapshot, PolicyDecision, PolicyHook,
    PolicyInput, PolicyReasonCode,
};
use serde_json::json;

fn project_config() -> PolicyConfig {
    let mut config = PolicyConfig::defaults_for_project_root(PathBuf::from("/home/user/project"));
    config.workspace.denied_write_patterns = vec![".git/**".into()];
    config
}

fn policy_input(method: &str, params: serde_json::Value) -> PolicyInput {
    PolicyInput {
        method: method.into(),
        params,
        authority: PolicyAuthorityInput,
        audit_context: PolicyAuditContext {
            turn_id: "turn-1".into(),
            session_id: "session-1".into(),
            call_id: Some(1),
            budget_snapshot: PolicyBudgetSnapshot {
                turn_observation_bytes_used: 0,
                turn_observation_bytes_limit: 65536,
                todo_ops_used: 0,
                todo_ops_limit: 10,
            },
        },
    }
}

#[test]
fn allows_observation_without_denied_patterns() {
    let mut config = project_config();
    config.workspace.denied_write_patterns.clear();
    let hook = DefaultPolicyHook::new(config);
    let input = policy_input(
        "observe.workspace",
        json!({"mode": "read_file", "path": "src/a.rs"}),
    );
    assert_eq!(hook.decide(&input), PolicyDecision::Allowed);
}

#[test]
fn allows_mutation_with_no_denied_patterns() {
    // Blacklist semantics: no denied patterns → all paths allowed
    let mut config = project_config();
    config.workspace.denied_write_patterns.clear();
    let hook = DefaultPolicyHook::new(config);
    let input = policy_input(
        "mutate.create",
        json!({"path": "src/new.rs", "content": "x"}),
    );
    assert_eq!(hook.decide(&input), PolicyDecision::Allowed);
}

#[test]
fn allows_mutation_not_in_deny_list() {
    let hook = DefaultPolicyHook::new(project_config());
    let input = policy_input(
        "mutate.create",
        json!({"path": "src/new.rs", "content": "x"}),
    );
    // src/new.rs is not in the deny list (only .git/** is) → allowed
    assert_eq!(hook.decide(&input), PolicyDecision::Allowed);
}

#[test]
fn denies_retired_command_run_as_unknown_method() {
    // Regression for issue #26 (Codeberg #4): exec.command_run is retired from
    // the catalog,
    // so the resolver rejects it as NonDeterminable before any policy check.
    let hook = DefaultPolicyHook::new(project_config());
    let input = policy_input(
        "exec.command_run",
        json!({"argv": ["sh", "-c", "rm -rf /"], "declared_impact": "diagnostic"}),
    );
    assert!(matches!(
        hook.decide(&input),
        PolicyDecision::Denied {
            reason_code: PolicyReasonCode::NonDeterminable,
            ..
        }
    ));
}

#[test]
fn denies_network_host() {
    let mut config = project_config();
    config.network.allowed_hosts = vec!["allowed.example".into()];
    config.network.allow_public_https = false;
    let hook = DefaultPolicyHook::new(config);
    let input = policy_input("fetch.web", json!({"url": "https://evil.example/page"}));
    assert!(matches!(
        hook.decide(&input),
        PolicyDecision::Denied {
            reason_code: PolicyReasonCode::NetworkDenied,
            ..
        }
    ));
}

#[test]
fn denies_observation_budget_exhausted() {
    let hook = DefaultPolicyHook::new(project_config());
    let mut input = policy_input(
        "observe.workspace",
        json!({"mode": "read_file", "path": "src/a.rs"}),
    );
    input
        .audit_context
        .budget_snapshot
        .turn_observation_bytes_used = 65536;
    assert!(matches!(
        hook.decide(&input),
        PolicyDecision::Denied {
            reason_code: PolicyReasonCode::BudgetExceeded,
            ..
        }
    ));
}

#[test]
fn denies_todo_budget_exhausted() {
    let hook = DefaultPolicyHook::new(project_config());
    let mut input = policy_input("session.todo_write", json!({"todos": []}));
    input.audit_context.budget_snapshot.todo_ops_used = 10;
    assert!(matches!(
        hook.decide(&input),
        PolicyDecision::Denied {
            reason_code: PolicyReasonCode::BudgetExceeded,
            ..
        }
    ));
}

#[test]
fn decision_is_deterministic() {
    let hook = DefaultPolicyHook::new(project_config());
    let input = policy_input("mutate.edit", json!({"path": "src/lib.rs", "edits": []}));
    let first = hook.decide_with_evidence(&input);
    let second = hook.decide_with_evidence(&input);
    assert_eq!(first.decision, second.decision);
    assert_eq!(
        first.evidence.resolved_paths,
        second.evidence.resolved_paths
    );
}

#[test]
fn workspace_policy_validates_denied_patterns() {
    // .. traversal in denied patterns is rejected
    let policy = WorkspacePolicy {
        denied_write_patterns: vec!["../etc".into()],
        ..Default::default()
    };
    assert!(policy.validate().is_err());
}

#[test]
fn workspace_policy_applies_denied_patterns_to_mutate_region() {
    let policy = WorkspacePolicy {
        denied_write_patterns: vec!["crates/**".into()],
        ..Default::default()
    };
    let cep = lancet_authority::CepPolicyConfig::defaults_for_project_root("/tmp/project");
    let mutate_region = policy.authorized_region_for("mutate.create", &cep);
    assert!(!mutate_region.denied_write_path_prefixes.is_empty());
}
