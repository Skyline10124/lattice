// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Direct-mode `exec.command_run` test suite.
//!
//! Focused coverage for the sole (Direct) command execution path:
//! classification, execution success/failure, workspace-mutation changed-path
//! validation, Direct verify integration, Guard L1 scope validation, effect
//! recording, boundary metadata, and audit rejection recording.

use super::*;
use crate::execution_backend::{
    DirectVerifyConfig, DirectoryEntry, ExecOutput, ExecRequest, ExecutionBackend,
    ExecutionBackendError, ExecutionRouting, FindRequest, GrepMatch, GrepRequest, PathFacts,
    PathIntent, UpperContext, WorkspacePath,
};
use crate::server::registry::{Cancellation, EventSink};
use lancet_authority::CepTurnContext;
use lancet_protocol::effect_types::ImpactKind;

use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use tokio::sync::{mpsc, watch};

// ── Test fixture ─────────────────────────────────────────────────────────────

/// Build a `HandlerContext` matching the post-envelope-removal struct shape
/// (no `authority_mode`, `base_envelope`, or `active_envelope` fields).
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
            cep_context: Arc::new(CepTurnContext::defaults_for_project_root(root)),
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
            cep_mode: lancet_protocol::policy::CepMode::Off,
            command_isolation:
                crate::enforcement::isolation::CommandIsolationRuntimeConfig::default(),
            execution: None,
            direct_verify: None,
            routing_invocation: crate::execution_backend::RoutingInvocationState::default(),
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

// ── CommandSpyBackend mock ───────────────────────────────────────────────────

#[derive(Default)]
struct CommandSpyBackend {
    resolve_calls: Mutex<Vec<(String, PathIntent)>>,
    exec_calls: Mutex<Vec<ExecRequest>>,
    resolve_result: Mutex<Option<Result<PathFacts, ExecutionBackendError>>>,
    exec_result: Mutex<Option<Result<ExecOutput, ExecutionBackendError>>>,
    changed_paths_result: Mutex<Option<Result<Vec<WorkspacePath>, ExecutionBackendError>>>,
}

impl CommandSpyBackend {
    fn cwd_facts(path: &str) -> PathFacts {
        PathFacts {
            normalized_path: path.to_string(),
            exists: true,
            kind: Some(crate::execution_backend::PathKind::Directory),
            parent_exists: true,
            parent_kind: Some(crate::execution_backend::PathKind::Directory),
            byte_size: None,
            canonical_under_workspace: true,
        }
    }
}

impl ExecutionBackend for CommandSpyBackend {
    fn backend_name(&self) -> &str {
        "command-spy"
    }

    fn resolve_path(
        &self,
        path: &WorkspacePath,
        intent: PathIntent,
    ) -> Pin<Box<dyn Future<Output = Result<PathFacts, ExecutionBackendError>> + Send + '_>> {
        self.resolve_calls
            .lock()
            .unwrap()
            .push((path.as_str().to_string(), intent));
        let result = self
            .resolve_result
            .lock()
            .unwrap()
            .clone()
            .unwrap_or_else(|| Ok(Self::cwd_facts(path.as_str())));
        Box::pin(async move { result })
    }

    fn read_file(
        &self,
        _path: &WorkspacePath,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<u8>, ExecutionBackendError>> + Send + '_>> {
        Box::pin(async {
            Err(ExecutionBackendError::Unavailable {
                message: "not used".into(),
            })
        })
    }

    fn write_file(
        &self,
        _path: &WorkspacePath,
        _content: &[u8],
    ) -> Pin<Box<dyn Future<Output = Result<(), ExecutionBackendError>> + Send + '_>> {
        Box::pin(async {
            Err(ExecutionBackendError::Unavailable {
                message: "not used".into(),
            })
        })
    }

    fn remove_file(
        &self,
        _path: &WorkspacePath,
    ) -> Pin<Box<dyn Future<Output = Result<(), ExecutionBackendError>> + Send + '_>> {
        Box::pin(async {
            Err(ExecutionBackendError::Unavailable {
                message: "not used".into(),
            })
        })
    }

    fn list_dir(
        &self,
        _path: &WorkspacePath,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<DirectoryEntry>, ExecutionBackendError>> + Send + '_>>
    {
        Box::pin(async {
            Err(ExecutionBackendError::Unavailable {
                message: "not used".into(),
            })
        })
    }

    fn grep(
        &self,
        _request: &GrepRequest,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<GrepMatch>, ExecutionBackendError>> + Send + '_>>
    {
        Box::pin(async {
            Err(ExecutionBackendError::Unavailable {
                message: "not used".into(),
            })
        })
    }

    fn find_files(
        &self,
        _request: &FindRequest,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<WorkspacePath>, ExecutionBackendError>> + Send + '_>>
    {
        Box::pin(async {
            Err(ExecutionBackendError::Unavailable {
                message: "not used".into(),
            })
        })
    }

    fn exec(
        &self,
        request: &ExecRequest,
    ) -> Pin<Box<dyn Future<Output = Result<ExecOutput, ExecutionBackendError>> + Send + '_>> {
        self.exec_calls.lock().unwrap().push(request.clone());
        let result = self
            .exec_result
            .lock()
            .unwrap()
            .clone()
            .unwrap_or(Ok(ExecOutput {
                exit_code: 0,
                stdout: "ok".into(),
                stderr: String::new(),
            }));
        Box::pin(async move { result })
    }

    fn changed_paths(
        &self,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<WorkspacePath>, ExecutionBackendError>> + Send + '_>>
    {
        let result = self
            .changed_paths_result
            .lock()
            .unwrap()
            .clone()
            .unwrap_or(Ok(Vec::new()));
        Box::pin(async move { result })
    }
}

// ── Helpers ──────────────────────────────────────────────────────────────────

fn routing_with_spy() -> (ExecutionRouting, Arc<CommandSpyBackend>) {
    let spy = Arc::new(CommandSpyBackend::default());
    let routing = ExecutionRouting {
        backend: spy.clone(),
        upper: UpperContext {
            execution_backend: "command-spy".into(),
            upper_workspace_id: "upper-command".into(),
        },
    };
    (routing, spy)
}

fn direct_policy_config(root: &std::path::Path) -> lancet_authority::policy::default::PolicyConfig {
    let mut config = lancet_authority::policy::default::PolicyConfig::defaults_for_project_root(
        root.to_path_buf(),
    );
    config.workspace.allowed_write_paths.push("src/**".into());
    config
        .command
        .allowed_diagnostic_commands
        .push(vec!["echo".into()]);
    config
}

fn direct_ctx_with_policy(root: &str, routing: ExecutionRouting) -> HandlerContext {
    direct_ctx_with_policy_mode(root, routing, lancet_protocol::policy::CepMode::Off)
}

fn direct_ctx_with_policy_mode(
    root: &str,
    routing: ExecutionRouting,
    cep_mode: lancet_protocol::policy::CepMode,
) -> HandlerContext {
    let (mut ctx, _, _) = test_ctx(root);
    ctx.execution = Some(routing);
    ctx.cep_mode = cep_mode;
    let config = direct_policy_config(std::path::Path::new(root));
    let hook = Arc::new(lancet_authority::policy::default::DefaultPolicyHook::new(
        config.clone(),
    ));
    ctx.policy_hook = hook.clone();
    ctx.default_policy_hook = Some(hook);
    ctx.cep_context = Arc::new(CepTurnContext::from_policy(config.to_cep_policy_config()));
    ctx
}

fn set_spy_changed_paths(spy: &CommandSpyBackend, paths: Vec<&str>) {
    let parsed: Vec<WorkspacePath> = paths
        .into_iter()
        .map(|p| WorkspacePath::parse(p).unwrap())
        .collect();
    *spy.changed_paths_result.lock().unwrap() = Some(Ok(parsed));
}

async fn dispatch_direct(
    registry: &MethodRegistry,
    ctx: HandlerContext,
    params: RunParams,
) -> Result<serde_json::Value, CepError> {
    registry
        .dispatch(Run::NAME, serde_json::to_value(&params).unwrap(), ctx)
        .await
}

fn routing_metadata(
    durable: &crate::server::call_durable_metadata::CallDurableMetadata,
) -> serde_json::Value {
    serde_json::from_str(&durable.take_json().expect("metadata json")).unwrap()
}

fn run_params(argv: Vec<&str>, impact: Option<ImpactKind>) -> RunParams {
    RunParams {
        argv: argv.into_iter().map(String::from).collect(),
        cwd: None,
        env: BTreeMap::new(),
        timeout_ms: None,
        declared_impact: impact,
    }
}

// ── Ported phase34_direct_command tests (top-level) ──────────────────────────

#[tokio::test]
async fn direct_command_routes_backend_only() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path().to_str().unwrap();
    let registry = native_command_registry(root);
    let (routing, spy) = routing_with_spy();
    let ctx = direct_ctx_with_policy(root, routing);
    let durable = ctx.durable_metadata.clone();

    let result = dispatch_direct(
        &registry,
        ctx,
        run_params(vec!["echo", "upper"], Some(ImpactKind::CommandExecution)),
    )
    .await
    .unwrap();

    assert_eq!(result["exit_code"], 0);
    assert_eq!(spy.exec_calls.lock().unwrap().len(), 1);
    let meta = routing_metadata(&durable);
    assert_eq!(meta["adapter_preflight_invoked"], true);
    assert_eq!(meta["backend_invoked"], true);
    assert!(meta.get("rejection_stage").is_none());
}

#[tokio::test]
async fn direct_command_preserves_argv() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path().to_str().unwrap();
    let registry = native_command_registry(root);
    let (routing, spy) = routing_with_spy();
    let ctx = direct_ctx_with_policy(root, routing);
    let token = "spaces and; metacharacters".to_string();

    dispatch_direct(
        &registry,
        ctx,
        RunParams {
            argv: vec!["echo".into(), token.clone()],
            cwd: None,
            env: BTreeMap::new(),
            timeout_ms: None,
            declared_impact: Some(ImpactKind::CommandExecution),
        },
    )
    .await
    .unwrap();

    assert_eq!(spy.exec_calls.lock().unwrap()[0].argv[1], token);
}

#[tokio::test]
async fn direct_command_policy_denial_precedes_adapter() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path().to_str().unwrap();
    let registry = native_command_registry(root);
    let (routing, spy) = routing_with_spy();
    let ctx = direct_ctx_with_policy(root, routing);
    let durable = ctx.durable_metadata.clone();

    let err = dispatch_direct(
        &registry,
        ctx,
        run_params(
            vec!["sh", "-c", "echo bad"],
            Some(ImpactKind::CommandExecution),
        ),
    )
    .await
    .unwrap_err();

    assert_eq!(err.code, CepErrorCode::GuardDenied);
    assert!(spy.resolve_calls.lock().unwrap().is_empty());
    assert!(spy.exec_calls.lock().unwrap().is_empty());
    let meta = routing_metadata(&durable);
    assert_eq!(meta["adapter_preflight_invoked"], false);
    assert_eq!(meta["backend_invoked"], false);
    assert_eq!(meta["rejection_stage"], "policy_gate");
}

#[tokio::test]
async fn direct_command_cwd_denial_preflight_only() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path().to_str().unwrap();
    let registry = native_command_registry(root);
    let (routing, spy) = routing_with_spy();
    *spy.resolve_result.lock().unwrap() = Some(Ok(PathFacts {
        normalized_path: ".git".into(),
        exists: true,
        kind: Some(crate::execution_backend::PathKind::Directory),
        parent_exists: true,
        parent_kind: Some(crate::execution_backend::PathKind::Directory),
        byte_size: None,
        canonical_under_workspace: true,
    }));
    let ctx = direct_ctx_with_policy(root, routing);
    let durable = ctx.durable_metadata.clone();

    let err = dispatch_direct(
        &registry,
        ctx,
        RunParams {
            argv: vec!["echo".into(), "cwd-deny".into()],
            cwd: Some(".git".into()),
            env: BTreeMap::new(),
            timeout_ms: None,
            declared_impact: Some(ImpactKind::CommandExecution),
        },
    )
    .await
    .unwrap_err();

    assert_eq!(err.code, CepErrorCode::GuardDenied);
    assert!(spy.exec_calls.lock().unwrap().is_empty());
    let meta = routing_metadata(&durable);
    assert_eq!(meta["adapter_preflight_invoked"], true);
    assert_eq!(meta["backend_invoked"], false);
    assert_eq!(meta["rejection_stage"], "adapter_preflight");
}

#[tokio::test]
async fn direct_command_backend_failure_no_fallback() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path().to_str().unwrap();
    let sentinel = dir.path().join("lower-sentinel.txt");
    std::fs::write(&sentinel, "unchanged").unwrap();
    let before = std::fs::metadata(&sentinel).unwrap().modified().unwrap();

    let registry = native_command_registry(root);
    let (routing, spy) = routing_with_spy();
    *spy.exec_result.lock().unwrap() = Some(Err(ExecutionBackendError::ExecutionFailed {
        message: "adapter blew up".into(),
    }));
    let ctx = direct_ctx_with_policy(root, routing);
    let durable = ctx.durable_metadata.clone();

    let err = dispatch_direct(
        &registry,
        ctx,
        run_params(vec!["echo", "fail"], Some(ImpactKind::CommandExecution)),
    )
    .await
    .unwrap_err();

    assert_eq!(err.code, CepErrorCode::ExecutionFailed);
    let after = std::fs::metadata(&sentinel).unwrap().modified().unwrap();
    assert_eq!(before, after);
    let meta = routing_metadata(&durable);
    assert_eq!(meta["adapter_preflight_invoked"], true);
    assert_eq!(meta["backend_invoked"], true);
    assert_eq!(meta["rejection_stage"], "backend_execution");
}

#[tokio::test]
async fn direct_command_root_cwd_passes_write_scope_preflight() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path().to_str().unwrap();
    let registry = native_command_registry(root);
    let (routing, spy) = routing_with_spy();
    let ctx = direct_ctx_with_policy(root, routing);

    dispatch_direct(
        &registry,
        ctx,
        RunParams {
            argv: vec!["echo".into(), "root-cwd".into()],
            cwd: Some(".".into()),
            env: BTreeMap::new(),
            timeout_ms: None,
            declared_impact: Some(ImpactKind::WorkspaceMutation),
        },
    )
    .await
    .unwrap();

    assert_eq!(spy.exec_calls.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn direct_command_cwd_outside_write_region_denied() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path().to_str().unwrap();
    let registry = native_command_registry(root);
    let (routing, spy) = routing_with_spy();
    let ctx = direct_ctx_with_policy(root, routing);
    let durable = ctx.durable_metadata.clone();

    let err = dispatch_direct(
        &registry,
        ctx,
        RunParams {
            argv: vec!["echo".into(), "outside-scope".into()],
            cwd: Some("tests".into()),
            env: BTreeMap::new(),
            timeout_ms: None,
            declared_impact: Some(ImpactKind::WorkspaceMutation),
        },
    )
    .await
    .unwrap_err();

    assert_eq!(err.code, CepErrorCode::GuardDenied);
    assert!(spy.exec_calls.lock().unwrap().is_empty());
    let meta = routing_metadata(&durable);
    assert_eq!(meta["adapter_preflight_invoked"], true);
    assert_eq!(meta["backend_invoked"], false);
    assert_eq!(meta["rejection_stage"], "adapter_preflight");
}

#[tokio::test]
async fn direct_verify_root_cwd_passes_write_scope_preflight() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path().to_str().unwrap();
    let registry = native_command_registry(root);
    let (routing, spy) = routing_with_spy();

    let config = direct_policy_config(std::path::Path::new(root));
    let hook = Arc::new(lancet_authority::policy::default::DefaultPolicyHook::new(
        config.clone(),
    ));
    let mut ctx = direct_ctx_with_policy(root, routing);
    ctx.policy_hook = hook.clone();
    ctx.default_policy_hook = Some(hook);
    ctx.cep_context = Arc::new(CepTurnContext::from_policy(config.to_cep_policy_config()));
    ctx.direct_verify = Some(DirectVerifyConfig {
        argv: vec!["echo".into(), "verify-root".into()],
        cwd: WorkspacePath::project_root(),
        timeout_ms: None,
        expected_exit_code: 0,
    });

    dispatch_direct(
        &registry,
        ctx,
        RunParams {
            argv: vec!["echo".into(), "mutate-root".into()],
            cwd: Some(".".into()),
            env: BTreeMap::new(),
            timeout_ms: None,
            declared_impact: Some(ImpactKind::WorkspaceMutation),
        },
    )
    .await
    .unwrap();

    let exec_calls = spy.exec_calls.lock().unwrap();
    assert_eq!(exec_calls.len(), 2);
    assert_eq!(exec_calls[0].argv, vec!["echo", "mutate-root"]);
    assert_eq!(exec_calls[1].argv, vec!["echo", "verify-root"]);
}

#[tokio::test]
async fn direct_verify_uses_same_backend() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path().to_str().unwrap();
    let registry = native_command_registry(root);
    let (routing, spy) = routing_with_spy();

    let config = direct_policy_config(std::path::Path::new(root));
    let hook = Arc::new(lancet_authority::policy::default::DefaultPolicyHook::new(
        config.clone(),
    ));
    let mut ctx = direct_ctx_with_policy(root, routing);
    ctx.policy_hook = hook.clone();
    ctx.default_policy_hook = Some(hook);
    ctx.cep_context = Arc::new(CepTurnContext::from_policy(config.to_cep_policy_config()));
    ctx.direct_verify = Some(DirectVerifyConfig {
        argv: vec!["echo".into(), "verify-ok".into()],
        cwd: WorkspacePath::project_root(),
        timeout_ms: None,
        expected_exit_code: 0,
    });

    dispatch_direct(
        &registry,
        ctx,
        run_params(vec!["echo", "mutate"], Some(ImpactKind::WorkspaceMutation)),
    )
    .await
    .unwrap();

    let exec_calls = spy.exec_calls.lock().unwrap();
    assert_eq!(exec_calls.len(), 2);
    assert_eq!(exec_calls[0].argv, vec!["echo", "mutate"]);
    assert_eq!(exec_calls[1].argv, vec!["echo", "verify-ok"]);
}

#[tokio::test]
async fn direct_command_out_of_scope_denied() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path().to_str().unwrap();
    let registry = native_command_registry(root);
    let (routing, spy) = routing_with_spy();
    set_spy_changed_paths(&spy, vec!["tests/out.rs"]);
    let mut ctx =
        direct_ctx_with_policy_mode(root, routing, lancet_protocol::policy::CepMode::Enforce);
    ctx.direct_verify = Some(DirectVerifyConfig {
        argv: vec!["echo".into(), "verify-should-not-run".into()],
        cwd: WorkspacePath::project_root(),
        timeout_ms: None,
        expected_exit_code: 0,
    });
    let durable = ctx.durable_metadata.clone();

    let err = dispatch_direct(
        &registry,
        ctx,
        run_params(
            vec!["echo", "mutate-outside"],
            Some(ImpactKind::WorkspaceMutation),
        ),
    )
    .await
    .unwrap_err();

    assert_eq!(err.code, CepErrorCode::ReplanRequired);
    assert_eq!(spy.exec_calls.lock().unwrap().len(), 1);
    let meta = routing_metadata(&durable);
    assert!(
        meta["out_of_scope_paths"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v.as_str() == Some("tests/out.rs"))
    );
}

#[tokio::test]
async fn direct_command_in_scope_proceeds() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path().to_str().unwrap();
    let registry = native_command_registry(root);
    let (routing, spy) = routing_with_spy();
    set_spy_changed_paths(&spy, vec!["src/lib.rs"]);
    let mut ctx =
        direct_ctx_with_policy_mode(root, routing, lancet_protocol::policy::CepMode::Enforce);
    ctx.direct_verify = Some(DirectVerifyConfig {
        argv: vec!["echo".into(), "verify-ok".into()],
        cwd: WorkspacePath::project_root(),
        timeout_ms: None,
        expected_exit_code: 0,
    });

    dispatch_direct(
        &registry,
        ctx,
        run_params(
            vec!["echo", "mutate-in-scope"],
            Some(ImpactKind::WorkspaceMutation),
        ),
    )
    .await
    .unwrap();

    assert_eq!(spy.exec_calls.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn direct_command_enforce_per_path_effects_pass() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path().to_str().unwrap();
    let registry = native_command_registry(root);
    let (routing, spy) = routing_with_spy();
    set_spy_changed_paths(&spy, vec!["src/lib.rs"]);
    let ctx = direct_ctx_with_policy_mode(root, routing, lancet_protocol::policy::CepMode::Enforce);
    let ledger = ctx.effect_ledger.clone();
    let authorized = ctx.authorized_region.clone();

    dispatch_direct(
        &registry,
        ctx,
        run_params(
            vec!["echo", "mutate-effects"],
            Some(ImpactKind::WorkspaceMutation),
        ),
    )
    .await
    .unwrap();

    let region = authorized.get().expect("authorized region after dispatch");
    let observed = ledger.snapshot();
    let mutation_effects: Vec<_> = observed
        .iter()
        .filter(|e| e.kind == ImpactKind::WorkspaceMutation)
        .collect();
    assert_eq!(mutation_effects.len(), 1);
    assert!(matches!(
        &mutation_effects[0].scope,
        lancet_authority::effect::ledger::ObservedScope::Path(path)
            if path.to_string_lossy() == "src/lib.rs"
    ));
    for effect in &observed {
        assert!(
            region.contains(effect),
            "effect must be authorized: {effect:?}"
        );
    }
}

#[tokio::test]
async fn direct_command_no_changed_paths_records_command_only() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path().to_str().unwrap();
    let registry = native_command_registry(root);
    let (routing, _spy) = routing_with_spy();
    let ctx = direct_ctx_with_policy_mode(root, routing, lancet_protocol::policy::CepMode::Enforce);
    let ledger = ctx.effect_ledger.clone();

    dispatch_direct(
        &registry,
        ctx,
        run_params(vec!["echo", "no-diff"], Some(ImpactKind::WorkspaceMutation)),
    )
    .await
    .unwrap();

    let observed = ledger.snapshot();
    assert!(
        observed
            .iter()
            .any(|e| e.kind == ImpactKind::CommandExecution),
        "must record CommandExecution"
    );
    assert!(
        !observed
            .iter()
            .any(|e| e.kind == ImpactKind::WorkspaceMutation),
        "empty changed_paths must not record WorkspaceMutation"
    );
}

#[tokio::test]
async fn direct_command_changed_paths_error_denies() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path().to_str().unwrap();
    let registry = native_command_registry(root);
    let (routing, spy) = routing_with_spy();
    *spy.changed_paths_result.lock().unwrap() = Some(Err(ExecutionBackendError::ExecutionFailed {
        message: "git status failed".into(),
    }));
    let ctx = direct_ctx_with_policy(root, routing);
    let durable = ctx.durable_metadata.clone();

    let err = dispatch_direct(
        &registry,
        ctx,
        run_params(
            vec!["echo", "mutate-fail-diff"],
            Some(ImpactKind::WorkspaceMutation),
        ),
    )
    .await
    .unwrap_err();

    assert_eq!(err.code, CepErrorCode::ExecutionFailed);
    let meta = routing_metadata(&durable);
    assert_eq!(meta["rejection_stage"], "backend_execution");
}

// ── New Direct-mode coverage tests ───────────────────────────────────────────

#[tokio::test]
async fn direct_command_classifies_diagnostic_when_impact_undeclared() {
    // `echo` is in the allowed_diagnostic_commands allowlist; with no
    // declared_impact the classifier should treat it as Diagnostic.
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path().to_str().unwrap();
    let registry = native_command_registry(root);
    let (routing, _spy) = routing_with_spy();
    let ctx = direct_ctx_with_policy(root, routing);
    let durable = ctx.durable_metadata.clone();

    let result = dispatch_direct(&registry, ctx, run_params(vec!["echo", "hi"], None))
        .await
        .unwrap();

    assert_eq!(result["exit_code"], 0);
    let meta = routing_metadata(&durable);
    // The execution metadata records the classification label.
    assert_eq!(meta["classification"], "diagnostic");
}

#[tokio::test]
async fn direct_command_classifies_workspace_mutation_when_declared() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path().to_str().unwrap();
    let registry = native_command_registry(root);
    let (routing, _spy) = routing_with_spy();
    let ctx = direct_ctx_with_policy(root, routing);
    let durable = ctx.durable_metadata.clone();

    let result = dispatch_direct(
        &registry,
        ctx,
        run_params(vec!["echo", "mutate"], Some(ImpactKind::WorkspaceMutation)),
    )
    .await
    .unwrap();

    assert_eq!(result["exit_code"], 0);
    let meta = routing_metadata(&durable);
    assert_eq!(meta["classification"], "workspace_mutation");
}

#[tokio::test]
async fn direct_command_hard_denied_records_denial_metadata() {
    // `sh -c` is in the default denied_commands list — must be hard-denied
    // before any adapter/backend invocation.
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path().to_str().unwrap();
    let registry = native_command_registry(root);
    let (routing, spy) = routing_with_spy();
    let ctx = direct_ctx_with_policy(root, routing);
    let durable = ctx.durable_metadata.clone();

    let err = dispatch_direct(
        &registry,
        ctx,
        run_params(
            vec!["sh", "-c", "echo bad"],
            Some(ImpactKind::CommandExecution),
        ),
    )
    .await
    .unwrap_err();

    assert_eq!(err.code, CepErrorCode::GuardDenied);
    assert!(spy.exec_calls.lock().unwrap().is_empty());
    let meta = routing_metadata(&durable);
    // denial_source metadata is recorded for policy-denied commands.
    // In Direct mode, `sh -c` is rejected by the inline shell policy before
    // the hard-deny check, so denial_source is "cep_policy".
    assert_eq!(meta["denial_source"], "cep_policy");
    assert_eq!(meta["backend_invoked"], false);
}

#[tokio::test]
async fn direct_command_cwd_denial_records_guard_l1_rejection() {
    // A cwd inside .git must be rejected at the adapter preflight stage with
    // a guard_l1 denial_source and AdapterPreflight rejection_stage.
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path().to_str().unwrap();
    let registry = native_command_registry(root);
    let (routing, _spy) = routing_with_spy();
    let ctx = direct_ctx_with_policy(root, routing);
    let durable = ctx.durable_metadata.clone();

    let err = dispatch_direct(
        &registry,
        ctx,
        RunParams {
            argv: vec!["echo".into(), "git-cwd".into()],
            cwd: Some(".git".into()),
            env: BTreeMap::new(),
            timeout_ms: None,
            declared_impact: Some(ImpactKind::CommandExecution),
        },
    )
    .await
    .unwrap_err();

    assert_eq!(err.code, CepErrorCode::GuardDenied);
    let meta = routing_metadata(&durable);
    assert_eq!(meta["rejection_stage"], "adapter_preflight");
    assert_eq!(meta["denial_source"], "guard_l1");
}

#[tokio::test]
async fn direct_verify_failure_records_verification_rejection() {
    // When the verify command exits non-zero, the handler must record a
    // VerificationFailure rejection stage and return a verify-failed error.
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path().to_str().unwrap();
    let registry = native_command_registry(root);
    let (routing, spy) = routing_with_spy();
    set_spy_changed_paths(&spy, vec!["src/lib.rs"]);

    // Configure the spy to return exit_code=1 for the verify call (second exec).
    // We use a single exec_result that returns exit 1 for all calls; the
    // mutation call itself succeeds (exit 1 is still a successful exec), and
    // the verify check sees exit 1 != expected 0 → verification failure.
    *spy.exec_result.lock().unwrap() = Some(Ok(ExecOutput {
        exit_code: 1,
        stdout: "fail".into(),
        stderr: String::new(),
    }));

    let mut ctx =
        direct_ctx_with_policy_mode(root, routing, lancet_protocol::policy::CepMode::Enforce);
    ctx.direct_verify = Some(DirectVerifyConfig {
        argv: vec!["echo".into(), "verify-fail".into()],
        cwd: WorkspacePath::project_root(),
        timeout_ms: None,
        expected_exit_code: 0,
    });
    let durable = ctx.durable_metadata.clone();

    let err = dispatch_direct(
        &registry,
        ctx,
        run_params(vec!["echo", "mutate"], Some(ImpactKind::WorkspaceMutation)),
    )
    .await
    .unwrap_err();

    assert_eq!(err.code, CepErrorCode::VerifyFailed);
    let meta = routing_metadata(&durable);
    assert_eq!(meta["rejection_stage"], "verification_failure");
    assert_eq!(meta["denial_source"], "direct_verify");
}

#[tokio::test]
async fn direct_command_records_effect_ledger_on_success() {
    // A successful diagnostic command must record a CommandExecution effect
    // in the ledger.
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path().to_str().unwrap();
    let registry = native_command_registry(root);
    let (routing, _spy) = routing_with_spy();
    let ctx = direct_ctx_with_policy(root, routing);
    let ledger = ctx.effect_ledger.clone();

    dispatch_direct(
        &registry,
        ctx,
        run_params(
            vec!["echo", "effect-test"],
            Some(ImpactKind::CommandExecution),
        ),
    )
    .await
    .unwrap();

    let observed = ledger.snapshot();
    assert!(
        observed
            .iter()
            .any(|e| e.kind == ImpactKind::CommandExecution),
        "diagnostic command must record CommandExecution effect"
    );
}

#[tokio::test]
async fn direct_command_execution_metadata_includes_boundary_fields() {
    // The durable metadata for a successful command must include
    // backend_invoked, policy_enforced, and byte-count fields.
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path().to_str().unwrap();
    let registry = native_command_registry(root);
    let (routing, _spy) = routing_with_spy();
    let ctx = direct_ctx_with_policy(root, routing);
    let durable = ctx.durable_metadata.clone();

    dispatch_direct(
        &registry,
        ctx,
        run_params(vec!["echo", "boundary"], Some(ImpactKind::CommandExecution)),
    )
    .await
    .unwrap();

    let meta = routing_metadata(&durable);
    assert_eq!(meta["backend_invoked"], true);
    assert_eq!(meta["policy_enforced"], true);
    assert!(meta.get("stdout_bytes").is_some());
    assert!(meta.get("stderr_bytes").is_some());
}

#[tokio::test]
async fn direct_command_backend_unavailable_when_no_execution_routing() {
    // With execution = None (host-only), the handler must error before
    // invoking any backend.
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path().to_str().unwrap();
    let registry = native_command_registry(root);
    let (ctx, _, _) = test_ctx(root);
    // No execution routing set — stays None.

    let err = dispatch_direct(
        &registry,
        ctx,
        run_params(
            vec!["echo", "no-backend"],
            Some(ImpactKind::CommandExecution),
        ),
    )
    .await
    .unwrap_err();

    assert_eq!(err.code, CepErrorCode::ExecutionFailed);
}

#[tokio::test]
async fn direct_command_empty_argv_rejected_at_schema() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path().to_str().unwrap();
    let registry = native_command_registry(root);
    let (routing, _spy) = routing_with_spy();
    let ctx = direct_ctx_with_policy(root, routing);

    let err = dispatch_direct(
        &registry,
        ctx,
        RunParams {
            argv: vec![],
            cwd: None,
            env: BTreeMap::new(),
            timeout_ms: None,
            declared_impact: Some(ImpactKind::CommandExecution),
        },
    )
    .await
    .unwrap_err();

    assert_eq!(err.code, CepErrorCode::SchemaMismatch);
}

#[tokio::test]
async fn direct_command_env_nul_value_rejected_at_schema() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path().to_str().unwrap();
    let registry = native_command_registry(root);
    let (routing, _spy) = routing_with_spy();
    let ctx = direct_ctx_with_policy(root, routing);

    let mut env = BTreeMap::new();
    env.insert("BAD".into(), "value\0with-nul".into());

    let err = dispatch_direct(
        &registry,
        ctx,
        RunParams {
            argv: vec!["echo".into(), "env-test".into()],
            cwd: None,
            env,
            timeout_ms: None,
            declared_impact: Some(ImpactKind::CommandExecution),
        },
    )
    .await
    .unwrap_err();

    assert_eq!(err.code, CepErrorCode::SchemaMismatch);
}

#[tokio::test]
async fn direct_command_cancellation_aborts_before_backend() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path().to_str().unwrap();
    let registry = native_command_registry(root);
    let (routing, spy) = routing_with_spy();
    let (mut ctx, _, cancel_tx) = test_ctx(root);
    ctx.execution = Some(routing);
    ctx.cep_mode = lancet_protocol::policy::CepMode::Off;
    let config = direct_policy_config(std::path::Path::new(root));
    let hook = Arc::new(lancet_authority::policy::default::DefaultPolicyHook::new(
        config.clone(),
    ));
    ctx.policy_hook = hook.clone();
    ctx.default_policy_hook = Some(hook);
    ctx.cep_context = Arc::new(CepTurnContext::from_policy(config.to_cep_policy_config()));

    // Cancel before dispatch.
    cancel_tx.send(true).unwrap();

    let err = dispatch_direct(
        &registry,
        ctx,
        run_params(
            vec!["echo", "cancelled"],
            Some(ImpactKind::CommandExecution),
        ),
    )
    .await
    .unwrap_err();

    assert_eq!(err.code, CepErrorCode::ExecutionFailed);
    assert!(spy.exec_calls.lock().unwrap().is_empty());
}
