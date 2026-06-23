// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Direct authority policy semantics: determinism, envelope independence, and
//! workspace write-region behavior.

use std::path::PathBuf;

use lancet_authority::policy::default::{DefaultPolicyHook, PolicyConfig, WorkspacePolicy};
use lancet_protocol::policy::Capability;
use lancet_protocol::policy::{
    CapabilitySource, EffectiveEnvelopeSnapshot, PolicyAuditContext, PolicyAuthorityInput,
    PolicyBudgetSnapshot, PolicyDecision, PolicyHook, PolicyInput, PolicyReasonCode,
    SourcedCapability,
};
use serde_json::json;

fn project_config() -> PolicyConfig {
    let mut config = PolicyConfig::defaults_for_project_root(PathBuf::from("/home/user/project"));
    config.workspace.allowed_write_paths = vec!["src/**".into()];
    config
}

fn direct_input(method: &str, params: serde_json::Value) -> PolicyInput {
    PolicyInput {
        method: method.into(),
        params,
        authority: PolicyAuthorityInput::Direct,
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

fn envelope_input(
    method: &str,
    params: serde_json::Value,
    snapshot: EffectiveEnvelopeSnapshot,
) -> PolicyInput {
    PolicyInput {
        method: method.into(),
        params,
        authority: PolicyAuthorityInput::Envelope {
            envelope_snapshot: snapshot,
        },
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

fn active_envelope_snapshot() -> EffectiveEnvelopeSnapshot {
    EffectiveEnvelopeSnapshot {
        base_envelope_id: "base".into(),
        declared_envelope_id: Some("declared".into()),
        capabilities: vec![SourcedCapability {
            capability: Capability::WritePath {
                path_pattern: "other/**".into(),
            },
            source: CapabilitySource::Declared {
                envelope_id: "declared".into(),
            },
        }],
    }
}

#[test]
fn direct_policy_input_has_no_envelope_snapshot_field() {
    let input = direct_input(
        "observe.workspace",
        json!({"mode": "read_file", "path": "src/a.rs"}),
    );
    assert!(matches!(input.authority, PolicyAuthorityInput::Direct));
}

#[test]
fn direct_allows_observation_without_write_paths() {
    let mut config = project_config();
    config.workspace.allowed_write_paths.clear();
    let hook = DefaultPolicyHook::new(config);
    let input = direct_input(
        "observe.workspace",
        json!({"mode": "read_file", "path": "src/a.rs"}),
    );
    assert_eq!(hook.decide(&input), PolicyDecision::Allowed);
}

#[test]
fn direct_denies_mutation_without_write_paths() {
    let mut config = project_config();
    config.workspace.allowed_write_paths.clear();
    let hook = DefaultPolicyHook::new(config);
    let input = direct_input(
        "mutate.create",
        json!({"path": "src/new.rs", "content": "x"}),
    );
    assert!(matches!(
        hook.decide(&input),
        PolicyDecision::Denied {
            reason_code: PolicyReasonCode::PathOutsideScope,
            ..
        }
    ));
}

#[test]
fn direct_allows_mutation_with_configured_write_paths() {
    let hook = DefaultPolicyHook::new(project_config());
    let input = direct_input(
        "mutate.create",
        json!({"path": "src/new.rs", "content": "x"}),
    );
    assert_eq!(hook.decide(&input), PolicyDecision::Allowed);
}

#[test]
fn direct_denies_diagnostic_command_argv() {
    let hook = DefaultPolicyHook::new(project_config());
    let input = direct_input(
        "exec.command_run",
        json!({"argv": ["sh", "-c", "rm -rf /"], "declared_impact": "diagnostic"}),
    );
    assert!(matches!(
        hook.decide(&input),
        PolicyDecision::Denied {
            reason_code: PolicyReasonCode::CommandDenied,
            ..
        }
    ));
}

#[test]
fn direct_allows_diagnostic_command() {
    let hook = DefaultPolicyHook::new(project_config());
    let input = direct_input(
        "exec.command_run",
        json!({"argv": ["echo", "ok"], "declared_impact": "diagnostic"}),
    );
    assert_eq!(hook.decide(&input), PolicyDecision::Allowed);
}

#[test]
fn direct_denies_workspace_mutating_command_without_write_paths() {
    let mut config = project_config();
    config.workspace.allowed_write_paths.clear();
    let hook = DefaultPolicyHook::new(config);
    let input = direct_input(
        "exec.command_run",
        json!({"argv": ["cargo", "build"], "declared_impact": "workspace_mutation"}),
    );
    assert!(matches!(
        hook.decide(&input),
        PolicyDecision::Denied {
            reason_code: PolicyReasonCode::PathOutsideScope,
            ..
        }
    ));
}

#[test]
fn direct_denies_network_host() {
    let mut config = project_config();
    config.network.allowed_hosts = vec!["allowed.example".into()];
    config.network.allow_public_https = false;
    let hook = DefaultPolicyHook::new(config);
    let input = direct_input("fetch.web", json!({"url": "https://evil.example/page"}));
    assert!(matches!(
        hook.decide(&input),
        PolicyDecision::Denied {
            reason_code: PolicyReasonCode::NetworkDenied,
            ..
        }
    ));
}

#[test]
fn direct_denies_observation_budget_exhausted() {
    let hook = DefaultPolicyHook::new(project_config());
    let mut input = direct_input(
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
fn direct_denies_todo_budget_exhausted() {
    let hook = DefaultPolicyHook::new(project_config());
    let mut input = direct_input("session.todo_write", json!({"todos": []}));
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
fn direct_decision_is_deterministic() {
    let hook = DefaultPolicyHook::new(project_config());
    let input = direct_input("mutate.edit", json!({"path": "src/lib.rs", "edits": []}));
    let first = hook.decide_with_evidence(&input);
    let second = hook.decide_with_evidence(&input);
    assert_eq!(first.decision, second.decision);
    assert_eq!(
        first.evidence.resolved_paths,
        second.evidence.resolved_paths
    );
}

#[test]
fn direct_decision_invariant_under_envelope_mutation() {
    let hook = DefaultPolicyHook::new(project_config());
    let input = direct_input(
        "mutate.create",
        json!({"path": "src/new.rs", "content": "fn main() {}"}),
    );
    let baseline = hook.decide_with_evidence(&input);

    let _envelope_only = envelope_input("session.envelope", json!({}), active_envelope_snapshot());
    let after_envelope_context = hook.decide_with_evidence(&input);
    assert_eq!(baseline.decision, after_envelope_context.decision);
    assert_eq!(
        baseline.evidence.resolved_paths,
        after_envelope_context.evidence.resolved_paths
    );
}

#[test]
fn workspace_policy_rejects_git_paths() {
    let policy = WorkspacePolicy {
        allowed_write_paths: vec![".git/config".into()],
    };
    assert!(policy.validate().is_err());
}

#[test]
fn workspace_policy_shared_by_mutate_and_command_regions() {
    let policy = WorkspacePolicy {
        allowed_write_paths: vec!["crates/**".into()],
    };
    let cep = lancet_authority::CepPolicyConfig::defaults_for_project_root("/tmp/project");
    let mutate_region = policy.authorized_region_for("mutate.create", None, &cep);
    let command_region = policy.authorized_region_for(
        "exec.command_run",
        Some(&lancet_authority::policy::command::EffectiveImpact::WorkspaceMutation),
        &cep,
    );
    assert!(!mutate_region.write_path_prefixes.is_empty());
    assert_eq!(
        mutate_region.write_path_prefixes,
        command_region.write_path_prefixes
    );
}
