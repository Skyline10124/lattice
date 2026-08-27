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
            invocation_id: Some(
                lancet_protocol::InvocationId::parse("00000000-0000-4000-8000-000000000001")
                    .expect("invocation id"),
            ),
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
fn denies_migrated_mutations_as_unknown_legacy_methods() {
    let hook = DefaultPolicyHook::new(project_config());
    for method in ["mutate.create", "mutate.edit"] {
        let input = policy_input(method, json!({"path": "src/new.rs"}));
        assert!(matches!(
            hook.decide(&input),
            PolicyDecision::Denied {
                reason_code: PolicyReasonCode::NonDeterminable,
                ..
            }
        ));
    }
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
    // Assert the profile pattern by name, not merely that the vec is non-empty:
    // System Path Exclusions push into this same vec, so `!is_empty()` was
    // satisfiable without the profile pattern contributing anything at all.
    assert!(
        mutate_region
            .denied_write_path_prefixes
            .iter()
            .any(|p| p.ends_with("crates")),
        "profile denied_write_patterns must reach the mutate region: {:?}",
        mutate_region.denied_write_path_prefixes
    );
}

/// System Path Exclusions are host-enforced and NOT profile-configurable
/// (AGENTS.md / ADR 0003), so they must be present in the mutate region even
/// when the profile denies nothing.
///
/// Deliberately configures an empty `denied_write_patterns`: the previous
/// assertion for this behavior only checked that
/// `denied_write_path_prefixes` was non-empty while passing a profile pattern
/// that populated the very same vec, so deleting the exclusion loop in
/// `authorized_region_for` left it green — see #111.
#[test]
fn mutate_region_carries_system_path_exclusions_without_any_profile_pattern() {
    let policy = WorkspacePolicy {
        denied_write_patterns: vec![],
        ..Default::default()
    };
    let cep = lancet_authority::CepPolicyConfig::defaults_for_project_root("/tmp/project");

    for method in ["mutate.create", "mutate.edit"] {
        let region = policy.authorized_region_for(method, &cep);
        for exclusion in [".git", ".lattice"] {
            assert!(
                region
                    .denied_write_path_prefixes
                    .iter()
                    .any(|p| p == std::path::Path::new(exclusion)),
                "{method}: host-enforced exclusion {exclusion:?} missing from \
                 denied_write_path_prefixes: {:?}",
                region.denied_write_path_prefixes
            );
        }
    }
}
