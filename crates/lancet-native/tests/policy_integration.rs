//! Policy integration tests.
//!
//! Constructs `PolicyInput` values with layered snapshots and budget snapshots,
//! runs `DefaultPolicyHook`, and covers capability + command + network + budget
//! interactions end to end.

use std::path::PathBuf;

use lancet_authority::policy::default::{DefaultPolicyHook, PolicyConfig};
use lancet_protocol::envelope::Capability;
use lancet_protocol::policy::{
    CapabilitySource, EffectiveEnvelopeSnapshot, PolicyAuditContext, PolicyBudgetSnapshot,
    PolicyDecision, PolicyHook, PolicyInput, PolicyReasonCode, SourcedCapability,
};
use serde_json::json;

fn test_config() -> PolicyConfig {
    PolicyConfig::defaults_for_project_root(PathBuf::from("/home/user/project"))
}

fn base_source() -> CapabilitySource {
    CapabilitySource::Base
}

fn declared_source() -> CapabilitySource {
    CapabilitySource::Declared {
        envelope_id: "test-envelope".into(),
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

// ── End-to-end: observe.workspace read_file allowed with base ReadPath ──────────────────

#[test]
fn integration_read_file_allowed_with_base_capability() {
    let hook = DefaultPolicyHook::new(test_config());
    let input = PolicyInput {
        method: "observe.workspace".into(),
        params: json!({"mode": "read_file", "path": "src/main.rs"}),
        envelope_snapshot: envelope_with_caps(vec![SourcedCapability {
            capability: Capability::ReadPath {
                path_pattern: "src/**".into(),
            },
            source: base_source(),
        }]),
        audit_context: audit_ctx(default_budget()),
    };
    assert_eq!(hook.decide(&input), PolicyDecision::Allowed);
}

// ── End-to-end: mutate.create requires declared WritePath ─────────────────

#[test]
fn integration_create_requires_declared_envelope() {
    let hook = DefaultPolicyHook::new(test_config());
    let input = PolicyInput {
        method: "mutate.create".into(),
        params: json!({"path": "src/new.rs", "content": "fn main() {}"}),
        envelope_snapshot: empty_envelope(),
        audit_context: audit_ctx(default_budget()),
    };
    let decision = hook.decide(&input);
    assert!(matches!(
        decision,
        PolicyDecision::Denied {
            reason_code: PolicyReasonCode::MissingCapability,
            ..
        }
    ));
}

// ── End-to-end: mutate.create with declared WritePath allowed ─────────────

#[test]
fn integration_create_allowed_with_declared_capability() {
    let hook = DefaultPolicyHook::new(test_config());
    let input = PolicyInput {
        method: "mutate.create".into(),
        params: json!({"path": "src/new.rs", "content": "fn main() {}"}),
        envelope_snapshot: envelope_with_caps(vec![SourcedCapability {
            capability: Capability::WritePath {
                path_pattern: "src/**".into(),
            },
            source: declared_source(),
        }]),
        audit_context: audit_ctx(default_budget()),
    };
    assert_eq!(hook.decide(&input), PolicyDecision::Allowed);
}

// ── End-to-end: exec.command_run denied by command policy ───────────────────

#[test]
fn integration_command_run_denied_by_policy() {
    let hook = DefaultPolicyHook::new(test_config());
    let input = PolicyInput {
        method: "exec.command_run".into(),
        params: json!({
            "argv": ["sh", "-c", "echo hello"],
            "declared_impact": "workspace_mutation"
        }),
        envelope_snapshot: envelope_with_caps(vec![SourcedCapability {
            capability: Capability::RunCommand {
                argv_allowlist: vec!["sh -c".into()],
            },
            source: declared_source(),
        }]),
        audit_context: audit_ctx(default_budget()),
    };
    let decision = hook.decide(&input);
    assert!(matches!(
        decision,
        PolicyDecision::Denied {
            reason_code: PolicyReasonCode::CommandDenied,
            ..
        }
    ));
}

// ── End-to-end: exec.command_run allowed for diagnostic command ─────────────

#[test]
fn integration_command_run_allowed_for_diagnostic() {
    let hook = DefaultPolicyHook::new(test_config());
    let input = PolicyInput {
        method: "exec.command_run".into(),
        params: json!({
            "argv": ["cargo", "test"],
            "declared_impact": "command_execution"
        }),
        envelope_snapshot: envelope_with_caps(vec![SourcedCapability {
            capability: Capability::RunCommand {
                argv_allowlist: vec!["cargo test".into()],
            },
            source: declared_source(),
        }]),
        audit_context: audit_ctx(default_budget()),
    };
    assert_eq!(hook.decide(&input), PolicyDecision::Allowed);
}

// ── End-to-end: fetch.web denied by network policy ─────────────────────

#[test]
fn integration_web_fetch_denied_by_network_policy() {
    let mut config = test_config();
    config.network.allowed_hosts = vec!["api.github.com".into()];
    config.network.allow_public_https = false;
    let hook = DefaultPolicyHook::new(config);
    let input = PolicyInput {
        method: "fetch.web".into(),
        params: json!({"url": "https://evil.example.com/page"}),
        envelope_snapshot: envelope_with_caps(vec![SourcedCapability {
            capability: Capability::FetchUrl { host: "*".into() },
            source: base_source(),
        }]),
        audit_context: audit_ctx(default_budget()),
    };
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
    let input = PolicyInput {
        method: "fetch.web".into(),
        params: json!({"url": "https://example.com/page"}),
        envelope_snapshot: envelope_with_caps(vec![SourcedCapability {
            capability: Capability::FetchUrl {
                host: "example.com".into(),
            },
            source: base_source(),
        }]),
        audit_context: audit_ctx(default_budget()),
    };
    assert_eq!(hook.decide(&input), PolicyDecision::Allowed);
}

// ── End-to-end: budget exceeded denies observation ─────────────────────

#[test]
fn integration_budget_exceeded_denies_observation() {
    let hook = DefaultPolicyHook::new(test_config());
    let input = PolicyInput {
        method: "observe.workspace".into(),
        params: json!({"mode": "read_file", "path": "src/main.rs"}),
        envelope_snapshot: envelope_with_caps(vec![SourcedCapability {
            capability: Capability::ReadPath {
                path_pattern: "src/**".into(),
            },
            source: base_source(),
        }]),
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
        envelope_snapshot: envelope_with_caps(vec![SourcedCapability {
            capability: Capability::MutateSessionState,
            source: base_source(),
        }]),
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

// ── End-to-end: capability + budget interaction ────────────────────────

#[test]
fn integration_capability_checked_before_budget() {
    let hook = DefaultPolicyHook::new(test_config());
    // No capability at all — should be denied by capability, not budget
    let input = PolicyInput {
        method: "mutate.create".into(),
        params: json!({"path": "src/new.rs", "content": "x"}),
        envelope_snapshot: empty_envelope(),
        audit_context: audit_ctx(PolicyBudgetSnapshot {
            turn_observation_bytes_used: 65536,
            turn_observation_bytes_limit: 65536,
            todo_ops_used: 10,
            todo_ops_limit: 10,
        }),
    };
    let decision = hook.decide(&input);
    assert!(matches!(
        decision,
        PolicyDecision::Denied {
            reason_code: PolicyReasonCode::MissingCapability,
            ..
        }
    ));
}
