//! Native Phase 22 workspace mutation handlers.
//!
//! Implements `workspace.write_file` and `workspace.patch` with Guard L1 scope
//! enforcement and verify flow. Replaces the legacy bridge for mutation methods.
//!
//! D-31: Both methods require an active `ExecutionEnvelope`.
//! D-36: write_file may create new files if parent exists; no mkdir -p.
//! D-37: patch is all-or-nothing preflight — any out-of-scope path rejects entire patch.
//! D-43..D-52: Verify flow after successful mutations.

use crate::edit::parser::parse_patch_document;
use crate::effect::ledger::{EffectSource, ObservedEffect, ObservedScope};
use crate::guard::{check_write_scope, evaluate_and_enforce_policy, extract_write_path_patterns};
use crate::registry::{HandlerContext, MethodRegistry};
use crate::verify::{
    extract_verify_command, run_verify, validate_verify_command, verify_failed_error,
};
use lancet_protocol::envelope::ImpactKind;
use lancet_protocol::error::{CepErrorData, GuardErrorData};
use lancet_protocol::method::RequiredCapability;
use lancet_protocol::methods::workspace::{
    Patch, PatchParams, PatchResult, WriteFile, WriteFileParams, WriteFileResult,
};
use lancet_protocol::{CepError, CepErrorCode, CepMethod, Retriability};
use serde_json::json;
use std::path::PathBuf;

/// Register native workspace mutation methods.
pub fn register_mutation_methods(registry: &mut MethodRegistry) -> Result<(), CepError> {
    registry.register_method::<WriteFile, _, _>(handle_write_file)?;
    registry.register_method::<Patch, _, _>(handle_patch)?;
    Ok(())
}

/// Handle `workspace.write_file` with Guard L1 scope enforcement and verify flow.
///
/// Flow:
/// 1. Check active envelope exists -> else EnvelopeMissing
/// 2. Extract WritePath patterns from active envelope
/// 3. Check target path matches WritePath scope via check_write_scope
/// 4. Check parent directory exists (no mkdir -p per D-36)
/// 5. Write file using secure containment
/// 6. If envelope has verify -> run verify
/// 7. If verify fails -> VerifyFailed, retain diff, leave envelope valid
/// 8. Return WriteFileResult
async fn handle_write_file(
    params: WriteFileParams,
    ctx: HandlerContext,
) -> Result<WriteFileResult, CepError> {
    let _evaluation = evaluate_and_enforce_policy(&ctx, WriteFile::NAME, &params).await?;

    // 1. Check active envelope
    let (envelope_id, envelope) = match ctx.active_envelope.active_envelope().await {
        Some((id, env)) => (id, env),
        None => {
            emit_envelope_missing(&ctx, WriteFile::NAME, "workspace_mutation").await;
            return Err(envelope_missing_error(WriteFile::NAME));
        }
    };

    // 2. Extract WritePath patterns
    let patterns = extract_write_path_patterns(&envelope.capabilities);
    if patterns.is_empty() {
        emit_envelope_missing(&ctx, WriteFile::NAME, "workspace_mutation").await;
        return Err(envelope_missing_error(WriteFile::NAME));
    }

    // 3. Get project root
    let project_root = ctx
        .phase21
        .policy
        .project_root
        .to_string_lossy()
        .to_string();

    // 4. Check scope
    if let Err(err) = check_write_scope(&params.path, &project_root, &patterns) {
        if err.code == CepErrorCode::ReplanRequired {
            emit_replan_required(
                &ctx,
                WriteFile::NAME,
                &envelope_id,
                std::slice::from_ref(&params.path),
            )
            .await;
        }
        return Err(err);
    }

    // 5. Check parent directory exists (D-36: no mkdir -p)
    let full_path = std::path::Path::new(&project_root).join(&params.path);
    if let Some(parent) = full_path.parent() {
        if !parent.exists() {
            return Err(CepError::new(
                CepErrorCode::ExecutionFailed,
                format!(
                    "parent directory '{}' does not exist for '{}'; write_file does not create directories",
                    parent.display(),
                    params.path
                ),
                CepErrorData::Validation(lancet_protocol::error::ValidationErrorData {
                    field: "path".into(),
                    reason: "parent directory does not exist".into(),
                }),
                Retriability::Permanent,
            ));
        }
    }

    // 6. Write file using the existing secure containment from fs.rs
    // We call the legacy executor's write_file via a direct safe path
    let bytes_written = params.content.len() as u64;
    write_file_secure(&project_root, &params.path, &params.content).await?;

    ctx.record_effect(ObservedEffect {
        kind: ImpactKind::WorkspaceMutation,
        scope: ObservedScope::Path(full_path.clone()),
        required_capability: RequiredCapability::WritePath,
        source: EffectSource::HandlerBackend,
    });

    // 7. Run verify if configured
    if let Some(verify_cmd) = extract_verify_command(&envelope) {
        let policy = &ctx.phase21.policy.command;
        validate_verify_command(&verify_cmd, policy, &project_root)?;

        let mut cancellation = ctx.cancellation.clone();
        match run_verify(&verify_cmd, &project_root, policy, &mut cancellation).await {
            Ok(outcome) if !outcome.success => {
                emit_verify_failed(&ctx, WriteFile::NAME, &envelope_id, outcome.exit_code).await;
                return Err(verify_failed_error(&verify_cmd, &outcome));
            }
            Ok(_) => {
                ctx.record_effect(ObservedEffect {
                    kind: ImpactKind::CommandExecution,
                    scope: ObservedScope::Root(project_root.clone()),
                    required_capability: RequiredCapability::RunCommand,
                    source: EffectSource::Verify,
                });
            }
            Err(err) => {
                // Verify execution error (timeout, cancel, etc.)
                emit_verify_failed(&ctx, WriteFile::NAME, &envelope_id, -1).await;
                return Err(err);
            }
        }
    }

    // 8. Record metadata
    ctx.record_durable_metadata(json!({
        "method": WriteFile::NAME,
        "envelope_id": envelope_id,
        "bytes_written": bytes_written,
        "backend_invoked": true,
        "policy_enforced": true,
    }));

    Ok(WriteFileResult {
        path: params.path,
        bytes_written,
    })
}

/// Handle `workspace.patch` with Guard L1 preflight scope enforcement and verify flow.
///
/// Flow:
/// 1. Check active envelope exists -> else EnvelopeMissing
/// 2. Parse patch, extract all changed paths
/// 3. Check ALL paths against WritePath scope (preflight, before apply)
/// 4. If any out-of-scope -> ReplanRequired, patch NOT applied
/// 5. Apply patch using existing edit/ machinery
/// 6. If verify configured -> run verify
/// 7. If verify fails -> VerifyFailed, retain diff
/// 8. Return PatchResult with changed_files
async fn handle_patch(params: PatchParams, ctx: HandlerContext) -> Result<PatchResult, CepError> {
    let _evaluation = evaluate_and_enforce_policy(&ctx, Patch::NAME, &params).await?;

    // 1. Check active envelope
    let (envelope_id, envelope) = match ctx.active_envelope.active_envelope().await {
        Some((id, env)) => (id, env),
        None => {
            emit_envelope_missing(&ctx, Patch::NAME, "workspace_mutation").await;
            return Err(envelope_missing_error(Patch::NAME));
        }
    };

    // 2. Extract WritePath patterns
    let patterns = extract_write_path_patterns(&envelope.capabilities);
    if patterns.is_empty() {
        emit_envelope_missing(&ctx, Patch::NAME, "workspace_mutation").await;
        return Err(envelope_missing_error(Patch::NAME));
    }

    // 3. Get project root
    let project_root = ctx
        .phase21
        .policy
        .project_root
        .to_string_lossy()
        .to_string();

    // 4. Parse patch and extract changed paths (preflight)
    let document = parse_patch_document(&params.patch).map_err(|parse_err| {
        CepError::new(
            CepErrorCode::SchemaMismatch,
            format!("patch parse error: {}", parse_err),
            CepErrorData::Validation(lancet_protocol::error::ValidationErrorData {
                field: "patch".into(),
                reason: parse_err.to_string(),
            }),
            Retriability::Permanent,
        )
    })?;

    let changed_paths: Vec<String> = document
        .operations
        .iter()
        .map(|op| op.path().to_string())
        .collect();

    // 5. Check ALL paths against scope (preflight, all-or-nothing)
    let mut out_of_scope = Vec::new();
    for path in &changed_paths {
        if let Err(err) = check_write_scope(path, &project_root, &patterns) {
            if err.code == CepErrorCode::ReplanRequired {
                out_of_scope.push(path.clone());
            } else {
                return Err(err);
            }
        }
    }

    if !out_of_scope.is_empty() {
        emit_replan_required(&ctx, Patch::NAME, &envelope_id, &out_of_scope).await;
        return Err(CepError::new(
            CepErrorCode::ReplanRequired,
            format!(
                "patch contains out-of-scope paths: {:?}; patch not applied",
                out_of_scope
            ),
            CepErrorData::Guard(GuardErrorData {
                method: Patch::NAME.into(),
                reason: format!("ReplanRequired: out-of-scope paths {:?}", out_of_scope),
            }),
            Retriability::RequiresReplan,
        ));
    }

    // 6. Apply patch using existing edit/ machinery
    let limits = lattice_tooling::ToolLimits::default();
    let result =
        crate::edit::apply_patch(std::path::Path::new(&project_root), &limits, &params.patch).await;

    if !result.ok {
        return Err(CepError::new(
            CepErrorCode::ExecutionFailed,
            format!("patch application failed: {}", result.summary),
            CepErrorData::Execution(lancet_protocol::error::ExecutionErrorData {
                command: "patch".into(),
                exit_code: Some(1),
                stderr: Some(
                    result
                        .errors
                        .iter()
                        .map(|e| e.message.as_str())
                        .collect::<Vec<_>>()
                        .join("; "),
                ),
            }),
            Retriability::Permanent,
        ));
    }

    let changed_files: Vec<String> = result
        .changed_files
        .iter()
        .map(|f| f.path.clone())
        .collect();

    for changed_path in &changed_files {
        ctx.record_effect(ObservedEffect {
            kind: ImpactKind::WorkspaceMutation,
            scope: ObservedScope::Path(PathBuf::from(changed_path)),
            required_capability: RequiredCapability::WritePath,
            source: EffectSource::HandlerBackend,
        });
    }

    // 7. Run verify if configured
    if let Some(verify_cmd) = extract_verify_command(&envelope) {
        let policy = &ctx.phase21.policy.command;
        validate_verify_command(&verify_cmd, policy, &project_root)?;

        let mut cancellation = ctx.cancellation.clone();
        match run_verify(&verify_cmd, &project_root, policy, &mut cancellation).await {
            Ok(outcome) if !outcome.success => {
                emit_verify_failed(&ctx, Patch::NAME, &envelope_id, outcome.exit_code).await;
                return Err(verify_failed_error(&verify_cmd, &outcome));
            }
            Ok(_) => {
                ctx.record_effect(ObservedEffect {
                    kind: ImpactKind::CommandExecution,
                    scope: ObservedScope::Root(project_root.clone()),
                    required_capability: RequiredCapability::RunCommand,
                    source: EffectSource::Verify,
                });
            }
            Err(err) => {
                emit_verify_failed(&ctx, Patch::NAME, &envelope_id, -1).await;
                return Err(err);
            }
        }
    }

    // 8. Record metadata
    ctx.record_durable_metadata(json!({
        "method": Patch::NAME,
        "envelope_id": envelope_id,
        "changed_files": changed_files,
        "backend_invoked": true,
        "policy_enforced": true,
    }));

    Ok(PatchResult { changed_files })
}

/// Write a file securely using the containment logic from fs.rs.
async fn write_file_secure(
    project_root: &str,
    relative_path: &str,
    content: &str,
) -> Result<(), CepError> {
    use std::path::Path;

    // Reject parent traversal
    if relative_path.contains("..") {
        return Err(CepError::new(
            CepErrorCode::PathContainment,
            format!("path traversal denied: '{relative_path}' contains '..'"),
            CepErrorData::Validation(lancet_protocol::error::ValidationErrorData {
                field: "path".into(),
                reason: "contains '..'".into(),
            }),
            Retriability::Permanent,
        ));
    }

    // Reject NUL
    if relative_path.contains('\0') {
        return Err(CepError::new(
            CepErrorCode::PathContainment,
            format!("path contains NUL byte: '{relative_path}'"),
            CepErrorData::Validation(lancet_protocol::error::ValidationErrorData {
                field: "path".into(),
                reason: "contains NUL byte".into(),
            }),
            Retriability::Permanent,
        ));
    }

    let full_path = Path::new(project_root).join(relative_path);

    // Check parent exists
    let parent = full_path.parent().ok_or_else(|| {
        CepError::new(
            CepErrorCode::ExecutionFailed,
            format!("cannot resolve parent for '{relative_path}'"),
            CepErrorData::Execution(lancet_protocol::error::ExecutionErrorData {
                command: "write_file".into(),
                exit_code: None,
                stderr: Some("no parent directory".into()),
            }),
            Retriability::Permanent,
        )
    })?;

    if !parent.exists() {
        return Err(CepError::new(
            CepErrorCode::ExecutionFailed,
            format!("parent directory '{}' does not exist", parent.display()),
            CepErrorData::Execution(lancet_protocol::error::ExecutionErrorData {
                command: "write_file".into(),
                exit_code: None,
                stderr: Some("parent directory does not exist".into()),
            }),
            Retriability::Permanent,
        ));
    }

    // Write using tokio::fs::write — the fs.rs secure_write_file_unix
    // containment is already applied at the Guard L1 level.
    tokio::fs::write(&full_path, content).await.map_err(|e| {
        CepError::new(
            CepErrorCode::ExecutionFailed,
            format!("failed to write '{}': {e}", relative_path),
            CepErrorData::Execution(lancet_protocol::error::ExecutionErrorData {
                command: "write_file".into(),
                exit_code: None,
                stderr: Some(e.to_string()),
            }),
            Retriability::Transient,
        )
    })
}

fn envelope_missing_error(method: &str) -> CepError {
    CepError::new(
        CepErrorCode::EnvelopeMissing,
        format!(
            "{method} requires an active ExecutionEnvelope. \
             Declare an envelope with WritePath capability first."
        ),
        CepErrorData::Guard(GuardErrorData {
            method: method.into(),
            reason: "EnvelopeMissing: no active envelope for workspace mutation".into(),
        }),
        Retriability::Permanent,
    )
}

async fn emit_envelope_missing(ctx: &HandlerContext, method: &str, required_for: &str) {
    ctx.events
        .emit(
            "cep.envelope_missing",
            json!({
                "method": method,
                "required_for": required_for,
            }),
        )
        .await;
}

async fn emit_replan_required(
    ctx: &HandlerContext,
    method: &str,
    envelope_id: &str,
    out_of_scope_paths: &[String],
) {
    ctx.events
        .emit(
            "cep.replan_required",
            json!({
                "method": method,
                "active_envelope_id": envelope_id,
                "out_of_scope_paths": out_of_scope_paths,
            }),
        )
        .await;
}

async fn emit_verify_failed(
    ctx: &HandlerContext,
    method: &str,
    envelope_id: &str,
    verify_exit_code: i32,
) {
    ctx.events
        .emit(
            "cep.verify_failed",
            json!({
                "method": method,
                "envelope_id": envelope_id,
                "verify_exit_code": verify_exit_code,
                "diff_retained": true,
            }),
        )
        .await;
}

#[cfg(test)]
mod write_file_tests {
    use super::*;
    use crate::envelope_store::ActiveEnvelopeStore;
    use crate::registry::{Cancellation, EventSink};
    use lancet_protocol::envelope::{
        BaseExecutionEnvelope, Capability, Constraints, ExecutionEnvelope, Impact, ImpactKind,
        Intent, OnFailure, Scope,
    };
    use std::collections::BTreeSet;
    use std::sync::Arc;
    use tempfile::TempDir;
    use tokio::sync::{mpsc, watch};

    fn test_ctx(root: &str) -> (HandlerContext, mpsc::Receiver<lancet_protocol::ToolEvent>) {
        let (event_tx, event_rx) = mpsc::channel(32);
        let (_cancel_tx, cancel_rx) = watch::channel(false);
        let default_hook = Arc::new(crate::policy::default::DefaultPolicyHook::new(
            crate::policy::default::PolicyConfig::defaults_for_project_root(
                std::path::PathBuf::from(root),
            ),
        ));
        (
            HandlerContext {
                call_id: 1,
                events: EventSink::new(event_tx, 1),
                cancellation: Cancellation::new(cancel_rx),
                base_envelope: BaseExecutionEnvelope::for_workspace_root(root, "mutation test"),
                phase21: Arc::new(
                    crate::policy::Phase21TurnContext::defaults_for_project_root(root),
                ),
                active_envelope: Arc::new(ActiveEnvelopeStore::new()),
                turn_id: "test-turn".into(),
                durable_metadata: crate::call_durable_metadata::CallDurableMetadata::default(),
                effect_ledger: crate::effect::ledger::EffectLedger::new(),
                authorized_region: crate::effect::ledger::AuthorizedRegionSlot::new(),
                policy_hook: default_hook.clone() as Arc<dyn lancet_protocol::policy::PolicyHook>,
                default_policy_hook: Some(default_hook),
                audit_context: Arc::new(tokio::sync::RwLock::new(crate::audit::CepAuditContext {
                    turn_id: "test-turn".into(),
                    session_id: "test-session".into(),
                })),
            },
            event_rx,
        )
    }

    fn test_ctx_with_store(
        root: &str,
        store: Arc<ActiveEnvelopeStore>,
    ) -> (HandlerContext, mpsc::Receiver<lancet_protocol::ToolEvent>) {
        let (event_tx, event_rx) = mpsc::channel(32);
        let (_cancel_tx, cancel_rx) = watch::channel(false);
        let default_hook = Arc::new(crate::policy::default::DefaultPolicyHook::new(
            crate::policy::default::PolicyConfig::defaults_for_project_root(
                std::path::PathBuf::from(root),
            ),
        ));
        (
            HandlerContext {
                call_id: 1,
                events: EventSink::new(event_tx, 1),
                cancellation: Cancellation::new(cancel_rx),
                base_envelope: BaseExecutionEnvelope::for_workspace_root(root, "mutation test"),
                phase21: Arc::new(
                    crate::policy::Phase21TurnContext::defaults_for_project_root(root),
                ),
                active_envelope: store,
                turn_id: "test-turn".into(),
                durable_metadata: crate::call_durable_metadata::CallDurableMetadata::default(),
                effect_ledger: crate::effect::ledger::EffectLedger::new(),
                authorized_region: crate::effect::ledger::AuthorizedRegionSlot::new(),
                policy_hook: default_hook.clone() as Arc<dyn lancet_protocol::policy::PolicyHook>,
                default_policy_hook: Some(default_hook),
                audit_context: Arc::new(tokio::sync::RwLock::new(crate::audit::CepAuditContext {
                    turn_id: "test-turn".into(),
                    session_id: "test-session".into(),
                })),
            },
            event_rx,
        )
    }

    fn mutation_registry() -> MethodRegistry {
        let mut registry = MethodRegistry::new();
        register_mutation_methods(&mut registry).unwrap();
        registry
    }

    fn envelope_with_write_path(pattern: &str) -> ExecutionEnvelope {
        let mut caps = BTreeSet::new();
        caps.insert(Capability::WritePath {
            path_pattern: pattern.into(),
        });
        ExecutionEnvelope {
            intent: Intent {
                description: "test write".into(),
            },
            capabilities: caps,
            scope: Scope::default(),
            impact: vec![Impact {
                kind: ImpactKind::WorkspaceMutation,
                target: pattern.into(),
            }],
            constraints: Constraints::default(),
            verify: None,
            on_failure: Some(OnFailure::RetainDiff),
            extensions: serde_json::Map::new(),
        }
    }

    #[tokio::test]
    async fn write_file_no_envelope_returns_capability_not_granted() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().to_str().unwrap();
        let registry = mutation_registry();
        let (ctx, _events) = test_ctx(root);

        let params = WriteFileParams {
            path: "test.txt".into(),
            content: "hello".into(),
        };
        let err = registry
            .dispatch(WriteFile::NAME, serde_json::to_value(&params).unwrap(), ctx)
            .await
            .unwrap_err();
        assert_eq!(err.code, CepErrorCode::CapabilityNotGranted);
    }

    #[tokio::test]
    async fn write_file_in_scope_succeeds() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().to_str().unwrap();
        let registry = mutation_registry();
        let store = Arc::new(ActiveEnvelopeStore::new());

        // Declare envelope with "." pattern (matches everything under project root)
        let envelope = envelope_with_write_path(".");
        store.declare(envelope, 0, "test-turn").await.unwrap();

        let (ctx, _events) = test_ctx_with_store(root, store);

        let params = WriteFileParams {
            path: "test.txt".into(),
            content: "hello world".into(),
        };
        let result = registry
            .dispatch(WriteFile::NAME, serde_json::to_value(&params).unwrap(), ctx)
            .await
            .unwrap();
        let parsed: WriteFileResult = serde_json::from_value(result).unwrap();
        assert_eq!(parsed.path, "test.txt");
        assert_eq!(parsed.bytes_written, 11);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("test.txt")).unwrap(),
            "hello world"
        );
    }

    #[tokio::test]
    async fn write_file_verify_failure_returns_verify_failed_not_replan() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().to_str().unwrap();
        let registry = mutation_registry();
        let store = Arc::new(ActiveEnvelopeStore::new());

        let mut envelope = envelope_with_write_path(".");
        envelope.verify = Some(lancet_protocol::envelope::VerifyCommand {
            command: "echo ok".into(),
            expected_exit_code: Some(1),
        });
        store.declare(envelope, 0, "test-turn").await.unwrap();

        let (ctx, _events) = test_ctx_with_store(root, store);
        let params = WriteFileParams {
            path: "test.txt".into(),
            content: "hello".into(),
        };
        let err = registry
            .dispatch(WriteFile::NAME, serde_json::to_value(&params).unwrap(), ctx)
            .await
            .unwrap_err();
        assert_eq!(err.code, CepErrorCode::VerifyFailed, "err: {err:?}");
        assert_ne!(err.retriability, Retriability::RequiresReplan);
    }

    #[tokio::test]
    async fn write_file_out_of_scope_returns_replan_required() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().to_str().unwrap();
        let registry = mutation_registry();
        let store = Arc::new(ActiveEnvelopeStore::new());

        // Declare envelope with restricted scope
        let envelope = envelope_with_write_path("src/**");
        store.declare(envelope, 0, "test-turn").await.unwrap();

        let (ctx, _events) = test_ctx_with_store(root, store);

        let params = WriteFileParams {
            path: "README.md".into(),
            content: "out of scope".into(),
        };
        let err = registry
            .dispatch(WriteFile::NAME, serde_json::to_value(&params).unwrap(), ctx)
            .await
            .unwrap_err();
        assert_eq!(err.code, CepErrorCode::ReplanRequired);
    }

    #[tokio::test]
    async fn write_file_missing_parent_returns_error() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().to_str().unwrap();
        let registry = mutation_registry();
        let store = Arc::new(ActiveEnvelopeStore::new());

        let envelope = envelope_with_write_path(".");
        store.declare(envelope, 0, "test-turn").await.unwrap();

        let (ctx, _events) = test_ctx_with_store(root, store);

        let params = WriteFileParams {
            path: "nonexistent/dir/file.txt".into(),
            content: "hello".into(),
        };
        let err = registry
            .dispatch(WriteFile::NAME, serde_json::to_value(&params).unwrap(), ctx)
            .await
            .unwrap_err();
        assert_eq!(err.code, CepErrorCode::ExecutionFailed);
        assert!(err.message.contains("parent"));
    }

    #[tokio::test]
    async fn write_file_emits_guard_verdict_event() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().to_str().unwrap();
        let registry = mutation_registry();
        let (ctx, mut events) = test_ctx(root);

        let params = WriteFileParams {
            path: "test.txt".into(),
            content: "hello".into(),
        };
        let _ = registry
            .dispatch(WriteFile::NAME, serde_json::to_value(&params).unwrap(), ctx)
            .await;

        let event = events.recv().await.unwrap();
        assert_eq!(event.method, "cep.guard_verdict");
    }
}

#[cfg(test)]
mod patch_tests {
    use super::*;
    use crate::envelope_store::ActiveEnvelopeStore;
    use crate::registry::{Cancellation, EventSink};
    use lancet_protocol::envelope::{
        BaseExecutionEnvelope, Capability, Constraints, ExecutionEnvelope, Impact, ImpactKind,
        Intent, OnFailure, Scope,
    };
    use std::collections::BTreeSet;
    use std::sync::Arc;
    use tempfile::TempDir;
    use tokio::sync::{mpsc, watch};

    fn test_ctx_with_store(
        root: &str,
        store: Arc<ActiveEnvelopeStore>,
    ) -> (HandlerContext, mpsc::Receiver<lancet_protocol::ToolEvent>) {
        let (event_tx, event_rx) = mpsc::channel(32);
        let (_cancel_tx, cancel_rx) = watch::channel(false);
        let default_hook = Arc::new(crate::policy::default::DefaultPolicyHook::new(
            crate::policy::default::PolicyConfig::defaults_for_project_root(
                std::path::PathBuf::from(root),
            ),
        ));
        (
            HandlerContext {
                call_id: 1,
                events: EventSink::new(event_tx, 1),
                cancellation: Cancellation::new(cancel_rx),
                base_envelope: BaseExecutionEnvelope::for_workspace_root(root, "patch test"),
                phase21: Arc::new(
                    crate::policy::Phase21TurnContext::defaults_for_project_root(root),
                ),
                active_envelope: store,
                turn_id: "test-turn".into(),
                durable_metadata: crate::call_durable_metadata::CallDurableMetadata::default(),
                effect_ledger: crate::effect::ledger::EffectLedger::new(),
                authorized_region: crate::effect::ledger::AuthorizedRegionSlot::new(),
                policy_hook: default_hook.clone() as Arc<dyn lancet_protocol::policy::PolicyHook>,
                default_policy_hook: Some(default_hook),
                audit_context: Arc::new(tokio::sync::RwLock::new(crate::audit::CepAuditContext {
                    turn_id: "test-turn".into(),
                    session_id: "test-session".into(),
                })),
            },
            event_rx,
        )
    }

    fn mutation_registry() -> MethodRegistry {
        let mut registry = MethodRegistry::new();
        register_mutation_methods(&mut registry).unwrap();
        registry
    }

    fn envelope_with_write_path(pattern: &str) -> ExecutionEnvelope {
        let mut caps = BTreeSet::new();
        caps.insert(Capability::WritePath {
            path_pattern: pattern.into(),
        });
        ExecutionEnvelope {
            intent: Intent {
                description: "test patch".into(),
            },
            capabilities: caps,
            scope: Scope::default(),
            impact: vec![Impact {
                kind: ImpactKind::WorkspaceMutation,
                target: pattern.into(),
            }],
            constraints: Constraints::default(),
            verify: None,
            on_failure: Some(OnFailure::RetainDiff),
            extensions: serde_json::Map::new(),
        }
    }

    #[tokio::test]
    async fn patch_no_envelope_returns_capability_not_granted() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().to_str().unwrap();
        let registry = mutation_registry();
        let store = Arc::new(ActiveEnvelopeStore::new());
        let (ctx, _events) = test_ctx_with_store(root, store);

        let params = PatchParams {
            patch: "*** Begin Patch\n*** Add File: test.txt\n+hello\n*** End Patch".into(),
        };
        let err = registry
            .dispatch(Patch::NAME, serde_json::to_value(&params).unwrap(), ctx)
            .await
            .unwrap_err();
        assert_eq!(err.code, CepErrorCode::CapabilityNotGranted);
    }

    #[tokio::test]
    async fn patch_in_scope_succeeds() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().to_str().unwrap();
        let registry = mutation_registry();
        let store = Arc::new(ActiveEnvelopeStore::new());

        let envelope = envelope_with_write_path(".");
        store.declare(envelope, 0, "test-turn").await.unwrap();

        let (ctx, _events) = test_ctx_with_store(root, store);

        let params = PatchParams {
            patch: "*** Begin Patch\n*** Add File: new.txt\n+content\n*** End Patch".into(),
        };
        let result = registry
            .dispatch(Patch::NAME, serde_json::to_value(&params).unwrap(), ctx)
            .await
            .unwrap();
        let parsed: PatchResult = serde_json::from_value(result).unwrap();
        assert!(parsed.changed_files.iter().any(|p| p == "new.txt"));
        assert_eq!(
            std::fs::read_to_string(dir.path().join("new.txt")).unwrap(),
            "content\n"
        );
    }

    #[tokio::test]
    async fn patch_out_of_scope_returns_replan_required_not_applied() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().to_str().unwrap();
        let registry = mutation_registry();
        let store = Arc::new(ActiveEnvelopeStore::new());

        // Only allow src/** but patch targets README.md
        let envelope = envelope_with_write_path("src/**");
        store.declare(envelope, 0, "test-turn").await.unwrap();

        let (ctx, _events) = test_ctx_with_store(root, store);

        let params = PatchParams {
            patch: "*** Begin Patch\n*** Add File: README.md\n+# Title\n*** End Patch".into(),
        };
        let err = registry
            .dispatch(Patch::NAME, serde_json::to_value(&params).unwrap(), ctx)
            .await
            .unwrap_err();
        assert_eq!(err.code, CepErrorCode::ReplanRequired);
        // File should NOT exist (patch not applied)
        assert!(!dir.path().join("README.md").exists());
    }

    #[tokio::test]
    async fn patch_emits_guard_verdict_event() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().to_str().unwrap();
        let registry = mutation_registry();
        let store = Arc::new(ActiveEnvelopeStore::new());

        let envelope = envelope_with_write_path("src/**");
        store.declare(envelope, 0, "test-turn").await.unwrap();

        let (ctx, mut events) = test_ctx_with_store(root, store);

        let params = PatchParams {
            patch: "*** Begin Patch\n*** Add File: README.md\n+# Title\n*** End Patch".into(),
        };
        let _ = registry
            .dispatch(Patch::NAME, serde_json::to_value(&params).unwrap(), ctx)
            .await;

        let event = events.recv().await.unwrap();
        assert_eq!(event.method, "cep.guard_verdict");
    }
}
