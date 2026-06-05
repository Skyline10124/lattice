// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

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

static TEST_ARTIFACT_VERIFY_WRITE_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

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
async fn command_boundary_enforce_unknown_required_probe_fails_closed_after_setup_attempt() {
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
    assert_eq!(metadata["boundary_setup_failed_closed"], false);
    assert_eq!(
        metadata["host_probe_summary"]["primitives"]["network_namespace"]["status"],
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
    assert_eq!(metadata["boundary_setup_failed_closed"], true);
    assert_eq!(
        metadata["effective_boundary"]["network"]["status"],
        "unbounded"
    );
    assert!(
        metadata["effective_boundary"]["network"]["diagnostics"]["setup_failure_stage"]
            .as_str()
            .is_some()
    );
}

#[tokio::test]
async fn command_boundary_audit_available_required_runs_with_live_setup_fallback_unbounded() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path().to_str().unwrap();
    let registry = native_command_registry(root);
    let (mut ctx, _events, _cancel_tx) = test_ctx_with_run_command(root, vec!["echo".into()]).await;
    ctx.command_isolation = command_boundary_config(
        crate::enforcement::isolation::CommandIsolationMode::Audit,
        vec![lancet_protocol::BoundaryDimension::Network],
        command_boundary_probe(&[(
            lancet_protocol::BoundaryPrimitive::NetworkNamespace,
            lancet_protocol::HostProbeStatus::Available,
            "netns available in probe",
        )]),
    );
    ctx.command_isolation
        .read_only_toolchain_paths
        .push(std::path::PathBuf::from("/usr/bin"));
    let durable = ctx.durable_metadata.clone();

    let value = run_echo_with_context(&registry, ctx)
        .await
        .expect("audit mode may execute with honest unbounded labels");
    let parsed: RunResult = serde_json::from_value(value).unwrap();

    assert_eq!(parsed.exit_code, 0);
    assert!(parsed.stdout.contains("command-boundary"));
    let metadata = command_boundary_metadata_json(&durable);
    assert_eq!(metadata["backend_invoked"], true);
    assert_eq!(metadata["boundary_setup_failed_closed"], true);
    assert_eq!(
        metadata["effective_boundary"]["network"]["status"],
        "unbounded"
    );
    assert_eq!(
        metadata["effective_boundary"]["network"]["diagnostics"]["supervisor_backend"],
        "native_linux"
    );
    assert!(
        metadata["effective_boundary"]["network"]["diagnostics"]["setup_failure_stage"]
            .as_str()
            .is_some_and(|stage| stage != "pre_exec_setup")
    );
    assert!(
        metadata["effective_boundary"]["network"]["source_primitives"]
            .as_array()
            .is_none_or(|primitives| primitives.is_empty())
    );
}

#[tokio::test]
async fn command_boundary_enforce_required_network_uses_supervisor_attempt_before_backend() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path().to_str().unwrap();
    let registry = native_command_registry(root);
    let (mut ctx, _events, _cancel_tx) = test_ctx_with_run_command(root, vec!["echo".into()]).await;
    ctx.command_isolation = command_boundary_config(
        crate::enforcement::isolation::CommandIsolationMode::Enforce,
        vec![lancet_protocol::BoundaryDimension::Network],
        command_boundary_probe(&[(
            lancet_protocol::BoundaryPrimitive::NetworkNamespace,
            lancet_protocol::HostProbeStatus::Available,
            "netns available in probe",
        )]),
    );
    ctx.command_isolation
        .read_only_toolchain_paths
        .push(std::path::PathBuf::from("/usr/bin"));
    let durable = ctx.durable_metadata.clone();

    let err = run_echo_with_context(&registry, ctx)
        .await
        .expect_err("required network must fail closed without applied netns evidence");

    assert_eq!(err.code, CepErrorCode::GuardDenied);
    let metadata = command_boundary_metadata_json(&durable);
    assert_eq!(metadata["backend_invoked"], false);
    assert_eq!(
        metadata["effective_boundary"]["network"]["status"],
        "failed_closed"
    );
    assert_eq!(
        metadata["effective_boundary"]["network"]["diagnostics"]["supervisor_backend"],
        "native_linux"
    );
    assert_eq!(
        metadata["effective_boundary"]["network"]["diagnostics"]["namespace_status"],
        "failed_closed"
    );
    assert!(
        metadata["effective_boundary"]["network"]["diagnostics"]["setup_failure_stage"]
            .as_str()
            .is_some_and(|stage| stage != "pre_exec_setup")
    );
    assert_eq!(metadata["boundary_setup_failed_closed"], true);
}

#[tokio::test]
async fn command_boundary_enforce_required_filesystem_requires_mount_view_evidence() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path().to_str().unwrap();
    let registry = native_command_registry(root);
    let (mut ctx, _events, _cancel_tx) = test_ctx_with_run_command(root, vec!["echo".into()]).await;
    ctx.command_isolation = command_boundary_config(
        crate::enforcement::isolation::CommandIsolationMode::Enforce,
        vec![lancet_protocol::BoundaryDimension::Filesystem],
        command_boundary_probe(&[(
            lancet_protocol::BoundaryPrimitive::MountNamespace,
            lancet_protocol::HostProbeStatus::Available,
            "mount namespace available in probe",
        )]),
    );
    ctx.command_isolation
        .read_only_toolchain_paths
        .push(std::path::PathBuf::from("/usr/bin"));
    let durable = ctx.durable_metadata.clone();

    let err = run_echo_with_context(&registry, ctx)
        .await
        .expect_err("required filesystem must fail closed without applied mount view evidence");

    assert_eq!(err.code, CepErrorCode::GuardDenied);
    let metadata = command_boundary_metadata_json(&durable);
    assert_eq!(metadata["backend_invoked"], false);
    assert_eq!(
        metadata["effective_boundary"]["filesystem"]["status"],
        "failed_closed"
    );
    assert_eq!(
        metadata["effective_boundary"]["filesystem"]["diagnostics"]["mount_view_status"],
        "failed_closed"
    );
    assert!(
        metadata["effective_boundary"]["filesystem"]["source_primitives"]
            .as_array()
            .is_none_or(|primitives| primitives.is_empty())
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

fn fake_diagnostic_overlay_attempt(failed_closed: bool) -> CommandIsolationAttempt {
    CommandIsolationAttempt::new(BoundaryAttemptOutcome {
        supervisor_backend: Some("fixed_overlay".to_string()),
        env_policy: Some("allowlist".to_string()),
        fd_cleanup_status: Some("applied".to_string()),
        failed_closed,
        setup_failure_stage: failed_closed.then(|| "overlay_mount".to_string()),
        setup_failure_reason: failed_closed.then(|| "injected overlay setup failure".to_string()),
        ..BoundaryAttemptOutcome::default()
    })
}

fn fixed_diagnostic_overlay(
    project_root: &std::path::Path,
) -> crate::executor::shell::FixedOverlayExecution {
    let plan = crate::executor::supervisor::overlay::OverlayPlan::deterministic(
        project_root,
        crate::executor::supervisor::overlay::OverlayMode::Diagnostic,
        "diagnostic-routing-test",
    )
    .expect("overlay plan");
    crate::executor::shell::FixedOverlayExecution::new(plan)
}

fn failing_fixed_diagnostic_overlay(
    project_root: &std::path::Path,
    failure: crate::executor::supervisor::overlay::CleanupFailureKind,
) -> crate::executor::shell::FixedOverlayExecution {
    let plan = crate::executor::supervisor::overlay::OverlayPlan::deterministic(
        project_root,
        crate::executor::supervisor::overlay::OverlayMode::Diagnostic,
        "diagnostic-routing-test",
    )
    .expect("overlay plan");
    crate::executor::shell::FixedOverlayExecution::failing(plan, failure)
}

fn fixed_artifact_overlay(
    project_root: &std::path::Path,
) -> crate::executor::shell::FixedOverlayExecution {
    let session_id = format!(
        "artifact-routing-test-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let plan = crate::executor::supervisor::overlay::OverlayPlan::deterministic(
        project_root,
        crate::executor::supervisor::overlay::OverlayMode::Artifact,
        &session_id,
    )
    .expect("artifact overlay plan");
    let _ = std::fs::remove_dir_all(&plan.session_root);
    std::fs::create_dir_all(&plan.paths.upper).expect("artifact upper");
    crate::executor::shell::FixedOverlayExecution::new(plan)
}

fn successful_artifact_cleanup(
    _overlay: &crate::executor::shell::FixedOverlayExecution,
    disposition: crate::executor::supervisor::overlay::OverlayDisposition,
) -> crate::executor::supervisor::overlay::OverlayCleanupReport {
    crate::executor::supervisor::overlay::OverlayCleanupReport {
        mode: crate::executor::supervisor::overlay::OverlayMode::Artifact,
        disposition,
        outcome: crate::executor::supervisor::overlay::CleanupOutcome::Succeeded,
        failure_kind: None,
        path_label: None,
        reason: None,
        child_cleanup_succeeded: true,
    }
}

async fn run_fixed_diagnostic_overlay_case(
    root: &str,
    ctx: HandlerContext,
    argv: Vec<String>,
    timeout_ms: u64,
    behavior: crate::executor::shell::SupervisorPreExecBehavior,
    overlay: crate::executor::shell::FixedOverlayExecution,
    failed_closed: bool,
) -> Result<RunResult, CepError> {
    let policy = ctx.cep_context.policy.command.clone();
    let params = RunParams {
        argv: argv.clone(),
        cwd: None,
        env: BTreeMap::new(),
        timeout_ms: Some(timeout_ms),
        declared_impact: Some(ImpactKind::CommandExecution),
    };
    let lower = crate::executor::supervisor::snapshot::capture_lower_fingerprint(
        std::path::Path::new(root),
    )
    .expect("lower fingerprint");
    run_diagnostic_overlay_supervised(
        argv,
        BTreeMap::new(),
        params,
        ctx,
        Run::NAME,
        timeout_ms,
        root.to_string(),
        &policy,
        lower,
        SupervisorExecution::Fixed {
            attempt: fake_diagnostic_overlay_attempt(failed_closed),
            behavior,
            overlay: Some(overlay),
        },
    )
    .await
}

async fn test_ctx_with_artifact_caps(
    root: &str,
    argv_allowlist: Vec<String>,
    write_path_pattern: &str,
    verify: Option<VerifyCommand>,
) -> (
    HandlerContext,
    mpsc::Receiver<lancet_protocol::ToolEvent>,
    watch::Sender<bool>,
    lancet_authority::effect::ledger::EffectLedger,
) {
    let (event_tx, event_rx) = mpsc::channel(32);
    let (cancel_tx, cancel_rx) = watch::channel(false);
    let default_hook = Arc::new(lancet_authority::policy::default::DefaultPolicyHook::new(
        lancet_authority::policy::default::PolicyConfig::defaults_for_project_root(
            std::path::PathBuf::from(root),
        ),
    ));
    let store = Arc::new(ActiveEnvelopeStore::new());
    let mut caps = BTreeSet::new();
    caps.insert(Capability::RunCommand { argv_allowlist });
    caps.insert(Capability::WritePath {
        path_pattern: write_path_pattern.into(),
    });
    let envelope = ExecutionEnvelope {
        intent: Intent {
            description: "artifact command test".into(),
        },
        capabilities: caps,
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
        verify,
        on_failure: None,
        extensions: serde_json::Map::new(),
    };
    store.declare(envelope, 0, "test-turn").await.unwrap();
    let effect_ledger = lancet_authority::effect::ledger::EffectLedger::new();
    let authorized_region = lancet_authority::effect::ledger::AuthorizedRegionSlot::new();
    let mut region = lancet_authority::effect::region::AuthorizedEffectRegion::empty();
    region
        .kinds
        .insert(lancet_protocol::envelope::ImpactKind::WorkspaceMutation);
    if let Some((path, is_prefix)) = lancet_authority::effect::region::normalize_write_region_path(
        write_path_pattern,
        &std::path::PathBuf::from(root),
    ) {
        if is_prefix {
            region.write_path_prefixes.push(path);
        } else {
            region.write_paths.push(path);
        }
    }
    authorized_region.set(region);
    (
        HandlerContext {
            call_id: 1,
            events: EventSink::new(event_tx, 1),
            cancellation: Cancellation::new(cancel_rx),
            base_envelope: BaseExecutionEnvelope::for_workspace_root(root, "artifact handler test"),
            cep_context: Arc::new(CepTurnContext::defaults_for_project_root(root)),
            active_envelope: store,
            turn_id: "test-turn".into(),
            durable_metadata: crate::server::call_durable_metadata::CallDurableMetadata::default(),
            effect_ledger: effect_ledger.clone(),
            authorized_region,
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
        effect_ledger,
    )
}

async fn run_fixed_artifact_overlay_case(
    root: &str,
    ctx: HandlerContext,
    argv: Vec<String>,
    overlay: crate::executor::shell::FixedOverlayExecution,
    cleanup: crate::executor::supervisor::overlay::OverlayCleanupReport,
) -> Result<RunResult, CepError> {
    let policy = ctx.cep_context.policy.command.clone();
    let params = RunParams {
        argv: argv.clone(),
        cwd: None,
        env: BTreeMap::new(),
        timeout_ms: Some(5000),
        declared_impact: Some(ImpactKind::WorkspaceMutation),
    };
    let active_record = ctx
        .active_envelope
        .active_record()
        .await
        .expect("active artifact envelope");
    let lower =
        crate::executor::supervisor::snapshot::capture_lower_baseline(std::path::Path::new(root))
            .expect("lower baseline");
    run_artifact_overlay_supervised(
        params,
        ctx,
        Run::NAME,
        5000,
        argv,
        BTreeMap::new(),
        active_record,
        root.to_string(),
        &policy,
        lower,
        SupervisorExecution::Fixed {
            attempt: fake_diagnostic_overlay_attempt(false),
            behavior: crate::executor::shell::SupervisorPreExecBehavior::Ready,
            overlay: Some(overlay),
        },
        Some(cleanup),
    )
    .await
}

fn assert_overlay_matrix_metadata(
    metadata: &serde_json::Value,
    overlay_status: &str,
    cleanup_outcome: &str,
    lower_consistency: &str,
) {
    assert_eq!(metadata["overlay_mode"], "diagnostic_discard");
    assert_eq!(metadata["overlay_status"], overlay_status);
    assert_eq!(metadata["overlay_disposition"], "discarded");
    assert_eq!(metadata["cleanup_outcome"], cleanup_outcome);
    assert_eq!(metadata["lower_consistency"], lower_consistency);
    assert!(metadata["effective_boundary"]
        .get("overlay_status")
        .is_none());
}

#[tokio::test]
async fn diagnostic_overlay_routing_hard_denied_returns_before_overlay_setup() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path().to_str().unwrap();
    let registry = native_command_registry(root);
    let (mut ctx, _events, _cancel_tx) = test_ctx_with_run_command(root, vec!["sh".into()]).await;
    ctx.command_isolation = command_boundary_config(
        crate::enforcement::isolation::CommandIsolationMode::Enforce,
        vec![lancet_protocol::BoundaryDimension::Filesystem],
        command_boundary_probe(&[(
            lancet_protocol::BoundaryPrimitive::MountNamespace,
            lancet_protocol::HostProbeStatus::Available,
            "mount namespace available in probe",
        )]),
    );
    let durable = ctx.durable_metadata.clone();

    let params = RunParams {
        argv: vec!["sh".into(), "-c".into(), "echo denied".into()],
        cwd: None,
        env: BTreeMap::new(),
        timeout_ms: None,
        declared_impact: None,
    };
    let err = registry
        .dispatch(Run::NAME, serde_json::to_value(&params).unwrap(), ctx)
        .await
        .expect_err("hard denied command must not reach overlay setup");

    assert_eq!(err.code, CepErrorCode::GuardDenied);
    let metadata = command_boundary_metadata_json(&durable);
    assert!(metadata.get("overlay_status").is_none());
    assert_eq!(metadata["backend_invoked"], false);
}

#[tokio::test]
async fn diagnostic_overlay_routing_mode_off_records_not_requested() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path().to_str().unwrap();
    let registry = native_command_registry(root);
    let (mut ctx, _events, _cancel_tx) = test_ctx_with_run_command(root, vec!["echo".into()]).await;
    ctx.command_isolation = command_boundary_config(
        crate::enforcement::isolation::CommandIsolationMode::Off,
        vec![],
        command_boundary_probe(&[]),
    );
    let durable = ctx.durable_metadata.clone();

    let result = run_echo_with_context(&registry, ctx)
        .await
        .expect("mode off keeps real-workspace diagnostic execution");
    let parsed: RunResult = serde_json::from_value(result).unwrap();

    assert_eq!(parsed.exit_code, 0);
    let metadata = command_boundary_metadata_json(&durable);
    assert_eq!(metadata["overlay_status"], "not_requested");
    assert_eq!(metadata["overlay_mode"], "none");
    assert_eq!(metadata["cleanup_outcome"], "not_requested");
    assert!(metadata.get("effective_boundary").is_some());
    assert!(metadata["effective_boundary"]
        .get("overlay_status")
        .is_none());
}

#[tokio::test]
async fn diagnostic_overlay_routing_audit_setup_failure_records_not_applied() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path().to_str().unwrap();
    let (mut ctx, _events, _cancel_tx) = test_ctx_with_run_command(root, vec!["echo".into()]).await;
    ctx.command_isolation = command_boundary_config(
        crate::enforcement::isolation::CommandIsolationMode::Audit,
        vec![lancet_protocol::BoundaryDimension::Filesystem],
        command_boundary_probe(&[(
            lancet_protocol::BoundaryPrimitive::MountNamespace,
            lancet_protocol::HostProbeStatus::Available,
            "mount namespace available in probe",
        )]),
    );
    let durable = ctx.durable_metadata.clone();
    let policy = ctx.cep_context.policy.command.clone();
    let params = RunParams {
        argv: vec!["echo".into(), "fallback".into()],
        cwd: None,
        env: BTreeMap::new(),
        timeout_ms: Some(5000),
        declared_impact: Some(ImpactKind::CommandExecution),
    };
    let lower = crate::executor::supervisor::snapshot::capture_lower_fingerprint(dir.path())
        .expect("lower fingerprint");

    let result = run_diagnostic_overlay_supervised(
        params.argv.clone(),
        BTreeMap::new(),
        params,
        ctx,
        Run::NAME,
        5000,
        root.to_string(),
        &policy,
        lower,
        SupervisorExecution::Fixed {
            attempt: fake_diagnostic_overlay_attempt(true),
            behavior: crate::executor::shell::SupervisorPreExecBehavior::FailBeforeExec,
            overlay: Some(fixed_diagnostic_overlay(dir.path())),
        },
    )
    .await
    .expect("audit mode may fall back after overlay setup failure");
    assert!(result.stdout.contains("fallback"));

    let metadata = command_boundary_metadata_json(&durable);
    assert_eq!(metadata["overlay_mode"], "diagnostic_discard");
    assert_eq!(metadata["overlay_status"], "not_applied");
    assert_eq!(metadata["overlay_disposition"], "not_applied");
    assert_eq!(metadata["cleanup_outcome"], "cleanup_succeeded");
    assert_eq!(metadata["backend_invoked"], true);
    assert!(metadata["effective_boundary"]
        .get("overlay_status")
        .is_none());
}

#[tokio::test]
async fn diagnostic_overlay_failure_matrix_success_discards_metadata_and_preserves_lower() {
    let dir = tempfile::TempDir::new().unwrap();
    std::fs::write(dir.path().join("baseline.txt"), "baseline").unwrap();
    let root = dir.path().to_str().unwrap();
    let (mut ctx, _events, _cancel_tx) = test_ctx_with_run_command(root, vec!["true".into()]).await;
    ctx.command_isolation.mode = crate::enforcement::isolation::CommandIsolationMode::Enforce;
    let durable = ctx.durable_metadata.clone();
    let before = crate::executor::supervisor::snapshot::capture_workspace_snapshot(root).unwrap();

    let result = run_fixed_diagnostic_overlay_case(
        root,
        ctx,
        vec!["true".into()],
        5000,
        crate::executor::shell::SupervisorPreExecBehavior::Ready,
        fixed_diagnostic_overlay(dir.path()),
        false,
    )
    .await
    .expect("success case");

    assert_eq!(result.exit_code, 0);
    let after = crate::executor::supervisor::snapshot::capture_workspace_snapshot(root).unwrap();
    assert_eq!(before, after);
    let metadata = command_boundary_metadata_json(&durable);
    assert_overlay_matrix_metadata(&metadata, "applied", "cleanup_succeeded", "unchanged");
}

#[tokio::test]
async fn diagnostic_overlay_failure_matrix_nonzero_exit_discards_metadata_and_preserves_lower() {
    let dir = tempfile::TempDir::new().unwrap();
    std::fs::write(dir.path().join("baseline.txt"), "baseline").unwrap();
    let root = dir.path().to_str().unwrap();
    let (mut ctx, _events, _cancel_tx) = test_ctx_with_run_command(root, vec!["sh".into()]).await;
    ctx.command_isolation.mode = crate::enforcement::isolation::CommandIsolationMode::Enforce;
    let durable = ctx.durable_metadata.clone();
    let before = crate::executor::supervisor::snapshot::capture_workspace_snapshot(root).unwrap();

    let result = run_fixed_diagnostic_overlay_case(
        root,
        ctx,
        vec!["sh".into(), "-c".into(), "exit 7".into()],
        5000,
        crate::executor::shell::SupervisorPreExecBehavior::Ready,
        fixed_diagnostic_overlay(dir.path()),
        false,
    )
    .await
    .expect("nonzero exit still returns RunResult");

    assert_eq!(result.exit_code, 7);
    let after = crate::executor::supervisor::snapshot::capture_workspace_snapshot(root).unwrap();
    assert_eq!(before, after);
    let metadata = command_boundary_metadata_json(&durable);
    assert_overlay_matrix_metadata(&metadata, "applied", "cleanup_succeeded", "unchanged");
}

#[tokio::test]
async fn diagnostic_overlay_failure_matrix_timeout_discards_metadata_and_preserves_lower() {
    let dir = tempfile::TempDir::new().unwrap();
    std::fs::write(dir.path().join("baseline.txt"), "baseline").unwrap();
    let root = dir.path().to_str().unwrap();
    let (mut ctx, _events, _cancel_tx) =
        test_ctx_with_run_command(root, vec!["sleep".into()]).await;
    ctx.command_isolation.mode = crate::enforcement::isolation::CommandIsolationMode::Enforce;
    let durable = ctx.durable_metadata.clone();
    let before = crate::executor::supervisor::snapshot::capture_workspace_snapshot(root).unwrap();

    let err = run_fixed_diagnostic_overlay_case(
        root,
        ctx,
        vec!["sleep".into(), "10".into()],
        50,
        crate::executor::shell::SupervisorPreExecBehavior::Ready,
        fixed_diagnostic_overlay(dir.path()),
        false,
    )
    .await
    .expect_err("timeout case");

    assert_eq!(err.code, CepErrorCode::CommandTimeout);
    let after = crate::executor::supervisor::snapshot::capture_workspace_snapshot(root).unwrap();
    assert_eq!(before, after);
    let metadata = command_boundary_metadata_json(&durable);
    assert_overlay_matrix_metadata(&metadata, "applied", "cleanup_succeeded", "unchanged");
}

#[tokio::test]
async fn diagnostic_overlay_failure_matrix_cancellation_discards_metadata_and_preserves_lower() {
    let dir = tempfile::TempDir::new().unwrap();
    std::fs::write(dir.path().join("baseline.txt"), "baseline").unwrap();
    let root = dir.path().to_str().unwrap();
    let (mut ctx, _events, cancel_tx) = test_ctx_with_run_command(root, vec!["sleep".into()]).await;
    ctx.command_isolation.mode = crate::enforcement::isolation::CommandIsolationMode::Enforce;
    let durable = ctx.durable_metadata.clone();
    let before = crate::executor::supervisor::snapshot::capture_workspace_snapshot(root).unwrap();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        let _ = cancel_tx.send(true);
    });

    let err = run_fixed_diagnostic_overlay_case(
        root,
        ctx,
        vec!["sleep".into(), "10".into()],
        5000,
        crate::executor::shell::SupervisorPreExecBehavior::Ready,
        fixed_diagnostic_overlay(dir.path()),
        false,
    )
    .await
    .expect_err("cancellation case");

    assert_eq!(err.code, CepErrorCode::ExecutionFailed);
    let after = crate::executor::supervisor::snapshot::capture_workspace_snapshot(root).unwrap();
    assert_eq!(before, after);
    let metadata = command_boundary_metadata_json(&durable);
    assert_overlay_matrix_metadata(&metadata, "applied", "cleanup_succeeded", "unchanged");
}

#[tokio::test]
async fn diagnostic_overlay_failure_matrix_denial_discards_without_backend_and_preserves_lower() {
    let dir = tempfile::TempDir::new().unwrap();
    let marker = dir.path().join("should-not-exist");
    let root = dir.path().to_str().unwrap();
    let (mut ctx, _events, _cancel_tx) =
        test_ctx_with_run_command(root, vec!["touch".into()]).await;
    ctx.command_isolation.mode = crate::enforcement::isolation::CommandIsolationMode::Enforce;
    let durable = ctx.durable_metadata.clone();
    let before = crate::executor::supervisor::snapshot::capture_workspace_snapshot(root).unwrap();

    let err = run_fixed_diagnostic_overlay_case(
        root,
        ctx,
        vec!["touch".into(), marker.to_string_lossy().to_string()],
        5000,
        crate::executor::shell::SupervisorPreExecBehavior::FailBeforeExec,
        fixed_diagnostic_overlay(dir.path()),
        true,
    )
    .await
    .expect_err("setup denial case");

    assert_eq!(err.code, CepErrorCode::GuardDenied);
    assert!(!marker.exists());
    let after = crate::executor::supervisor::snapshot::capture_workspace_snapshot(root).unwrap();
    assert_eq!(before, after);
    let metadata = command_boundary_metadata_json(&durable);
    assert_overlay_matrix_metadata(&metadata, "setup_failed", "cleanup_succeeded", "unchanged");
    assert_eq!(metadata["backend_invoked"], false);
}

#[tokio::test]
async fn diagnostic_overlay_failure_matrix_setup_failure_fallback_records_not_applied() {
    let dir = tempfile::TempDir::new().unwrap();
    std::fs::write(dir.path().join("baseline.txt"), "baseline").unwrap();
    let root = dir.path().to_str().unwrap();
    let (mut ctx, _events, _cancel_tx) = test_ctx_with_run_command(root, vec!["echo".into()]).await;
    ctx.command_isolation = command_boundary_config(
        crate::enforcement::isolation::CommandIsolationMode::Audit,
        vec![lancet_protocol::BoundaryDimension::Filesystem],
        command_boundary_probe(&[(
            lancet_protocol::BoundaryPrimitive::MountNamespace,
            lancet_protocol::HostProbeStatus::Available,
            "mount namespace available in probe",
        )]),
    );
    let durable = ctx.durable_metadata.clone();
    let before = crate::executor::supervisor::snapshot::capture_workspace_snapshot(root).unwrap();

    let result = run_fixed_diagnostic_overlay_case(
        root,
        ctx,
        vec!["echo".into(), "fallback".into()],
        5000,
        crate::executor::shell::SupervisorPreExecBehavior::FailBeforeExec,
        fixed_diagnostic_overlay(dir.path()),
        true,
    )
    .await
    .expect("audit fallback case");

    assert!(result.stdout.contains("fallback"));
    let after = crate::executor::supervisor::snapshot::capture_workspace_snapshot(root).unwrap();
    assert_eq!(before, after);
    let metadata = command_boundary_metadata_json(&durable);
    assert_eq!(metadata["overlay_status"], "not_applied");
    assert_eq!(metadata["overlay_disposition"], "not_applied");
    assert_eq!(metadata["cleanup_outcome"], "cleanup_succeeded");
    assert_eq!(metadata["lower_consistency"], "unchanged");
}

#[tokio::test]
async fn diagnostic_overlay_failure_matrix_shell_spawn_failure_discards_and_preserves_lower() {
    let dir = tempfile::TempDir::new().unwrap();
    std::fs::write(dir.path().join("baseline.txt"), "baseline").unwrap();
    let root = dir.path().to_str().unwrap();
    let (mut ctx, _events, _cancel_tx) =
        test_ctx_with_run_command(root, vec!["missing-command".into()]).await;
    ctx.command_isolation.mode = crate::enforcement::isolation::CommandIsolationMode::Enforce;
    let durable = ctx.durable_metadata.clone();
    let before = crate::executor::supervisor::snapshot::capture_workspace_snapshot(root).unwrap();

    let err = run_fixed_diagnostic_overlay_case(
        root,
        ctx,
        vec!["definitely-not-a-lattice-test-command".into()],
        5000,
        crate::executor::shell::SupervisorPreExecBehavior::Ready,
        fixed_diagnostic_overlay(dir.path()),
        false,
    )
    .await
    .expect_err("spawn failure case");

    assert_eq!(err.code, CepErrorCode::ExecutionFailed);
    let after = crate::executor::supervisor::snapshot::capture_workspace_snapshot(root).unwrap();
    assert_eq!(before, after);
    let metadata = command_boundary_metadata_json(&durable);
    assert_overlay_matrix_metadata(&metadata, "applied", "cleanup_succeeded", "unchanged");
    assert_eq!(metadata["backend_invoked"], true);
}

#[tokio::test]
async fn diagnostic_overlay_failure_matrix_cleanup_failure_preserves_original_success() {
    let dir = tempfile::TempDir::new().unwrap();
    std::fs::write(dir.path().join("baseline.txt"), "baseline").unwrap();
    let root = dir.path().to_str().unwrap();
    let (mut ctx, _events, _cancel_tx) = test_ctx_with_run_command(root, vec!["true".into()]).await;
    ctx.command_isolation.mode = crate::enforcement::isolation::CommandIsolationMode::Enforce;
    let durable = ctx.durable_metadata.clone();
    let before = crate::executor::supervisor::snapshot::capture_workspace_snapshot(root).unwrap();

    let result = run_fixed_diagnostic_overlay_case(
        root,
        ctx,
        vec!["true".into()],
        5000,
        crate::executor::shell::SupervisorPreExecBehavior::Ready,
        failing_fixed_diagnostic_overlay(
            dir.path(),
            crate::executor::supervisor::overlay::CleanupFailureKind::UnmountMerged,
        ),
        false,
    )
    .await
    .expect("cleanup failure keeps diagnostic command result");

    assert_eq!(result.exit_code, 0);
    let after = crate::executor::supervisor::snapshot::capture_workspace_snapshot(root).unwrap();
    assert_eq!(before, after);
    let metadata = command_boundary_metadata_json(&durable);
    assert_overlay_matrix_metadata(&metadata, "applied", "cleanup_failed", "unchanged");
    assert_eq!(metadata["cleanup_failure_kind"], "unmount_merged");
}

#[tokio::test]
async fn diagnostic_overlay_failure_matrix_lower_drift_records_drift_and_discards() {
    let dir = tempfile::TempDir::new().unwrap();
    std::fs::write(dir.path().join("baseline.txt"), "baseline").unwrap();
    let root = dir.path().to_str().unwrap();
    let (mut ctx, _events, _cancel_tx) =
        test_ctx_with_run_command(root, vec!["sleep".into()]).await;
    ctx.command_isolation.mode = crate::enforcement::isolation::CommandIsolationMode::Enforce;
    let durable = ctx.durable_metadata.clone();
    let drift_path = dir.path().join("external-drift.txt");
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        std::fs::write(drift_path, "external drift").unwrap();
    });

    let result = run_fixed_diagnostic_overlay_case(
        root,
        ctx,
        vec!["sleep".into(), "0.1".into()],
        5000,
        crate::executor::shell::SupervisorPreExecBehavior::Ready,
        fixed_diagnostic_overlay(dir.path()),
        false,
    )
    .await
    .expect("lower drift case");

    assert_eq!(result.exit_code, 0);
    let metadata = command_boundary_metadata_json(&durable);
    assert_overlay_matrix_metadata(&metadata, "applied", "cleanup_succeeded", "drift_detected");
}

#[tokio::test]
async fn artifact_overlay_staging_denies_out_of_scope_without_touching_workspace() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path().to_str().unwrap();
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    let (mut ctx, mut events, _cancel_tx, ledger) = test_ctx_with_artifact_caps(
        root,
        vec!["sh".into()],
        "src/**",
        Some(VerifyCommand {
            command: "echo verify".into(),
            expected_exit_code: Some(0),
        }),
    )
    .await;
    ctx.command_isolation.mode = crate::enforcement::isolation::CommandIsolationMode::Enforce;
    let durable = ctx.durable_metadata.clone();
    let before = crate::executor::supervisor::snapshot::capture_workspace_snapshot(root).unwrap();
    let overlay = fixed_artifact_overlay(dir.path());
    let cleanup = successful_artifact_cleanup(
        &overlay,
        crate::executor::supervisor::overlay::OverlayDisposition::Denied,
    );

    let err = run_fixed_artifact_overlay_case(
        root,
        ctx,
        vec!["sh".into(), "-c".into(), "echo staged > outside.txt".into()],
        overlay,
        cleanup,
    )
    .await
    .expect_err("out-of-scope artifact must be denied");

    assert!(!dir.path().join("outside.txt").exists());
    let after = crate::executor::supervisor::snapshot::capture_workspace_snapshot(root).unwrap();
    assert_eq!(before, after);
    let metadata = command_boundary_metadata_json(&durable);
    assert_eq!(err.code, CepErrorCode::ReplanRequired);
    assert_eq!(metadata["overlay_mode"], "artifact");
    assert_eq!(metadata["overlay_status"], "applied");
    assert_eq!(metadata["artifact_manifest_status"], "denied");
    assert_eq!(metadata["artifact_adjudication"], "denied_write_scope");
    assert_eq!(metadata["verify_layer"], "not_run");
    assert_eq!(metadata["commit_outcome"], "not_started");
    assert_eq!(metadata["cleanup_outcome"], "cleanup_succeeded");
    assert!(metadata["failed_entry"].is_null());

    let effects = ledger.snapshot();
    assert!(effects.iter().any(|effect| {
        effect.kind == ImpactKind::WorkspaceMutation
            && matches!(&effect.scope, lancet_authority::effect::ledger::ObservedScope::Path(path) if path.ends_with("outside.txt"))
    }));
    let mut saw_replan_required = false;
    let mut saw_verify_failed = false;
    while let Ok(event) = events.try_recv() {
        saw_replan_required |= event.method == "cep.replan_required";
        saw_verify_failed |= event.method == "cep.verify_failed";
    }
    assert!(saw_replan_required);
    assert!(!saw_verify_failed);
}

#[tokio::test]
async fn artifact_adjudication_scope_denies_effect_region_before_verify() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path().to_str().unwrap();
    std::fs::create_dir_all(dir.path().join("allowed")).unwrap();
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    let (mut ctx, mut events, _cancel_tx, _ledger) = test_ctx_with_artifact_caps(
        root,
        vec!["sh".into()],
        "allowed/**",
        Some(VerifyCommand {
            command: "echo verify".into(),
            expected_exit_code: Some(0),
        }),
    )
    .await;
    ctx.command_isolation.mode = crate::enforcement::isolation::CommandIsolationMode::Enforce;
    ctx.authorized_region.set({
        let mut region = lancet_authority::effect::region::AuthorizedEffectRegion::empty();
        region
            .kinds
            .insert(lancet_protocol::envelope::ImpactKind::WorkspaceMutation);
        region.write_path_prefixes.push(dir.path().join("src"));
        region
    });
    let durable = ctx.durable_metadata.clone();
    let overlay = fixed_artifact_overlay(dir.path());
    let cleanup = successful_artifact_cleanup(
        &overlay,
        crate::executor::supervisor::overlay::OverlayDisposition::Denied,
    );

    let err = run_fixed_artifact_overlay_case(
        root,
        ctx,
        vec![
            "sh".into(),
            "-c".into(),
            "mkdir -p allowed && echo staged > allowed/generated.txt".into(),
        ],
        overlay,
        cleanup,
    )
    .await
    .expect_err("effect-region miss must be denied");

    assert_eq!(err.code, CepErrorCode::ReplanRequired);
    assert!(!dir.path().join("allowed/generated.txt").exists());
    let metadata = command_boundary_metadata_json(&durable);
    assert_eq!(metadata["artifact_adjudication"], "denied_effect_region");
    assert_eq!(metadata["verify_layer"], "not_run");
    assert_eq!(metadata["commit_outcome"], "not_started");
    let mut saw_verify_failed = false;
    while let Ok(event) = events.try_recv() {
        saw_verify_failed |= event.method == "cep.verify_failed";
    }
    assert!(!saw_verify_failed);
}

#[cfg(unix)]
#[tokio::test]
async fn unsupported_artifact_kind_denies_manifest_and_records_rejected_kind() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path().to_str().unwrap();
    let (mut ctx, mut events, _cancel_tx, ledger) = test_ctx_with_artifact_caps(
        root,
        vec!["mkfifo".into()],
        ".",
        Some(VerifyCommand {
            command: "echo verify".into(),
            expected_exit_code: Some(0),
        }),
    )
    .await;
    ctx.command_isolation.mode = crate::enforcement::isolation::CommandIsolationMode::Enforce;
    let durable = ctx.durable_metadata.clone();
    let overlay = fixed_artifact_overlay(dir.path());
    let cleanup = successful_artifact_cleanup(
        &overlay,
        crate::executor::supervisor::overlay::OverlayDisposition::Denied,
    );

    let err = run_fixed_artifact_overlay_case(
        root,
        ctx,
        vec!["mkfifo".into(), "pipe".into()],
        overlay,
        cleanup,
    )
    .await
    .expect_err("unsupported FIFO artifact must be denied");

    assert_eq!(err.code, CepErrorCode::GuardDenied);
    assert!(!dir.path().join("pipe").exists());
    let metadata = command_boundary_metadata_json(&durable);
    assert_eq!(metadata["artifact_manifest_status"], "denied");
    assert_eq!(metadata["artifact_adjudication"], "denied_unsupported_kind");
    assert_eq!(metadata["overlay_disposition"], "denied");
    assert_eq!(metadata["verify_layer"], "not_run");
    assert_eq!(metadata["commit_outcome"], "not_started");
    assert_eq!(metadata["rejected_kinds"][0]["path"], "pipe");
    assert_eq!(metadata["rejected_kinds"][0]["kind"], "fifo");

    assert!(!ledger
        .snapshot()
        .iter()
        .any(|effect| { effect.source == lancet_authority::effect::ledger::EffectSource::Verify }));
    let mut saw_verify_failed = false;
    while let Ok(event) = events.try_recv() {
        saw_verify_failed |= event.method == "cep.verify_failed";
    }
    assert!(!saw_verify_failed);
}

#[cfg(unix)]
#[tokio::test]
async fn artifact_overlay_commit_flow_commits_accepted_entries_after_verify() {
    let _verify_write_guard = TEST_ARTIFACT_VERIFY_WRITE_LOCK.lock().await;
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path().to_str().unwrap();
    std::fs::write(dir.path().join("existing.txt"), "old").unwrap();
    std::fs::write(dir.path().join("remove.txt"), "remove").unwrap();
    let (mut ctx, _events, _cancel_tx, ledger) = test_ctx_with_artifact_caps(
        root,
        vec!["sh".into()],
        ".",
        Some(VerifyCommand {
            command: "echo verify".into(),
            expected_exit_code: Some(0),
        }),
    )
    .await;
    ctx.command_isolation.mode = crate::enforcement::isolation::CommandIsolationMode::Enforce;
    let durable = ctx.durable_metadata.clone();
    let overlay = fixed_artifact_overlay(dir.path());
    let cleanup = successful_artifact_cleanup(
        &overlay,
        crate::executor::supervisor::overlay::OverlayDisposition::Committed,
    );
    set_test_artifact_verify_write(Some(""));

    let result = run_fixed_artifact_overlay_case(
        root,
        ctx,
        vec![
            "sh".into(),
            "-c".into(),
            "printf new > new.txt; printf updated > existing.txt; touch .wh.remove.txt; ln -s existing.txt link.txt".into(),
        ],
        overlay,
        cleanup,
    )
    .await
    .expect("accepted artifact command should commit");

    assert_eq!(result.exit_code, 0);
    assert_eq!(
        std::fs::read_to_string(dir.path().join("new.txt")).unwrap(),
        "new"
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("existing.txt")).unwrap(),
        "updated"
    );
    assert!(!dir.path().join("remove.txt").exists());
    assert_eq!(
        std::fs::read_link(dir.path().join("link.txt")).unwrap(),
        std::path::PathBuf::from("existing.txt")
    );
    let metadata = command_boundary_metadata_json(&durable);
    assert_eq!(metadata["artifact_manifest_status"], "accepted");
    assert_eq!(metadata["artifact_adjudication"], "accepted");
    assert_eq!(metadata["verify_layer"], "discarded");
    assert_eq!(metadata["commit_outcome"], "committed");
    assert_eq!(metadata["lower_consistency"], "unchanged");
    assert_eq!(metadata["commit_baseline_validation"], "passed");
    assert!(metadata["commit_baseline_failure_reason"].is_null());
    assert_eq!(metadata["commit_baseline_details"], serde_json::json!({}));
    assert_eq!(
        metadata["effective_boundary"]["filesystem"]["observed"],
        true
    );
    assert_eq!(metadata["effective_boundary"]["network"]["observed"], false);
    assert_eq!(
        metadata["effective_boundary"]["landlock"]["observed"],
        false
    );
    assert_eq!(metadata["applied_entries"].as_array().unwrap().len(), 4);
    assert!(ledger
        .snapshot()
        .iter()
        .any(|effect| { effect.source == lancet_authority::effect::ledger::EffectSource::Verify }));
}

#[tokio::test]
async fn artifact_overlay_commit_flow_lower_drift_metadata_after_verify_without_committing() {
    let _verify_write_guard = TEST_ARTIFACT_VERIFY_WRITE_LOCK.lock().await;
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path().to_str().unwrap();
    std::fs::write(dir.path().join("baseline.txt"), "before").unwrap();
    let (mut ctx, _events, _cancel_tx, _ledger) = test_ctx_with_artifact_caps(
        root,
        vec!["sh".into()],
        ".",
        Some(VerifyCommand {
            command: "echo verify".into(),
            expected_exit_code: Some(0),
        }),
    )
    .await;
    ctx.command_isolation.mode = crate::enforcement::isolation::CommandIsolationMode::Enforce;
    let durable = ctx.durable_metadata.clone();
    let overlay = fixed_artifact_overlay(dir.path());
    let cleanup = successful_artifact_cleanup(
        &overlay,
        crate::executor::supervisor::overlay::OverlayDisposition::Committed,
    );
    set_test_artifact_verify_write(Some(""));

    let policy = ctx.cep_context.policy.command.clone();
    let params = RunParams {
        argv: vec![
            "sh".into(),
            "-c".into(),
            "printf artifact > artifact.txt".into(),
        ],
        cwd: None,
        env: BTreeMap::new(),
        timeout_ms: Some(5000),
        declared_impact: Some(ImpactKind::WorkspaceMutation),
    };
    let active_record = ctx
        .active_envelope
        .active_record()
        .await
        .expect("active artifact envelope");
    let lower =
        crate::executor::supervisor::snapshot::capture_lower_baseline(std::path::Path::new(root))
            .expect("lower baseline");
    std::fs::write(dir.path().join("baseline.txt"), "drift").unwrap();

    let err = run_artifact_overlay_supervised(
        params,
        ctx,
        Run::NAME,
        5000,
        vec![
            "sh".into(),
            "-c".into(),
            "printf artifact > artifact.txt".into(),
        ],
        BTreeMap::new(),
        active_record,
        root.to_string(),
        &policy,
        lower,
        SupervisorExecution::Fixed {
            attempt: fake_diagnostic_overlay_attempt(false),
            behavior: crate::executor::shell::SupervisorPreExecBehavior::Ready,
            overlay: Some(overlay),
        },
        Some(cleanup),
    )
    .await
    .expect_err("lower drift after verify must deny commit");

    assert_eq!(err.code, CepErrorCode::ReplanRequired);
    assert!(!dir.path().join("artifact.txt").exists());
    let metadata = command_boundary_metadata_json(&durable);
    assert_eq!(metadata["artifact_adjudication"], "denied_lower_drift");
    assert_eq!(metadata["lower_consistency"], "drift_detected");
    assert_eq!(metadata["overlay_disposition"], "denied");
    assert_eq!(metadata["commit_outcome"], "denied");
    assert_eq!(metadata["commit_baseline_validation"], "failed");
    assert_eq!(metadata["commit_baseline_failure_reason"], "lower_drift");
    assert_eq!(
        metadata["commit_baseline_details"]["changed_paths"],
        serde_json::json!(["baseline.txt"])
    );
}

#[tokio::test]
async fn artifact_overlay_commit_flow_verify_contamination_metadata_blocks_commit() {
    let _verify_write_guard = TEST_ARTIFACT_VERIFY_WRITE_LOCK.lock().await;
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path().to_str().unwrap();
    let (mut ctx, _events, _cancel_tx, _ledger) = test_ctx_with_artifact_caps(
        root,
        vec!["sh".into()],
        ".",
        Some(VerifyCommand {
            command: "echo verify".into(),
            expected_exit_code: Some(0),
        }),
    )
    .await;
    ctx.command_isolation.mode = crate::enforcement::isolation::CommandIsolationMode::Enforce;
    let durable = ctx.durable_metadata.clone();
    let overlay = fixed_artifact_overlay(dir.path());
    let cleanup = successful_artifact_cleanup(
        &overlay,
        crate::executor::supervisor::overlay::OverlayDisposition::Committed,
    );
    set_test_artifact_verify_write(Some("verify-only.txt"));

    let err = run_fixed_artifact_overlay_case(
        root,
        ctx,
        vec![
            "sh".into(),
            "-c".into(),
            "printf artifact > artifact.txt".into(),
        ],
        overlay,
        cleanup,
    )
    .await
    .expect_err("verify contamination must deny artifact commit");

    assert_eq!(err.code, CepErrorCode::ReplanRequired);
    assert!(!dir.path().join("artifact.txt").exists());
    assert!(!dir.path().join("verify-only.txt").exists());
    let metadata = command_boundary_metadata_json(&durable);
    assert_eq!(metadata["artifact_adjudication"], "denied_commit_baseline");
    assert_eq!(metadata["lower_consistency"], "not_checked");
    assert_eq!(metadata["overlay_disposition"], "denied");
    assert_eq!(metadata["commit_outcome"], "denied");
    assert_eq!(metadata["commit_baseline_validation"], "failed");
    assert_eq!(
        metadata["commit_baseline_failure_reason"],
        "verify_layer_contamination"
    );
    assert_eq!(
        metadata["commit_baseline_details"]["contaminated_paths"],
        serde_json::json!(["verify-only.txt"])
    );
}

#[test]
fn artifact_overlay_commit_flow_anchor_short_circuit_metadata_preserves_lower_not_checked() {
    let commit_report = ArtifactCommitReport {
        commit_outcome: CommitOutcome::Denied,
        lower_consistency: "not_checked".to_string(),
        artifact_adjudication: "denied_commit_baseline".to_string(),
        overlay_disposition: crate::executor::supervisor::overlay::OverlayDisposition::Denied,
        commit_baseline_validation: "failed".to_string(),
        commit_baseline_failure_reason: Some("missing_anchor".to_string()),
        commit_baseline_details: serde_json::json!({
            "hunks": [{
                "file_path": "src/lib.rs",
                "hunk_index": 0,
                "anchor_count": 1,
                "anchor_hashes": ["blake3:abcd"],
                "match_result": "missing_anchor",
                "baseline_hash": "blake3:baseline"
            }]
        }),
        applied_entries: Vec::new(),
        failed_entry: None,
        changed_paths: Vec::new(),
    };
    let metadata = artifact_overlay_metadata_json(
        Run::NAME,
        ArtifactOverlayMetadata {
            status: OverlayStatus::Applied,
            disposition: Some(crate::executor::supervisor::overlay::OverlayDisposition::Denied),
            lower_consistency: lower_consistency_from_commit_report(&commit_report),
            artifact_manifest_status: "accepted",
            artifact_adjudication: &commit_report.artifact_adjudication,
            verify_layer: VerifyLayerStatus::Discarded,
            cleanup: None,
            commit_report: Some(&commit_report),
            rejected_kinds: &[],
            boundary_preflight: None,
            artifact_manifest_generated: true,
        },
    );

    assert_eq!(metadata["lower_consistency"], "not_checked");
    assert_eq!(metadata["commit_baseline_validation"], "failed");
    assert_eq!(metadata["commit_baseline_failure_reason"], "missing_anchor");
    assert_eq!(
        metadata["commit_baseline_details"]["hunks"][0]["match_result"],
        "missing_anchor"
    );
}

#[tokio::test]
async fn artifact_overlay_commit_flow_cleanup_failure_blocks_commit() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path().to_str().unwrap();
    let (mut ctx, _events, _cancel_tx, _ledger) =
        test_ctx_with_artifact_caps(root, vec!["sh".into()], ".", None).await;
    ctx.command_isolation.mode = crate::enforcement::isolation::CommandIsolationMode::Enforce;
    let durable = ctx.durable_metadata.clone();
    let overlay = fixed_artifact_overlay(dir.path());
    let cleanup = crate::executor::supervisor::overlay::OverlayCleanupReport {
        mode: crate::executor::supervisor::overlay::OverlayMode::Artifact,
        disposition: crate::executor::supervisor::overlay::OverlayDisposition::Committed,
        outcome: crate::executor::supervisor::overlay::CleanupOutcome::Failed,
        failure_kind: Some(crate::executor::supervisor::overlay::CleanupFailureKind::RemoveUpper),
        path_label: Some("upper"),
        reason: Some("injected cleanup failure".into()),
        child_cleanup_succeeded: true,
    };

    let err = run_fixed_artifact_overlay_case(
        root,
        ctx,
        vec![
            "sh".into(),
            "-c".into(),
            "printf artifact > artifact.txt".into(),
        ],
        overlay,
        cleanup,
    )
    .await
    .expect_err("cleanup failure must block artifact commit");

    assert_eq!(err.code, CepErrorCode::ExecutionFailed);
    assert!(!dir.path().join("artifact.txt").exists());
    let metadata = command_boundary_metadata_json(&durable);
    assert_eq!(metadata["artifact_adjudication"], "denied_cleanup_failed");
    assert_eq!(metadata["cleanup_outcome"], "cleanup_failed");
    assert_eq!(metadata["commit_outcome"], "not_started");
}

#[tokio::test]
async fn artifact_fallback_without_overlay_records_skipped_no_overlay() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path().to_str().unwrap();
    let (mut ctx, _events, _cancel_tx, _ledger) =
        test_ctx_with_artifact_caps(root, vec!["touch".into()], ".", None).await;
    ctx.command_isolation.mode = crate::enforcement::isolation::CommandIsolationMode::Audit;
    let durable = ctx.durable_metadata.clone();
    let policy = ctx.cep_context.policy.command.clone();
    let active_record = ctx
        .active_envelope
        .active_record()
        .await
        .expect("active artifact envelope");
    let pre_snapshot = crate::executor::supervisor::snapshot::capture_workspace_snapshot(root)
        .expect("pre snapshot");
    let params = RunParams {
        argv: vec!["touch".into(), "fallback.txt".into()],
        cwd: None,
        env: BTreeMap::new(),
        timeout_ms: Some(5000),
        declared_impact: Some(ImpactKind::WorkspaceMutation),
    };

    let result = artifact_fallback_without_overlay(
        params,
        ctx,
        Run::NAME,
        5000,
        active_record,
        root.to_string(),
        &policy,
        pre_snapshot,
    )
    .await
    .expect("audit fallback may run without overlay");

    assert_eq!(result.exit_code, 0);
    assert!(dir.path().join("fallback.txt").exists());
    let metadata = command_boundary_metadata_json(&durable);
    assert_eq!(metadata["overlay_status"], "not_applied");
    assert_eq!(metadata["artifact_adjudication"], "skipped_no_overlay");
    assert_eq!(metadata["commit_outcome"], "not_started");
}

#[tokio::test]
async fn artifact_mode_off_records_not_requested_and_uses_snapshot_diff() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path().to_str().unwrap();
    let registry = native_command_registry(root);
    let (mut ctx, _events, _cancel_tx, _ledger) =
        test_ctx_with_artifact_caps(root, vec!["touch".into()], ".", None).await;
    ctx.command_isolation.mode = crate::enforcement::isolation::CommandIsolationMode::Off;
    let durable = ctx.durable_metadata.clone();
    let params = RunParams {
        argv: vec!["touch".into(), "mode-off.txt".into()],
        cwd: None,
        env: BTreeMap::new(),
        timeout_ms: Some(5000),
        declared_impact: Some(ImpactKind::WorkspaceMutation),
    };

    let value = registry
        .dispatch(Run::NAME, serde_json::to_value(&params).unwrap(), ctx)
        .await
        .expect("mode off workspace mutation keeps fallback path");
    let parsed: RunResult = serde_json::from_value(value).unwrap();

    assert_eq!(parsed.exit_code, 0);
    assert!(dir.path().join("mode-off.txt").exists());
    let metadata = command_boundary_metadata_json(&durable);
    assert_eq!(metadata["overlay_status"], "not_requested");
    assert_eq!(metadata["artifact_adjudication"], "skipped_no_overlay");
    assert_eq!(metadata["commit_outcome"], "not_started");
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
    let keys: Vec<_> = metadata.as_object().unwrap().keys().cloned().collect();
    assert_eq!(
        keys,
        vec![
            "effect_observation".to_string(),
            "observed_effects".to_string()
        ]
    );
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
    let keys: Vec<_> = metadata.as_object().unwrap().keys().cloned().collect();
    assert_eq!(
        keys,
        vec![
            "effect_observation".to_string(),
            "observed_effects".to_string()
        ]
    );
    assert!(!metadata["observed_effects"]
        .as_array()
        .unwrap()
        .iter()
        .any(|effect| effect.as_str() == Some("workspace_mutation_diff")));
}
