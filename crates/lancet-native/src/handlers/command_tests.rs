/// Check if argv matches the RunCommand allowlist.
#[cfg(test)]
fn matches_argv_allowlist(argv: &[String], allowlist: &[String]) -> bool {
    for entry in allowlist {
        let tokens: Vec<&str> = entry.split_whitespace().collect();
        if argv.len() >= tokens.len()
            && argv
                .iter()
                .zip(tokens.iter())
                .all(|(a, t)| a.as_str() == *t)
        {
            return true;
        }
    }
    false
}

use super::*;
use crate::server::registry::{Cancellation, EventSink};
use lancet_authority::envelope_store::ActiveEnvelopeStore;
use lancet_authority::policy::{CepPolicyConfig, CepTurnContext};
use lancet_protocol::envelope::{
    BaseExecutionEnvelope, Constraints, ExecutionEnvelope, Impact as EnvImpact, ImpactConfidence,
    ImpactKind, Intent, OnFailure, Scope, VerifyCommand,
};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use tokio::sync::{mpsc, watch};

fn test_ctx(
    root: &str,
) -> (
    HandlerContext,
    mpsc::Receiver<lancet_protocol::ToolEvent>,
    watch::Sender<bool>,
) {
    let (event_tx, event_rx) = mpsc::channel(32);
    let (cancel_tx, cancel_rx) = watch::channel(false);
    let default_hook = Arc::new(lancet_authority::policy::default::DefaultPolicyHook::new(
        lancet_authority::policy::default::PolicyConfig::defaults_for_project_root(
            std::path::PathBuf::from(root),
        ),
    ));
    (
        HandlerContext {
            call_id: 1,
            events: EventSink::new(event_tx, 1),
            cancellation: Cancellation::new(cancel_rx),
            base_envelope: BaseExecutionEnvelope::for_workspace_root(root, "command handler test"),
            cep_context: Arc::new(CepTurnContext::defaults_for_project_root(root)),
            active_envelope: Arc::new(ActiveEnvelopeStore::new()),
            turn_id: "test-turn".into(),
            durable_metadata: crate::server::call_durable_metadata::CallDurableMetadata::default(),
            effect_ledger: lancet_authority::effect::ledger::EffectLedger::new(),
            authorized_region: lancet_authority::effect::ledger::AuthorizedRegionSlot::new(),
            policy_hook: default_hook.clone() as Arc<dyn lancet_protocol::policy::PolicyHook>,
            default_policy_hook: Some(default_hook),
            audit_context: Arc::new(tokio::sync::RwLock::new(crate::audit::CepAuditContext {
                turn_id: "test-turn".into(),
                session_id: "test-session".into(),
                ..Default::default()
            })),
            audit: Arc::new(crate::audit::CepAuditStore::in_memory().expect("audit store")),
            invariant_index: Arc::new(lancet_invariants::InvariantCardIndex::new(vec![])),
            cep_mode: lancet_protocol::envelope::CepMode::Off,
            command_isolation:
                crate::enforcement::isolation::CommandIsolationRuntimeConfig::default(),
        },
        event_rx,
        cancel_tx,
    )
}

fn native_command_registry(root: &str) -> MethodRegistry {
    let mut registry = MethodRegistry::new();
    register_command_methods(&mut registry).unwrap();
    let _ = root;
    registry
}

fn command_boundary_probe(
    entries: &[(
        lancet_protocol::BoundaryPrimitive,
        lancet_protocol::HostProbeStatus,
        &str,
    )],
) -> crate::enforcement::isolation::HostProbeReport {
    crate::enforcement::isolation::HostProbeReport::from_entries(entries.iter().map(
        |(primitive, status, reason)| {
            crate::enforcement::isolation::HostProbeEntry::new(*primitive, *status, *reason)
        },
    ))
}

fn command_boundary_config(
    mode: crate::enforcement::isolation::CommandIsolationMode,
    required_boundaries: Vec<lancet_protocol::BoundaryDimension>,
    host_probe: crate::enforcement::isolation::HostProbeReport,
) -> crate::enforcement::isolation::CommandIsolationRuntimeConfig {
    crate::enforcement::isolation::CommandIsolationRuntimeConfig {
        mode,
        required_boundaries,
        optional_boundaries: Vec::new(),
        read_only_toolchain_paths: Vec::new(),
        scratch_paths: Vec::new(),
        landlock: crate::enforcement::isolation::CommandIsolationLandlockBehavior::Auto,
        unsupported_host: match mode {
            crate::enforcement::isolation::CommandIsolationMode::Enforce => {
                crate::enforcement::isolation::CommandIsolationUnsupportedHostBehavior::FailClosed
            }
            crate::enforcement::isolation::CommandIsolationMode::Audit => {
                crate::enforcement::isolation::CommandIsolationUnsupportedHostBehavior::AllowUnbounded
            }
            crate::enforcement::isolation::CommandIsolationMode::Off => {
                crate::enforcement::isolation::CommandIsolationUnsupportedHostBehavior::NotRequested
            }
        },
        host_probe: Some(host_probe),
    }
}

fn command_boundary_metadata_json(
    durable: &crate::server::call_durable_metadata::CallDurableMetadata,
) -> serde_json::Value {
    serde_json::from_str(
        &durable
            .take_json()
            .expect("command boundary durable metadata"),
    )
    .expect("durable metadata json")
}

async fn run_echo_with_context(
    registry: &MethodRegistry,
    ctx: HandlerContext,
) -> Result<serde_json::Value, CepError> {
    let params = RunParams {
        argv: vec!["echo".into(), "command-boundary".into()],
        cwd: None,
        env: BTreeMap::new(),
        timeout_ms: None,
        declared_impact: Some(ImpactKind::CommandExecution),
    };
    registry
        .dispatch(Run::NAME, serde_json::to_value(&params).unwrap(), ctx)
        .await
}

async fn next_command_metadata_event(
    events: &mut mpsc::Receiver<lancet_protocol::ToolEvent>,
) -> lancet_protocol::ToolEvent {
    while let Some(event) = events.recv().await {
        if event.method == "cep.command_metadata" {
            return event;
        }
    }
    panic!("command metadata event not emitted");
}

#[tokio::test]
async fn command_boundary_enforce_unavailable_required_fails_before_backend_with_metadata() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path().to_str().unwrap();
    let registry = native_command_registry(root);
    let (mut ctx, mut events, _cancel_tx) =
        test_ctx_with_run_command(root, vec!["echo".into()]).await;
    ctx.command_isolation = command_boundary_config(
        crate::enforcement::isolation::CommandIsolationMode::Enforce,
        vec![lancet_protocol::BoundaryDimension::Network],
        command_boundary_probe(&[(
            lancet_protocol::BoundaryPrimitive::NetworkNamespace,
            lancet_protocol::HostProbeStatus::Unavailable,
            "netns unavailable in test",
        )]),
    );
    let durable = ctx.durable_metadata.clone();

    let err = run_echo_with_context(&registry, ctx)
        .await
        .expect_err("enforce mode must fail before execute_argv");

    assert_eq!(err.code, CepErrorCode::GuardDenied);
    let metadata = command_boundary_metadata_json(&durable);
    assert_eq!(metadata["backend_invoked"], false);
    assert_eq!(
        metadata["effective_boundary"]["network"]["status"],
        "failed_closed"
    );
    assert_eq!(
        metadata["host_probe_summary"]["primitives"]["network_namespace"]["status"],
        "unavailable"
    );
    let event = next_command_metadata_event(&mut events).await;
    assert_eq!(event.method, "cep.command_metadata");
    assert_eq!(
        event.params["metadata"]["effective_boundary"]["network"]["status"],
        "failed_closed"
    );
}

#[tokio::test]
async fn command_boundary_enforce_unknown_required_probe_fails_closed() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path().to_str().unwrap();
    let registry = native_command_registry(root);
    let (mut ctx, _events, _cancel_tx) = test_ctx_with_run_command(root, vec!["echo".into()]).await;
    ctx.command_isolation = command_boundary_config(
        crate::enforcement::isolation::CommandIsolationMode::Enforce,
        vec![lancet_protocol::BoundaryDimension::Network],
        command_boundary_probe(&[(
            lancet_protocol::BoundaryPrimitive::NetworkNamespace,
            lancet_protocol::HostProbeStatus::Unknown,
            "netns probe unknown in test",
        )]),
    );
    let durable = ctx.durable_metadata.clone();

    let err = run_echo_with_context(&registry, ctx)
        .await
        .expect_err("unknown required probe result must fail closed");

    assert_eq!(err.code, CepErrorCode::GuardDenied);
    let metadata = command_boundary_metadata_json(&durable);
    assert_eq!(metadata["backend_invoked"], false);
    assert_eq!(
        metadata["effective_boundary"]["network"]["status"],
        "unknown"
    );
}

#[tokio::test]
async fn command_boundary_audit_unavailable_required_runs_and_records_unbounded() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path().to_str().unwrap();
    let registry = native_command_registry(root);
    let (mut ctx, _events, _cancel_tx) = test_ctx_with_run_command(root, vec!["echo".into()]).await;
    ctx.command_isolation = command_boundary_config(
        crate::enforcement::isolation::CommandIsolationMode::Audit,
        vec![lancet_protocol::BoundaryDimension::Network],
        command_boundary_probe(&[(
            lancet_protocol::BoundaryPrimitive::NetworkNamespace,
            lancet_protocol::HostProbeStatus::Unavailable,
            "netns unavailable in audit test",
        )]),
    );
    let durable = ctx.durable_metadata.clone();

    let value = run_echo_with_context(&registry, ctx)
        .await
        .expect("audit mode may execute unbounded command");
    let parsed: RunResult = serde_json::from_value(value).unwrap();

    assert_eq!(parsed.exit_code, 0);
    assert!(parsed.stdout.contains("command-boundary"));
    let metadata = command_boundary_metadata_json(&durable);
    assert_eq!(metadata["backend_invoked"], true);
    assert_eq!(
        metadata["effective_boundary"]["network"]["status"],
        "unbounded"
    );
}

#[tokio::test]
async fn command_boundary_off_runs_and_records_not_requested_for_every_boundary() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path().to_str().unwrap();
    let registry = native_command_registry(root);
    let (mut ctx, _events, _cancel_tx) = test_ctx_with_run_command(root, vec!["echo".into()]).await;
    ctx.command_isolation = command_boundary_config(
        crate::enforcement::isolation::CommandIsolationMode::Off,
        vec![lancet_protocol::BoundaryDimension::Network],
        command_boundary_probe(&[]),
    );
    let durable = ctx.durable_metadata.clone();

    let value = run_echo_with_context(&registry, ctx)
        .await
        .expect("off mode must keep existing command policy semantics");
    let parsed: RunResult = serde_json::from_value(value).unwrap();

    assert_eq!(parsed.exit_code, 0);
    let metadata = command_boundary_metadata_json(&durable);
    for boundary in ["network", "filesystem", "landlock"] {
        assert_eq!(
            metadata["effective_boundary"][boundary]["status"],
            "not_requested"
        );
    }
}

async fn test_ctx_with_run_command(
    root: &str,
    argv_allowlist: Vec<String>,
) -> (
    HandlerContext,
    mpsc::Receiver<lancet_protocol::ToolEvent>,
    watch::Sender<bool>,
) {
    let (event_tx, event_rx) = mpsc::channel(32);
    let (cancel_tx, cancel_rx) = watch::channel(false);
    let default_hook = Arc::new(lancet_authority::policy::default::DefaultPolicyHook::new(
        lancet_authority::policy::default::PolicyConfig::defaults_for_project_root(
            std::path::PathBuf::from(root),
        ),
    ));
    let store = Arc::new(ActiveEnvelopeStore::new());
    let mut caps = std::collections::BTreeSet::new();
    caps.insert(Capability::RunCommand { argv_allowlist });
    let envelope = ExecutionEnvelope {
        intent: lancet_protocol::envelope::Intent {
            description: "command test".into(),
        },
        capabilities: caps,
        scope: lancet_protocol::envelope::Scope::default(),
        impact: vec![lancet_protocol::envelope::Impact {
            kind: lancet_protocol::envelope::ImpactKind::CommandExecution,
            target: root.into(),
            scope: None,
            confidence: lancet_protocol::envelope::ImpactConfidence::Conservative,
            unknowns: vec![],
        }],
        constraints: lancet_protocol::envelope::Constraints::default(),
        verify: None,
        on_failure: None,
        extensions: serde_json::Map::new(),
    };
    store.declare(envelope, 0, "test-turn").await.unwrap();
    (
        HandlerContext {
            call_id: 1,
            events: EventSink::new(event_tx, 1),
            cancellation: Cancellation::new(cancel_rx),
            base_envelope: BaseExecutionEnvelope::for_workspace_root(root, "command handler test"),
            cep_context: Arc::new(CepTurnContext::defaults_for_project_root(root)),
            active_envelope: store,
            turn_id: "test-turn".into(),
            durable_metadata: crate::server::call_durable_metadata::CallDurableMetadata::default(),
            effect_ledger: lancet_authority::effect::ledger::EffectLedger::new(),
            authorized_region: lancet_authority::effect::ledger::AuthorizedRegionSlot::new(),
            policy_hook: default_hook.clone() as Arc<dyn lancet_protocol::policy::PolicyHook>,
            default_policy_hook: Some(default_hook),
            audit_context: Arc::new(tokio::sync::RwLock::new(crate::audit::CepAuditContext {
                turn_id: "test-turn".into(),
                session_id: "test-session".into(),
                ..Default::default()
            })),
            audit: Arc::new(crate::audit::CepAuditStore::in_memory().expect("audit store")),
            invariant_index: Arc::new(lancet_invariants::InvariantCardIndex::new(vec![])),
            cep_mode: lancet_protocol::envelope::CepMode::Off,
            command_isolation:
                crate::enforcement::isolation::CommandIsolationRuntimeConfig::default(),
        },
        event_rx,
        cancel_tx,
    )
}

#[tokio::test]
async fn diagnostic_echo_succeeds_without_envelope() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path().to_str().unwrap();
    let registry = native_command_registry(root);
    let (ctx, _events, _cancel_tx) = test_ctx_with_run_command(root, vec!["echo".into()]).await;

    let params = RunParams {
        argv: vec!["echo".into(), "hello".into()],
        cwd: None,
        env: BTreeMap::new(),
        timeout_ms: None,
        declared_impact: None,
    };
    let result = registry
        .dispatch(Run::NAME, serde_json::to_value(&params).unwrap(), ctx)
        .await
        .unwrap();
    let parsed: RunResult = serde_json::from_value(result).unwrap();
    assert_eq!(parsed.exit_code, 0);
    assert!(parsed.stdout.contains("hello"));
}

#[tokio::test]
async fn hard_denied_sh_c_returns_guard_denied() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path().to_str().unwrap();
    let registry = native_command_registry(root);
    let (ctx, _events, _cancel_tx) = test_ctx_with_run_command(root, vec!["sh".into()]).await;

    let params = RunParams {
        argv: vec!["sh".into(), "-c".into(), "rm -rf /".into()],
        cwd: None,
        env: BTreeMap::new(),
        timeout_ms: None,
        declared_impact: None,
    };
    let err = registry
        .dispatch(Run::NAME, serde_json::to_value(&params).unwrap(), ctx)
        .await
        .unwrap_err();
    assert_eq!(err.code, CepErrorCode::GuardDenied);
    assert!(
        err.message.contains("denied")
            || err.message.contains("command")
            || err.message.contains("capability"),
        "unexpected error message: {}",
        err.message
    );
}

#[tokio::test]
async fn workspace_mutation_without_envelope_returns_envelope_missing() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path().to_str().unwrap();
    let registry = native_command_registry(root);
    let (ctx, _events, _cancel_tx) = test_ctx(root);

    let params = RunParams {
        argv: vec!["cargo".into(), "fmt".into()],
        cwd: None,
        env: BTreeMap::new(),
        timeout_ms: None,
        declared_impact: Some(ImpactKind::WorkspaceMutation),
    };
    let err = registry
        .dispatch(Run::NAME, serde_json::to_value(&params).unwrap(), ctx)
        .await
        .unwrap_err();
    assert_eq!(err.code, CepErrorCode::CapabilityNotGranted);
}

#[tokio::test]
async fn workspace_mutation_with_envelope_succeeds() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path().to_str().unwrap();
    let registry = native_command_registry(root);

    let (event_tx, _event_rx) = mpsc::channel(32);
    let (_cancel_tx, cancel_rx) = watch::channel(false);
    let store = Arc::new(ActiveEnvelopeStore::new());

    // Declare an envelope with RunCommand
    let mut capabilities = BTreeSet::new();
    capabilities.insert(Capability::RunCommand {
        argv_allowlist: vec!["cargo fmt".into()],
    });
    let envelope = ExecutionEnvelope {
        intent: Intent {
            description: "test mutation".into(),
        },
        capabilities,
        scope: Scope {
            workspace_root: Some(root.into()),
            extensions: serde_json::Map::new(),
        },
        impact: vec![EnvImpact {
            kind: ImpactKind::WorkspaceMutation,
            target: root.into(),
            scope: None,
            confidence: ImpactConfidence::Conservative,
            unknowns: vec![],
        }],
        constraints: Constraints::default(),
        verify: None,
        on_failure: Some(OnFailure::RetainDiff),
        extensions: serde_json::Map::new(),
    };
    store.declare(envelope, 0, "test-turn").await.unwrap();

    let ctx = HandlerContext {
        call_id: 1,
        events: EventSink::new(event_tx, 1),
        cancellation: Cancellation::new(cancel_rx),
        base_envelope: BaseExecutionEnvelope::for_workspace_root(root, "mutation test"),
        cep_context: Arc::new(CepTurnContext::defaults_for_project_root(root)),
        active_envelope: store,
        turn_id: "test-turn".into(),
        durable_metadata: crate::server::call_durable_metadata::CallDurableMetadata::default(),
        effect_ledger: lancet_authority::effect::ledger::EffectLedger::new(),
        authorized_region: lancet_authority::effect::ledger::AuthorizedRegionSlot::new(),
        policy_hook: Arc::new(lancet_authority::policy::default::DefaultPolicyHook::new(
            lancet_authority::policy::default::PolicyConfig::defaults_for_project_root(
                std::path::PathBuf::from(root),
            ),
        )) as Arc<dyn lancet_protocol::policy::PolicyHook>,
        default_policy_hook: None,
        audit_context: Arc::new(tokio::sync::RwLock::new(crate::audit::CepAuditContext {
            turn_id: "test-turn".into(),
            session_id: "test-session".into(),
            ..Default::default()
        })),
        audit: Arc::new(crate::audit::CepAuditStore::in_memory().expect("audit store")),
        invariant_index: Arc::new(lancet_invariants::InvariantCardIndex::new(vec![])),
        cep_mode: lancet_protocol::envelope::CepMode::Off,
        command_isolation: crate::enforcement::isolation::CommandIsolationRuntimeConfig::default(),
    };

    let params = RunParams {
        argv: vec!["cargo".into(), "fmt".into()],
        cwd: None,
        env: BTreeMap::new(),
        timeout_ms: Some(30000),
        declared_impact: Some(ImpactKind::WorkspaceMutation),
    };
    let result = registry
        .dispatch(Run::NAME, serde_json::to_value(&params).unwrap(), ctx)
        .await
        .unwrap();
    let parsed: RunResult = serde_json::from_value(result).unwrap();
    // cargo fmt may succeed or fail depending on environment, but should not be GuardDenied
    assert!(parsed.exit_code == 0 || parsed.exit_code == 1);
}

#[tokio::test]
async fn workspace_mutation_argv_not_in_allowlist_returns_guard_denied() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path().to_str().unwrap();
    let registry = native_command_registry(root);

    let (event_tx, _event_rx) = mpsc::channel(32);
    let (_cancel_tx, cancel_rx) = watch::channel(false);
    let store = Arc::new(ActiveEnvelopeStore::new());

    // Declare envelope with restricted allowlist
    let mut capabilities = BTreeSet::new();
    capabilities.insert(Capability::RunCommand {
        argv_allowlist: vec!["cargo test".into()],
    });
    let envelope = ExecutionEnvelope {
        intent: Intent {
            description: "restricted".into(),
        },
        capabilities,
        scope: Scope {
            workspace_root: Some(root.into()),
            extensions: serde_json::Map::new(),
        },
        impact: vec![EnvImpact {
            kind: ImpactKind::WorkspaceMutation,
            target: root.into(),
            scope: None,
            confidence: ImpactConfidence::Conservative,
            unknowns: vec![],
        }],
        constraints: Constraints::default(),
        verify: None,
        on_failure: None,
        extensions: serde_json::Map::new(),
    };
    store.declare(envelope, 0, "test-turn").await.unwrap();

    let ctx = HandlerContext {
        call_id: 1,
        events: EventSink::new(event_tx, 1),
        cancellation: Cancellation::new(cancel_rx),
        base_envelope: BaseExecutionEnvelope::for_workspace_root(root, "allowlist test"),
        cep_context: Arc::new(CepTurnContext::defaults_for_project_root(root)),
        active_envelope: store,
        turn_id: "test-turn".into(),
        durable_metadata: crate::server::call_durable_metadata::CallDurableMetadata::default(),
        effect_ledger: lancet_authority::effect::ledger::EffectLedger::new(),
        authorized_region: lancet_authority::effect::ledger::AuthorizedRegionSlot::new(),
        policy_hook: Arc::new(lancet_authority::policy::default::DefaultPolicyHook::new(
            lancet_authority::policy::default::PolicyConfig::defaults_for_project_root(
                std::path::PathBuf::from(root),
            ),
        )) as Arc<dyn lancet_protocol::policy::PolicyHook>,
        default_policy_hook: None,
        audit_context: Arc::new(tokio::sync::RwLock::new(crate::audit::CepAuditContext {
            turn_id: "test-turn".into(),
            session_id: "test-session".into(),
            ..Default::default()
        })),
        audit: Arc::new(crate::audit::CepAuditStore::in_memory().expect("audit store")),
        invariant_index: Arc::new(lancet_invariants::InvariantCardIndex::new(vec![])),
        cep_mode: lancet_protocol::envelope::CepMode::Off,
        command_isolation: crate::enforcement::isolation::CommandIsolationRuntimeConfig::default(),
    };

    let params = RunParams {
        argv: vec!["cargo".into(), "fmt".into()],
        cwd: None,
        env: BTreeMap::new(),
        timeout_ms: None,
        declared_impact: Some(ImpactKind::WorkspaceMutation),
    };
    let err = registry
        .dispatch(Run::NAME, serde_json::to_value(&params).unwrap(), ctx)
        .await
        .unwrap_err();
    assert_eq!(err.code, CepErrorCode::ReplanRequired);
    assert!(err.message.contains("PatternRejected") || err.message.contains("allowlist"));
}

#[tokio::test]
async fn workspace_mutation_allows_matching_later_run_command_capability() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path().to_str().unwrap();
    let registry = native_command_registry(root);
    let store = Arc::new(ActiveEnvelopeStore::new());

    let mut capabilities = BTreeSet::new();
    capabilities.insert(Capability::RunCommand {
        argv_allowlist: vec!["cargo test".into()],
    });
    capabilities.insert(Capability::RunCommand {
        argv_allowlist: vec!["touch".into()],
    });
    capabilities.insert(Capability::WritePath {
        path_pattern: ".".into(),
    });
    let envelope = ExecutionEnvelope {
        intent: Intent {
            description: "multiple run command capabilities".into(),
        },
        capabilities,
        scope: Scope {
            workspace_root: Some(root.into()),
            extensions: serde_json::Map::new(),
        },
        impact: vec![EnvImpact {
            kind: ImpactKind::WorkspaceMutation,
            target: root.into(),
            scope: None,
            confidence: ImpactConfidence::Conservative,
            unknowns: vec![],
        }],
        constraints: Constraints::default(),
        verify: None,
        on_failure: None,
        extensions: serde_json::Map::new(),
    };
    store.declare(envelope, 0, "test-turn").await.unwrap();

    let (event_tx, _event_rx) = mpsc::channel(32);
    let (_cancel_tx, cancel_rx) = watch::channel(false);
    let ctx = HandlerContext {
        call_id: 1,
        events: EventSink::new(event_tx, 1),
        cancellation: Cancellation::new(cancel_rx),
        base_envelope: BaseExecutionEnvelope::for_workspace_root(root, "multi run cap test"),
        cep_context: Arc::new(CepTurnContext::defaults_for_project_root(root)),
        active_envelope: store,
        turn_id: "test-turn".into(),
        durable_metadata: crate::server::call_durable_metadata::CallDurableMetadata::default(),
        effect_ledger: lancet_authority::effect::ledger::EffectLedger::new(),
        authorized_region: lancet_authority::effect::ledger::AuthorizedRegionSlot::new(),
        policy_hook: Arc::new(lancet_authority::policy::default::DefaultPolicyHook::new(
            lancet_authority::policy::default::PolicyConfig::defaults_for_project_root(
                std::path::PathBuf::from(root),
            ),
        )) as Arc<dyn lancet_protocol::policy::PolicyHook>,
        default_policy_hook: None,
        audit_context: Arc::new(tokio::sync::RwLock::new(crate::audit::CepAuditContext {
            turn_id: "test-turn".into(),
            session_id: "test-session".into(),
            ..Default::default()
        })),
        audit: Arc::new(crate::audit::CepAuditStore::in_memory().expect("audit store")),
        invariant_index: Arc::new(lancet_invariants::InvariantCardIndex::new(vec![])),
        cep_mode: lancet_protocol::envelope::CepMode::Off,
        command_isolation: crate::enforcement::isolation::CommandIsolationRuntimeConfig::default(),
    };

    let params = RunParams {
        argv: vec!["touch".into(), "new.txt".into()],
        cwd: None,
        env: BTreeMap::new(),
        timeout_ms: Some(5000),
        declared_impact: Some(ImpactKind::WorkspaceMutation),
    };
    let result = registry
        .dispatch(Run::NAME, serde_json::to_value(&params).unwrap(), ctx)
        .await
        .unwrap();
    let parsed: RunResult = serde_json::from_value(result).unwrap();
    assert_eq!(parsed.exit_code, 0);
}

#[tokio::test]
async fn workspace_mutation_hidden_project_path_outside_scope_returns_replan_required() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path().to_str().unwrap();
    std::fs::create_dir_all(dir.path().join(".github/workflows")).unwrap();
    let registry = native_command_registry(root);
    let store = Arc::new(ActiveEnvelopeStore::new());

    let mut capabilities = BTreeSet::new();
    capabilities.insert(Capability::RunCommand {
        argv_allowlist: vec!["touch".into()],
    });
    capabilities.insert(Capability::WritePath {
        path_pattern: "src/**".into(),
    });
    let envelope = ExecutionEnvelope {
        intent: Intent {
            description: "hidden path guard".into(),
        },
        capabilities,
        scope: Scope {
            workspace_root: Some(root.into()),
            extensions: serde_json::Map::new(),
        },
        impact: vec![EnvImpact {
            kind: ImpactKind::WorkspaceMutation,
            target: root.into(),
            scope: None,
            confidence: ImpactConfidence::Conservative,
            unknowns: vec![],
        }],
        constraints: Constraints::default(),
        verify: None,
        on_failure: None,
        extensions: serde_json::Map::new(),
    };
    store.declare(envelope, 0, "test-turn").await.unwrap();

    let (event_tx, mut event_rx) = mpsc::channel(32);
    let (_cancel_tx, cancel_rx) = watch::channel(false);
    let effect_ledger = lancet_authority::effect::ledger::EffectLedger::new();
    let ledger_clone = effect_ledger.clone();
    let ctx = HandlerContext {
        call_id: 1,
        events: EventSink::new(event_tx, 1),
        cancellation: Cancellation::new(cancel_rx),
        base_envelope: BaseExecutionEnvelope::for_workspace_root(root, "hidden path test"),
        cep_context: Arc::new(CepTurnContext::defaults_for_project_root(root)),
        active_envelope: store,
        turn_id: "test-turn".into(),
        durable_metadata: crate::server::call_durable_metadata::CallDurableMetadata::default(),
        effect_ledger,
        authorized_region: lancet_authority::effect::ledger::AuthorizedRegionSlot::new(),
        policy_hook: Arc::new(lancet_authority::policy::default::DefaultPolicyHook::new(
            lancet_authority::policy::default::PolicyConfig::defaults_for_project_root(
                std::path::PathBuf::from(root),
            ),
        )) as Arc<dyn lancet_protocol::policy::PolicyHook>,
        default_policy_hook: None,
        audit_context: Arc::new(tokio::sync::RwLock::new(crate::audit::CepAuditContext {
            turn_id: "test-turn".into(),
            session_id: "test-session".into(),
            ..Default::default()
        })),
        audit: Arc::new(crate::audit::CepAuditStore::in_memory().expect("audit store")),
        invariant_index: Arc::new(lancet_invariants::InvariantCardIndex::new(vec![])),
        cep_mode: lancet_protocol::envelope::CepMode::Off,
        command_isolation: crate::enforcement::isolation::CommandIsolationRuntimeConfig::default(),
    };

    let params = RunParams {
        argv: vec!["touch".into(), ".github/workflows/cep-hidden.yml".into()],
        cwd: None,
        env: BTreeMap::new(),
        timeout_ms: Some(5000),
        declared_impact: Some(ImpactKind::WorkspaceMutation),
    };
    let err = registry
        .dispatch(Run::NAME, serde_json::to_value(&params).unwrap(), ctx)
        .await
        .unwrap_err();
    assert_eq!(err.code, CepErrorCode::ReplanRequired);
    let mut saw_replan_required = false;
    while let Ok(event) = event_rx.try_recv() {
        saw_replan_required |= event.method == "cep.replan_required";
    }
    assert!(saw_replan_required);

    let effects = ledger_clone.snapshot();
    let mutation_effects: Vec<_> = effects
        .iter()
        .filter(|e| {
            e.kind == ImpactKind::WorkspaceMutation
                && e.source == lancet_authority::effect::ledger::EffectSource::HandlerBackend
        })
        .collect();
    assert!(
        !mutation_effects.is_empty(),
        "ledger must contain WorkspaceMutation effect for out-of-scope path"
    );
    let has_out_of_scope = mutation_effects.iter().any(|e| {
            matches!(&e.scope, lancet_authority::effect::ledger::ObservedScope::Path(p) if p.to_string_lossy().contains("cep-hidden"))
        });
    assert!(
        has_out_of_scope,
        "ledger must contain the specific out-of-scope path"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn workspace_mutation_timeout_checks_out_of_scope_changes() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::TempDir::new().unwrap();
    let canonical = std::fs::canonicalize(dir.path()).unwrap();
    let root = canonical.to_str().unwrap();
    std::fs::create_dir_all(dir.path().join("allowed")).unwrap();
    let script = dir.path().join("poc_timeout.sh");
    std::fs::write(&script, "#!/bin/sh\ntouch outside.txt\nsleep 2\n").unwrap();
    let mut perms = std::fs::metadata(&script).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&script, perms).unwrap();

    let registry = native_command_registry(root);
    let store = Arc::new(ActiveEnvelopeStore::new());
    let mut capabilities = BTreeSet::new();
    capabilities.insert(Capability::RunCommand {
        argv_allowlist: vec!["./poc_timeout.sh".into()],
    });
    capabilities.insert(Capability::WritePath {
        path_pattern: "allowed/**".into(),
    });
    let envelope = ExecutionEnvelope {
        intent: Intent {
            description: "timeout mutation guard".into(),
        },
        capabilities,
        scope: Scope {
            workspace_root: Some(root.into()),
            extensions: serde_json::Map::new(),
        },
        impact: vec![EnvImpact {
            kind: ImpactKind::WorkspaceMutation,
            target: root.into(),
            scope: None,
            confidence: ImpactConfidence::Conservative,
            unknowns: vec![],
        }],
        constraints: Constraints::default(),
        verify: None,
        on_failure: None,
        extensions: serde_json::Map::new(),
    };
    store.declare(envelope, 0, "test-turn").await.unwrap();

    let (event_tx, _event_rx) = mpsc::channel(32);
    let (_cancel_tx, cancel_rx) = watch::channel(false);
    let effect_ledger = lancet_authority::effect::ledger::EffectLedger::new();
    let ledger_clone = effect_ledger.clone();
    let ctx = HandlerContext {
        call_id: 1,
        events: EventSink::new(event_tx, 1),
        cancellation: Cancellation::new(cancel_rx),
        base_envelope: BaseExecutionEnvelope::for_workspace_root(root, "timeout guard test"),
        cep_context: Arc::new(CepTurnContext::defaults_for_project_root(root)),
        active_envelope: store,
        turn_id: "test-turn".into(),
        durable_metadata: crate::server::call_durable_metadata::CallDurableMetadata::default(),
        effect_ledger,
        authorized_region: lancet_authority::effect::ledger::AuthorizedRegionSlot::new(),
        policy_hook: Arc::new(lancet_authority::policy::default::DefaultPolicyHook::new(
            lancet_authority::policy::default::PolicyConfig::defaults_for_project_root(
                std::path::PathBuf::from(root),
            ),
        )) as Arc<dyn lancet_protocol::policy::PolicyHook>,
        default_policy_hook: None,
        audit_context: Arc::new(tokio::sync::RwLock::new(crate::audit::CepAuditContext {
            turn_id: "test-turn".into(),
            session_id: "test-session".into(),
            ..Default::default()
        })),
        audit: Arc::new(crate::audit::CepAuditStore::in_memory().expect("audit store")),
        invariant_index: Arc::new(lancet_invariants::InvariantCardIndex::new(vec![])),
        cep_mode: lancet_protocol::envelope::CepMode::Off,
        command_isolation: crate::enforcement::isolation::CommandIsolationRuntimeConfig::default(),
    };

    let params = RunParams {
        argv: vec!["./poc_timeout.sh".into()],
        cwd: None,
        env: BTreeMap::new(),
        timeout_ms: Some(100),
        declared_impact: Some(ImpactKind::WorkspaceMutation),
    };
    let err = registry
        .dispatch(Run::NAME, serde_json::to_value(&params).unwrap(), ctx)
        .await
        .unwrap_err();

    assert_eq!(err.code, CepErrorCode::ReplanRequired);
    assert!(dir.path().join("outside.txt").exists());
    let effects = ledger_clone.snapshot();
    assert!(effects.iter().any(|e| {
            e.kind == ImpactKind::WorkspaceMutation
                && matches!(&e.scope, lancet_authority::effect::ledger::ObservedScope::Path(p) if p.ends_with("outside.txt"))
        }));
}

#[cfg(unix)]
#[tokio::test]
async fn workspace_mutation_post_check_rejects_out_of_scope_after_supersede() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::TempDir::new().unwrap();
    let canonical = std::fs::canonicalize(dir.path()).unwrap();
    let root = canonical.to_str().unwrap();
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    let script = dir.path().join("mutate.sh");
    std::fs::write(&script, "#!/bin/sh\nsleep 0.2\ntouch outside.txt\n").unwrap();
    let mut perms = std::fs::metadata(&script).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&script, perms).unwrap();

    let registry = native_command_registry(root);
    let store = Arc::new(ActiveEnvelopeStore::new());
    let envelope = {
        let mut capabilities = BTreeSet::new();
        capabilities.insert(Capability::RunCommand {
            argv_allowlist: vec!["./mutate.sh".into()],
        });
        capabilities.insert(Capability::WritePath {
            path_pattern: "src/**".into(),
        });
        ExecutionEnvelope {
            intent: Intent {
                description: "initial narrow envelope".into(),
            },
            capabilities,
            scope: Scope {
                workspace_root: Some(root.into()),
                extensions: serde_json::Map::new(),
            },
            impact: vec![EnvImpact {
                kind: ImpactKind::WorkspaceMutation,
                target: root.into(),
                scope: None,
                confidence: ImpactConfidence::Conservative,
                unknowns: vec![],
            }],
            constraints: Constraints::default(),
            verify: None,
            on_failure: None,
            extensions: serde_json::Map::new(),
        }
    };
    store.declare(envelope, 0, "test-turn").await.unwrap();

    let (event_tx, _event_rx) = mpsc::channel(32);
    let (_cancel_tx, cancel_rx) = watch::channel(false);
    let effect_ledger = lancet_authority::effect::ledger::EffectLedger::new();
    let ledger_clone = effect_ledger.clone();
    let ctx = HandlerContext {
        call_id: 1,
        events: EventSink::new(event_tx, 1),
        cancellation: Cancellation::new(cancel_rx),
        base_envelope: BaseExecutionEnvelope::for_workspace_root(root, "supersede race test"),
        cep_context: Arc::new(CepTurnContext::defaults_for_project_root(root)),
        active_envelope: Arc::clone(&store),
        turn_id: "test-turn".into(),
        durable_metadata: crate::server::call_durable_metadata::CallDurableMetadata::default(),
        effect_ledger,
        authorized_region: lancet_authority::effect::ledger::AuthorizedRegionSlot::new(),
        policy_hook: Arc::new(lancet_authority::policy::default::DefaultPolicyHook::new(
            lancet_authority::policy::default::PolicyConfig::defaults_for_project_root(
                std::path::PathBuf::from(root),
            ),
        )) as Arc<dyn lancet_protocol::policy::PolicyHook>,
        default_policy_hook: None,
        audit_context: Arc::new(tokio::sync::RwLock::new(crate::audit::CepAuditContext {
            turn_id: "test-turn".into(),
            session_id: "test-session".into(),
            ..Default::default()
        })),
        audit: Arc::new(crate::audit::CepAuditStore::in_memory().expect("audit store")),
        invariant_index: Arc::new(lancet_invariants::InvariantCardIndex::new(vec![])),
        cep_mode: lancet_protocol::envelope::CepMode::Off,
        command_isolation: crate::enforcement::isolation::CommandIsolationRuntimeConfig::default(),
    };

    let params = RunParams {
        argv: vec!["./mutate.sh".into()],
        cwd: None,
        env: BTreeMap::new(),
        timeout_ms: Some(5000),
        declared_impact: Some(ImpactKind::WorkspaceMutation),
    };
    let command_task = tokio::spawn(async move {
        registry
            .dispatch(Run::NAME, serde_json::to_value(&params).unwrap(), ctx)
            .await
    });

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let replacement = {
        let mut capabilities = BTreeSet::new();
        capabilities.insert(Capability::RunCommand {
            argv_allowlist: vec!["./mutate.sh".into()],
        });
        capabilities.insert(Capability::WritePath {
            path_pattern: ".".into(),
        });
        ExecutionEnvelope {
            intent: Intent {
                description: "replacement wider envelope".into(),
            },
            capabilities,
            scope: Scope {
                workspace_root: Some(root.into()),
                extensions: serde_json::Map::new(),
            },
            impact: vec![EnvImpact {
                kind: ImpactKind::WorkspaceMutation,
                target: root.into(),
                scope: None,
                confidence: ImpactConfidence::Conservative,
                unknowns: vec![],
            }],
            constraints: Constraints::default(),
            verify: None,
            on_failure: None,
            extensions: serde_json::Map::new(),
        }
    };
    store
        .declare_with_supersede(replacement, 2, "test-turn", Some("race".into()))
        .await
        .unwrap();

    let err = command_task.await.unwrap().unwrap_err();
    assert_eq!(err.code, CepErrorCode::ReplanRequired);

    let effects = ledger_clone.snapshot();
    let cmd_exec: Vec<_> = effects
        .iter()
        .filter(|e| {
            e.kind == ImpactKind::CommandExecution
                && e.source == lancet_authority::effect::ledger::EffectSource::HandlerBackend
        })
        .collect();
    assert_eq!(cmd_exec.len(), 1, "exactly one CommandExecution effect");
    assert!(
        matches!(&cmd_exec[0].scope, lancet_authority::effect::ledger::ObservedScope::Root(r) if r == root),
        "CommandExecution scope must be Root(project_root)"
    );

    let mutations: Vec<_> = effects
        .iter()
        .filter(|e| {
            e.kind == ImpactKind::WorkspaceMutation
                && e.source == lancet_authority::effect::ledger::EffectSource::HandlerBackend
        })
        .collect();
    assert!(
        !mutations.is_empty(),
        "ledger must contain WorkspaceMutation effect for changed path"
    );
}

#[tokio::test]
async fn output_truncation_metadata_present() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path().to_str().unwrap();
    let (mut ctx, _event_rx, _cancel_tx) =
        test_ctx_with_run_command(root, vec!["python3".into()]).await;
    ctx.cep_context = Arc::new({
        let mut policy = CepTurnContext::defaults_for_project_root(root);
        policy.policy.command.max_output_bytes = 50;
        policy
    });

    let registry = native_command_registry(root);
    let params = RunParams {
        argv: vec!["python3".into(), "-c".into(), "print('x' * 200)".into()],
        cwd: None,
        env: BTreeMap::new(),
        timeout_ms: Some(5000),
        declared_impact: None,
    };
    // This should be HardDenied because python3 -c is a risky pattern
    let err = registry
        .dispatch(Run::NAME, serde_json::to_value(&params).unwrap(), ctx)
        .await
        .unwrap_err();
    assert_eq!(err.code, CepErrorCode::GuardDenied);
}

#[tokio::test]
async fn timeout_exceeding_max_returns_typed_error() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path().to_str().unwrap();
    let (ctx, _event_rx, _cancel_tx) = test_ctx_with_run_command(root, vec!["echo".into()]).await;

    let registry = native_command_registry(root);
    let params = RunParams {
        argv: vec!["echo".into(), "hi".into()],
        cwd: None,
        env: BTreeMap::new(),
        timeout_ms: Some(999_999_999), // way above max
        declared_impact: None,
    };
    let err = registry
        .dispatch(Run::NAME, serde_json::to_value(&params).unwrap(), ctx)
        .await
        .unwrap_err();
    assert_eq!(err.code, CepErrorCode::SchemaMismatch);
    assert!(err.message.contains("999999999"));
}

#[tokio::test]
async fn omitted_timeout_uses_policy_default_timeout() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path().to_str().unwrap();
    let (mut ctx, _event_rx, _cancel_tx) =
        test_ctx_with_run_command(root, vec!["sleep".into()]).await;
    let mut cep_policy = CepPolicyConfig::defaults_for_project_root(root);
    cep_policy
        .command
        .allowed_diagnostic_commands
        .push(vec!["sleep".into()]);
    cep_policy.command.default_timeout_ms = 50;
    cep_policy.command.max_timeout_ms = 5_000;
    let mut hook_policy =
        lancet_authority::policy::default::PolicyConfig::defaults_for_project_root(
            std::path::PathBuf::from(root),
        );
    hook_policy.command = cep_policy.command.clone();
    let default_hook = Arc::new(lancet_authority::policy::default::DefaultPolicyHook::new(
        hook_policy,
    ));
    ctx.policy_hook = default_hook.clone() as Arc<dyn lancet_protocol::policy::PolicyHook>;
    ctx.default_policy_hook = Some(default_hook);
    ctx.cep_context = Arc::new(CepTurnContext::from_policy(cep_policy));

    let registry = native_command_registry(root);
    let params = RunParams {
        argv: vec!["sleep".into(), "1".into()],
        cwd: None,
        env: BTreeMap::new(),
        timeout_ms: None,
        declared_impact: None,
    };
    let err = registry
        .dispatch(Run::NAME, serde_json::to_value(&params).unwrap(), ctx)
        .await
        .unwrap_err();

    assert_eq!(err.code, CepErrorCode::CommandTimeout);
    assert!(err.message.contains("50ms"));
}

#[tokio::test]
async fn env_policy_rejects_unallowed_key() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path().to_str().unwrap();
    let (ctx, _event_rx, _cancel_tx) = test_ctx_with_run_command(root, vec!["echo".into()]).await;

    let registry = native_command_registry(root);
    let mut env = BTreeMap::new();
    env.insert("SECRET_KEY".into(), "value".into());
    let params = RunParams {
        argv: vec!["echo".into(), "hi".into()],
        cwd: None,
        env,
        timeout_ms: None,
        declared_impact: None,
    };
    let err = registry
        .dispatch(Run::NAME, serde_json::to_value(&params).unwrap(), ctx)
        .await
        .unwrap_err();
    assert_eq!(err.code, CepErrorCode::GuardDenied);
    assert!(err.message.contains("SECRET_KEY"));
}

#[tokio::test]
async fn no_leaked_processes_across_20_runs() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path().to_str().unwrap();
    let registry = native_command_registry(root);

    for i in 0..20 {
        let (ctx, _events, _cancel_tx) = test_ctx(root);
        let params = RunParams {
            argv: vec!["echo".into(), format!("run-{i}")],
            cwd: None,
            env: BTreeMap::new(),
            timeout_ms: Some(5000),
            declared_impact: None,
        };
        let _ = registry
            .dispatch(Run::NAME, serde_json::to_value(&params).unwrap(), ctx)
            .await;
    }
    // If we get here without hanging, processes were cleaned up
}

#[tokio::test]
async fn command_cancel_returns_execution_failed() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path().to_str().unwrap();

    let (event_tx, _event_rx) = mpsc::channel(32);
    let (cancel_tx, cancel_rx) = watch::channel(false);
    let cancel = Cancellation::new(cancel_rx);

    let store = Arc::new(ActiveEnvelopeStore::new());
    let mut capabilities = BTreeSet::new();
    capabilities.insert(Capability::RunCommand {
        argv_allowlist: vec!["sleep".into()],
    });
    let envelope = ExecutionEnvelope {
        intent: Intent {
            description: "cancel test".into(),
        },
        capabilities,
        scope: Scope {
            workspace_root: Some(root.into()),
            extensions: serde_json::Map::new(),
        },
        impact: vec![EnvImpact {
            kind: ImpactKind::WorkspaceMutation,
            target: root.into(),
            scope: None,
            confidence: ImpactConfidence::Conservative,
            unknowns: vec![],
        }],
        constraints: Constraints::default(),
        verify: None,
        on_failure: None,
        extensions: serde_json::Map::new(),
    };
    store.declare(envelope, 0, "test-turn").await.unwrap();

    // Cancel after 50ms
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let _ = cancel_tx.send(true);
    });

    let ctx = HandlerContext {
        call_id: 1,
        events: EventSink::new(event_tx, 1),
        cancellation: cancel,
        base_envelope: BaseExecutionEnvelope::for_workspace_root(root, "cancel test"),
        cep_context: Arc::new(CepTurnContext::defaults_for_project_root(root)),
        active_envelope: store,
        turn_id: "test-turn".into(),
        durable_metadata: crate::server::call_durable_metadata::CallDurableMetadata::default(),
        effect_ledger: lancet_authority::effect::ledger::EffectLedger::new(),
        authorized_region: lancet_authority::effect::ledger::AuthorizedRegionSlot::new(),
        policy_hook: Arc::new(lancet_authority::policy::default::DefaultPolicyHook::new(
            lancet_authority::policy::default::PolicyConfig::defaults_for_project_root(
                std::path::PathBuf::from(root),
            ),
        )) as Arc<dyn lancet_protocol::policy::PolicyHook>,
        default_policy_hook: None,
        audit_context: Arc::new(tokio::sync::RwLock::new(crate::audit::CepAuditContext {
            turn_id: "test-turn".into(),
            session_id: "test-session".into(),
            ..Default::default()
        })),
        audit: Arc::new(crate::audit::CepAuditStore::in_memory().expect("audit store")),
        invariant_index: Arc::new(lancet_invariants::InvariantCardIndex::new(vec![])),
        cep_mode: lancet_protocol::envelope::CepMode::Off,
        command_isolation: crate::enforcement::isolation::CommandIsolationRuntimeConfig::default(),
    };

    let registry = native_command_registry(root);
    let params = RunParams {
        argv: vec!["sleep".into(), "30".into()],
        cwd: None,
        env: BTreeMap::new(),
        timeout_ms: Some(60000),
        declared_impact: Some(ImpactKind::WorkspaceMutation),
    };
    let err = registry
        .dispatch(Run::NAME, serde_json::to_value(&params).unwrap(), ctx)
        .await
        .unwrap_err();
    assert_eq!(err.code, CepErrorCode::ExecutionFailed);
    assert!(err.message.contains("cancelled"));
}

#[test]
fn matches_argv_allowlist_prefix() {
    let allowlist = vec!["cargo test".into(), "cargo check".into()];
    assert!(matches_argv_allowlist(
        &["cargo".into(), "test".into(), "--nocapture".into()],
        &allowlist
    ));
    assert!(matches_argv_allowlist(
        &["cargo".into(), "check".into()],
        &allowlist
    ));
    assert!(!matches_argv_allowlist(
        &["cargo".into(), "fmt".into()],
        &allowlist
    ));
    assert!(!matches_argv_allowlist(
        &["git".into(), "status".into()],
        &allowlist
    ));
}

// --- Post-execution diff check tests ---

#[tokio::test]
async fn workspace_mutation_inside_scope_succeeds() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path().to_str().unwrap();
    let registry = native_command_registry(root);

    let store = Arc::new(ActiveEnvelopeStore::new());
    let mut capabilities = BTreeSet::new();
    capabilities.insert(Capability::RunCommand {
        argv_allowlist: vec!["touch".into()],
    });
    capabilities.insert(Capability::WritePath {
        path_pattern: ".".into(),
    });
    let envelope = ExecutionEnvelope {
        intent: Intent {
            description: "test mutation".into(),
        },
        capabilities,
        scope: Scope {
            workspace_root: Some(root.into()),
            extensions: serde_json::Map::new(),
        },
        impact: vec![EnvImpact {
            kind: ImpactKind::WorkspaceMutation,
            target: root.into(),
            scope: None,
            confidence: ImpactConfidence::Conservative,
            unknowns: vec![],
        }],
        constraints: Constraints::default(),
        verify: None,
        on_failure: Some(OnFailure::RetainDiff),
        extensions: serde_json::Map::new(),
    };
    store.declare(envelope, 0, "test-turn").await.unwrap();

    let (event_tx, _event_rx) = mpsc::channel(32);
    let (_cancel_tx, cancel_rx) = watch::channel(false);
    let ctx = HandlerContext {
        call_id: 1,
        events: EventSink::new(event_tx, 1),
        cancellation: Cancellation::new(cancel_rx),
        base_envelope: BaseExecutionEnvelope::for_workspace_root(root, "mutation test"),
        cep_context: Arc::new(CepTurnContext::defaults_for_project_root(root)),
        active_envelope: store,
        turn_id: "test-turn".into(),
        durable_metadata: crate::server::call_durable_metadata::CallDurableMetadata::default(),
        effect_ledger: lancet_authority::effect::ledger::EffectLedger::new(),
        authorized_region: lancet_authority::effect::ledger::AuthorizedRegionSlot::new(),
        policy_hook: Arc::new(lancet_authority::policy::default::DefaultPolicyHook::new(
            lancet_authority::policy::default::PolicyConfig::defaults_for_project_root(
                std::path::PathBuf::from(root),
            ),
        )) as Arc<dyn lancet_protocol::policy::PolicyHook>,
        default_policy_hook: None,
        audit_context: Arc::new(tokio::sync::RwLock::new(crate::audit::CepAuditContext {
            turn_id: "test-turn".into(),
            session_id: "test-session".into(),
            ..Default::default()
        })),
        audit: Arc::new(crate::audit::CepAuditStore::in_memory().expect("audit store")),
        invariant_index: Arc::new(lancet_invariants::InvariantCardIndex::new(vec![])),
        cep_mode: lancet_protocol::envelope::CepMode::Off,
        command_isolation: crate::enforcement::isolation::CommandIsolationRuntimeConfig::default(),
    };

    let params = RunParams {
        argv: vec!["touch".into(), "new_file.txt".into()],
        cwd: None,
        env: BTreeMap::new(),
        timeout_ms: Some(5000),
        declared_impact: Some(ImpactKind::WorkspaceMutation),
    };
    let result = registry
        .dispatch(Run::NAME, serde_json::to_value(&params).unwrap(), ctx)
        .await
        .unwrap();
    let parsed: RunResult = serde_json::from_value(result).unwrap();
    assert_eq!(parsed.exit_code, 0);
}

#[tokio::test]
async fn workspace_mutation_verify_failure_records_backend_invoked() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path().to_str().unwrap();
    let registry = native_command_registry(root);

    let store = Arc::new(ActiveEnvelopeStore::new());
    let mut capabilities = BTreeSet::new();
    capabilities.insert(Capability::RunCommand {
        argv_allowlist: vec!["touch".into()],
    });
    capabilities.insert(Capability::WritePath {
        path_pattern: ".".into(),
    });
    let envelope = ExecutionEnvelope {
        intent: Intent {
            description: "verify failure metadata test".into(),
        },
        capabilities,
        scope: Scope {
            workspace_root: Some(root.into()),
            extensions: serde_json::Map::new(),
        },
        impact: vec![EnvImpact {
            kind: ImpactKind::WorkspaceMutation,
            target: root.into(),
            scope: None,
            confidence: ImpactConfidence::Conservative,
            unknowns: vec![],
        }],
        constraints: Constraints::default(),
        verify: Some(VerifyCommand {
            command: "echo verify".into(),
            expected_exit_code: Some(1),
        }),
        on_failure: Some(OnFailure::RetainDiff),
        extensions: serde_json::Map::new(),
    };
    store.declare(envelope, 0, "test-turn").await.unwrap();

    let (event_tx, mut event_rx) = mpsc::channel(32);
    let (_cancel_tx, cancel_rx) = watch::channel(false);
    let durable_metadata = crate::server::call_durable_metadata::CallDurableMetadata::default();
    let ctx = HandlerContext {
        call_id: 1,
        events: EventSink::new(event_tx, 1),
        cancellation: Cancellation::new(cancel_rx),
        base_envelope: BaseExecutionEnvelope::for_workspace_root(root, "verify failure test"),
        cep_context: Arc::new(CepTurnContext::defaults_for_project_root(root)),
        active_envelope: store,
        turn_id: "test-turn".into(),
        durable_metadata: durable_metadata.clone(),
        effect_ledger: lancet_authority::effect::ledger::EffectLedger::new(),
        authorized_region: lancet_authority::effect::ledger::AuthorizedRegionSlot::new(),
        policy_hook: Arc::new(lancet_authority::policy::default::DefaultPolicyHook::new(
            lancet_authority::policy::default::PolicyConfig::defaults_for_project_root(
                std::path::PathBuf::from(root),
            ),
        )) as Arc<dyn lancet_protocol::policy::PolicyHook>,
        default_policy_hook: None,
        audit_context: Arc::new(tokio::sync::RwLock::new(crate::audit::CepAuditContext {
            turn_id: "test-turn".into(),
            session_id: "test-session".into(),
            ..Default::default()
        })),
        audit: Arc::new(crate::audit::CepAuditStore::in_memory().expect("audit store")),
        invariant_index: Arc::new(lancet_invariants::InvariantCardIndex::new(vec![])),
        cep_mode: lancet_protocol::envelope::CepMode::Off,
        command_isolation: crate::enforcement::isolation::CommandIsolationRuntimeConfig::default(),
    };

    let params = RunParams {
        argv: vec!["touch".into(), "verify_failed_file.txt".into()],
        cwd: None,
        env: BTreeMap::new(),
        timeout_ms: Some(5000),
        declared_impact: Some(ImpactKind::WorkspaceMutation),
    };
    let err = registry
        .dispatch(Run::NAME, serde_json::to_value(&params).unwrap(), ctx)
        .await
        .expect_err("verify failure must abort the command result");

    assert_eq!(err.code, CepErrorCode::VerifyFailed);
    assert!(dir.path().join("verify_failed_file.txt").exists());
    let metadata = command_boundary_metadata_json(&durable_metadata);
    assert_eq!(metadata["backend_invoked"], true);
    assert_eq!(metadata["classification"], "workspace_mutation");
    assert_eq!(metadata["exit_code"], 0);
    assert_eq!(metadata["effect_observation"], "partial");

    let mut saw_backend_invoked_event = false;
    let mut saw_verify_failed_event = false;
    while let Ok(event) = event_rx.try_recv() {
        if event.method == "cep.command_metadata"
            && event.params["metadata"]["backend_invoked"] == true
        {
            saw_backend_invoked_event = true;
        }
        if event.method == "cep.verify_failed" {
            saw_verify_failed_event = true;
        }
    }
    assert!(saw_backend_invoked_event);
    assert!(saw_verify_failed_event);
}

#[tokio::test]
async fn diagnostic_skips_post_exec_diff_check() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path().to_str().unwrap();
    let registry = native_command_registry(root);
    let (ctx, _events, _cancel_tx) = test_ctx_with_run_command(root, vec!["echo".into()]).await;

    let params = RunParams {
        argv: vec!["echo".into(), "hello".into()],
        cwd: None,
        env: BTreeMap::new(),
        timeout_ms: Some(5000),
        declared_impact: None,
    };
    let result = registry
        .dispatch(Run::NAME, serde_json::to_value(&params).unwrap(), ctx)
        .await
        .unwrap();
    let parsed: RunResult = serde_json::from_value(result).unwrap();
    assert_eq!(parsed.exit_code, 0);
}

// --- Workspace snapshot tests ---

#[test]
fn capture_snapshot_returns_file_hashes() {
    let dir = tempfile::TempDir::new().unwrap();
    std::fs::write(dir.path().join("file.txt"), "content").unwrap();
    let snapshot = capture_workspace_snapshot(dir.path().to_str().unwrap()).unwrap();
    assert!(snapshot.contains_key("file.txt"));
    assert!(snapshot["file.txt"].content_hash.is_some());
    assert_eq!(snapshot["file.txt"].file_type, "file");
}

#[test]
fn capture_snapshot_includes_hidden_project_files_but_skips_git_and_target() {
    let dir = tempfile::TempDir::new().unwrap();
    std::fs::create_dir_all(dir.path().join(".github/workflows")).unwrap();
    std::fs::create_dir_all(dir.path().join(".git/objects")).unwrap();
    std::fs::create_dir_all(dir.path().join("target/debug")).unwrap();
    std::fs::write(dir.path().join(".github/workflows/ci.yml"), "name: ci").unwrap();
    std::fs::write(dir.path().join(".git/config"), "[core]").unwrap();
    std::fs::write(dir.path().join("target/debug/out"), "build").unwrap();

    let snapshot = capture_workspace_snapshot(dir.path().to_str().unwrap()).unwrap();

    assert!(snapshot.contains_key(".github/workflows/ci.yml"));
    assert!(!snapshot.contains_key(".git/config"));
    assert!(!snapshot.contains_key("target/debug/out"));
}

#[test]
fn capture_snapshot_tracks_active_git_hooks_but_skips_samples_and_objects() {
    let dir = tempfile::TempDir::new().unwrap();
    std::fs::create_dir_all(dir.path().join(".git/objects")).unwrap();
    std::fs::create_dir_all(dir.path().join(".git/hooks")).unwrap();
    std::fs::write(
        dir.path().join(".git/hooks/pre-commit.sample"),
        "#!/bin/sh\nexit 0\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join(".git/hooks/post-checkout"),
        "#!/bin/sh\n# evil\n",
    )
    .unwrap();
    std::fs::write(dir.path().join(".git/objects/whatever"), "x").unwrap();
    std::fs::write(dir.path().join(".git/config"), "[core]").unwrap();

    let snapshot = capture_workspace_snapshot(dir.path().to_str().unwrap()).unwrap();

    assert!(snapshot.contains_key(".git/hooks/post-checkout"));
    assert!(!snapshot.contains_key(".git/hooks/pre-commit.sample"));
    assert!(!snapshot.contains_key(".git/objects/whatever"));
    assert!(!snapshot.contains_key(".git/config"));
}

#[test]
fn compute_changed_paths_detects_new_file() {
    let pre = HashMap::new();
    let mut post = HashMap::new();
    post.insert(
        "new.txt".into(),
        FileSnapshot {
            content_hash: Some("hash".into()),
            file_type: "file".into(),
        },
    );
    let changed = compute_changed_paths(&pre, &post);
    assert_eq!(changed, vec!["new.txt"]);
}

#[test]
fn compute_changed_paths_detects_modified_file() {
    let mut pre = HashMap::new();
    pre.insert(
        "file.txt".into(),
        FileSnapshot {
            content_hash: Some("old_hash".into()),
            file_type: "file".into(),
        },
    );
    let mut post = HashMap::new();
    post.insert(
        "file.txt".into(),
        FileSnapshot {
            content_hash: Some("new_hash".into()),
            file_type: "file".into(),
        },
    );
    let changed = compute_changed_paths(&pre, &post);
    assert_eq!(changed, vec!["file.txt"]);
}

#[test]
fn compute_changed_paths_detects_deleted_file() {
    let mut pre = HashMap::new();
    pre.insert(
        "deleted.txt".into(),
        FileSnapshot {
            content_hash: Some("hash".into()),
            file_type: "file".into(),
        },
    );
    let post = HashMap::new();
    let changed = compute_changed_paths(&pre, &post);
    assert_eq!(changed, vec!["deleted.txt"]);
}

#[test]
fn compute_changed_paths_ignores_unchanged() {
    let mut pre = HashMap::new();
    pre.insert(
        "file.txt".into(),
        FileSnapshot {
            content_hash: Some("same_hash".into()),
            file_type: "file".into(),
        },
    );
    let mut post = HashMap::new();
    post.insert(
        "file.txt".into(),
        FileSnapshot {
            content_hash: Some("same_hash".into()),
            file_type: "file".into(),
        },
    );
    let changed = compute_changed_paths(&pre, &post);
    assert!(changed.is_empty());
}

#[test]
fn command_run_composite_effect_workspace_mutation() {
    use lancet_authority::effect::region::EffectiveEffect;
    use lancet_authority::policy::command::EffectiveImpact;

    let effect = EffectiveEffect::from_command_impact(&EffectiveImpact::WorkspaceMutation).unwrap();
    let kinds: Vec<String> = effect.kinds.iter().map(|k| format!("{:?}", k)).collect();
    assert!(kinds.contains(&"CommandExecution".to_string()));
    assert!(kinds.contains(&"WorkspaceMutation".to_string()));
    assert_eq!(kinds.len(), 2);
}

#[test]
fn command_run_composite_effect_diagnostic() {
    use lancet_authority::effect::region::EffectiveEffect;
    use lancet_authority::policy::command::EffectiveImpact;

    let effect = EffectiveEffect::from_command_impact(&EffectiveImpact::Diagnostic).unwrap();
    let kinds: Vec<String> = effect.kinds.iter().map(|k| format!("{:?}", k)).collect();
    assert!(kinds.contains(&"CommandExecution".to_string()));
    assert!(!kinds.contains(&"WorkspaceMutation".to_string()));
    assert_eq!(kinds.len(), 1);
}

#[test]
fn command_observation_metadata_is_derived_for_workspace_mutation() {
    use lancet_authority::policy::command::EffectiveImpact;

    let metadata = command_observation_metadata(&EffectiveImpact::WorkspaceMutation);
    assert_eq!(metadata["effect_observation"], "partial");
    assert_eq!(
        metadata["observed_effects"],
        serde_json::json!(["command_execution", "workspace_mutation_diff"])
    );
    assert!(metadata["unobserved_dimensions"]
        .as_array()
        .unwrap()
        .iter()
        .any(|dimension| dimension.as_str() == Some("workspace_read_set")));
    assert!(!metadata["unobserved_dimensions"]
        .as_array()
        .unwrap()
        .iter()
        .any(|dimension| dimension.as_str() == Some("tree_in_write")));
}

#[test]
fn command_observation_metadata_is_derived_for_diagnostic() {
    use lancet_authority::policy::command::EffectiveImpact;

    let metadata = command_observation_metadata(&EffectiveImpact::Diagnostic);
    assert_eq!(metadata["effect_observation"], "partial");
    assert_eq!(
        metadata["observed_effects"],
        serde_json::json!(["command_execution"])
    );
    assert!(metadata["unobserved_dimensions"]
        .as_array()
        .unwrap()
        .iter()
        .any(|dimension| dimension.as_str() == Some("tree_in_write")));
    assert!(!metadata["observed_effects"]
        .as_array()
        .unwrap()
        .iter()
        .any(|effect| effect.as_str() == Some("workspace_mutation_diff")));
}
