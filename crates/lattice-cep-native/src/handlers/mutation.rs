//! Native CEP workspace mutation handlers.
//!
//! Implements `workspace.write_file` and `workspace.patch` with Guard L1 scope
//! enforcement and verify flow.
//! Both methods require an active `ExecutionEnvelope`; `workspace.patch`
//! performs all-or-nothing scope preflight before applying changes.

use crate::edit::content_hash;
use crate::edit::parser::parse_patch_document;
use crate::edit::types::PatchOperation;
use crate::effect::ledger::{EffectSource, ObservedEffect, ObservedScope};
use crate::guard_l1::{
    check_write_scope, evaluate_and_enforce_policy, extract_write_path_patterns,
};
use crate::guard_l2;
use crate::registry::{HandlerContext, MethodRegistry};
use crate::verify::{
    extract_verify_command, run_verify, validate_verify_command, verify_failed_error,
};
use lattice_cep_catalog::method::RequiredCapability;
use lattice_cep_catalog::methods::workspace::{
    Patch, PatchParams, PatchResult, WriteFile, WriteFileParams, WriteFileResult,
};
use lattice_cep_catalog::CepMethod;
use lattice_cep_protocol::envelope::{CepMode, ImpactKind};
use lattice_cep_protocol::error::{CepErrorData, GuardErrorData};
use lattice_cep_protocol::{CepError, CepErrorCode, Retriability};
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
/// 4. Check parent directory exists (`write_file` does not create directories)
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
        .cep_context
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

    // 5. Check parent directory exists; write_file does not create directories.
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
                CepErrorData::Validation(lattice_cep_protocol::error::ValidationErrorData {
                    field: "path".into(),
                    reason: "parent directory does not exist".into(),
                }),
                Retriability::Permanent,
            ));
        }
    }

    // 6. Opaque invariant cards require human review before mutation.
    let cards = ctx.invariant_index.match_target(&params.path);
    if let Some(message) = crate::invariants::opaque_human_review_message(&cards) {
        return Err(crate::invariants::opaque_human_review_error(
            WriteFile::NAME,
            &message,
        ));
    }

    // 7. Guard L2: AST category check for .rs files with invariant cards
    if params.path.ends_with(".rs") && !matches!(ctx.cep_mode, CepMode::Off) {
        let _status = guard_l2::l2_status_for_method(WriteFile::NAME);
        let has_forbidden = cards.iter().any(|c| !c.forbidden.is_empty());
        if has_forbidden {
            let result = guard_l2::check_l2(
                &params.path,
                params.content.as_bytes(),
                &[(0, params.content.len())],
                &cards,
            );
            enforce_l2_result(&ctx, WriteFile::NAME, &params.path, &result)?;
        }
    }

    // 7. Write file using native secure containment.
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
        let policy = &ctx.cep_context.policy.command;
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
    let written_hash = content_hash(params.content.as_bytes());
    ctx.record_durable_metadata(json!({
        "method": WriteFile::NAME,
        "envelope_id": envelope_id,
        "bytes_written": bytes_written,
        "content_hash": written_hash,
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
/// 1. Parse patch, extract all changed paths; malformed patch text is schema
///    error, not a capability/scope failure.
/// 2. Check active envelope exists -> else EnvelopeMissing
/// 3. Check ALL paths against WritePath scope (preflight, before apply)
/// 4. If any out-of-scope -> ReplanRequired, patch NOT applied
/// 5. Apply patch using existing edit/ machinery
/// 6. If verify configured -> run verify
/// 7. If verify fails -> VerifyFailed, retain diff
/// 8. Return PatchResult with changed_files
async fn handle_patch(params: PatchParams, ctx: HandlerContext) -> Result<PatchResult, CepError> {
    let document = parse_patch_document(&params.patch).map_err(|parse_err| {
        CepError::new(
            CepErrorCode::SchemaMismatch,
            format!("patch parse error: {}", parse_err),
            CepErrorData::Validation(lattice_cep_protocol::error::ValidationErrorData {
                field: "patch".into(),
                reason: parse_err.to_string(),
            }),
            Retriability::Permanent,
        )
    })?;

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
        .cep_context
        .policy
        .project_root
        .to_string_lossy()
        .to_string();

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

    // 6. Opaque cards require review before mutation. Structural L2 runs below
    // against post-patch content so new files and introduced AST nodes are visible.
    for operation in &document.operations {
        let path = operation.path();
        let cards = ctx.invariant_index.match_target(path);
        if let Some(message) = crate::invariants::opaque_human_review_message(&cards) {
            return Err(crate::invariants::opaque_human_review_error(
                Patch::NAME,
                &message,
            ));
        }
        if path.ends_with(".rs")
            && !matches!(ctx.cep_mode, CepMode::Off)
            && cards.iter().any(|c| !c.forbidden.is_empty())
        {
            if let Some((before, after)) = projected_patch_content(&project_root, operation)? {
                let ranges = changed_line_byte_ranges(before.as_deref(), &after);
                let result = guard_l2::check_l2(path, after.as_bytes(), &ranges, &cards);
                enforce_l2_result(&ctx, Patch::NAME, path, &result)?;
            }
        }
    }

    // 7. Apply patch using existing edit/ machinery
    let limits = lattice_tooling::ToolLimits::default();
    let result =
        crate::edit::apply_patch(std::path::Path::new(&project_root), &limits, &params.patch).await;

    if !result.ok {
        return Err(CepError::new(
            CepErrorCode::ExecutionFailed,
            format!("patch application failed: {}", result.summary),
            CepErrorData::Execution(lattice_cep_protocol::error::ExecutionErrorData {
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

    let changed_paths: Vec<String> = result
        .changed_files
        .iter()
        .map(|f| f.path.clone())
        .collect();
    let changed_files_meta: Vec<serde_json::Value> = result
        .changed_files
        .iter()
        .map(|f| {
            json!({
                "path": f.path,
                "content_hash": f.hash_after,
            })
        })
        .collect();

    for changed_path in &changed_paths {
        let abs_path = if std::path::Path::new(changed_path).is_relative() {
            PathBuf::from(&project_root).join(changed_path)
        } else {
            PathBuf::from(changed_path)
        };
        ctx.record_effect(ObservedEffect {
            kind: ImpactKind::WorkspaceMutation,
            scope: ObservedScope::Path(abs_path),
            required_capability: RequiredCapability::WritePath,
            source: EffectSource::HandlerBackend,
        });
    }

    // 7. Run verify if configured
    if let Some(verify_cmd) = extract_verify_command(&envelope) {
        let policy = &ctx.cep_context.policy.command;
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
        "changed_files": changed_files_meta,
        "backend_invoked": true,
        "policy_enforced": true,
    }));

    Ok(PatchResult {
        changed_files: changed_paths,
    })
}

fn enforce_l2_result(
    ctx: &HandlerContext,
    method: &str,
    path: &str,
    result: &guard_l2::L2CheckResult,
) -> Result<(), CepError> {
    ctx.record_durable_metadata(json!({
        "l2_status": result.status,
        "l2_path": path,
    }));

    if result.violations.is_empty() {
        return Ok(());
    }

    let reasons: Vec<String> = result
        .violations
        .iter()
        .map(|v| format!("{} {} (card {})", v.category, v.reason, v.card_id))
        .collect();

    match ctx.cep_mode {
        CepMode::Enforce => Err(CepError::new(
            CepErrorCode::GuardDenied,
            format!(
                "L2 guard: forbidden categories in {}: {}",
                path,
                reasons.join("; ")
            ),
            CepErrorData::Guard(GuardErrorData {
                method: method.into(),
                reason: format!("L2 violations: {:?}", reasons),
            }),
            Retriability::Permanent,
        )),
        CepMode::Warn => {
            tracing::warn!(
                method = method,
                path = path,
                violations = ?reasons,
                "L2 guard violations allowed in warn mode"
            );
            ctx.record_durable_metadata(json!({
                "l2_warn_violations": reasons,
                "l2_path": path,
            }));
            Ok(())
        }
        CepMode::Off => Ok(()),
    }
}

fn projected_patch_content(
    project_root: &str,
    operation: &PatchOperation,
) -> Result<Option<(Option<String>, String)>, CepError> {
    match operation {
        PatchOperation::AddFile { lines, .. } => {
            let content = lines.join("\n");
            let after = if lines.is_empty() {
                String::new()
            } else {
                format!("{content}\n")
            };
            Ok(Some((None, after)))
        }
        PatchOperation::UpdateFile { path, hunks, .. } => {
            let full_path = std::path::Path::new(project_root).join(path);
            let before = std::fs::read_to_string(&full_path).map_err(|err| {
                CepError::new(
                    CepErrorCode::ExecutionFailed,
                    format!("cannot read patch target '{path}' for L2 preflight: {err}"),
                    CepErrorData::Execution(lattice_cep_protocol::error::ExecutionErrorData {
                        command: "patch_l2_preflight".into(),
                        exit_code: Some(1),
                        stderr: Some(err.to_string()),
                    }),
                    Retriability::Permanent,
                )
            })?;
            let after =
                crate::edit::apply::apply_hunks(path, &before, hunks, 0).map_err(|diagnostic| {
                    CepError::new(
                        CepErrorCode::SchemaMismatch,
                        format!("patch L2 preflight hunk error: {}", diagnostic.message),
                        CepErrorData::Validation(
                            lattice_cep_protocol::error::ValidationErrorData {
                                field: "patch".into(),
                                reason: diagnostic.message,
                            },
                        ),
                        Retriability::Permanent,
                    )
                })?;
            Ok(Some((Some(before), after)))
        }
        PatchOperation::DeleteFile { .. } => Ok(None),
    }
}

fn changed_line_byte_ranges(before: Option<&str>, after: &str) -> Vec<(usize, usize)> {
    let Some(before) = before else {
        return if after.is_empty() {
            vec![]
        } else {
            vec![(0, after.len())]
        };
    };

    if before == after {
        return vec![];
    }

    let before_lines: Vec<&str> = before.lines().collect();
    let after_lines: Vec<&str> = after.lines().collect();
    let bounds = line_byte_bounds(after);
    let mut ranges = Vec::new();
    let mut current_start: Option<usize> = None;

    for (idx, line) in after_lines.iter().enumerate() {
        let changed = before_lines.get(idx).copied() != Some(*line);
        if changed && current_start.is_none() {
            current_start = Some(
                bounds
                    .get(idx)
                    .map(|(start, _)| *start)
                    .unwrap_or(after.len()),
            );
        } else if !changed && current_start.is_some() {
            let start = current_start.take().unwrap_or(0);
            let end = bounds
                .get(idx.saturating_sub(1))
                .map(|(_, end)| *end)
                .unwrap_or(start);
            ranges.push((start, end));
        }
    }

    if let Some(start) = current_start {
        let end = bounds.last().map(|(_, end)| *end).unwrap_or(after.len());
        ranges.push((start, end));
    }

    if ranges.is_empty() && before != after && !after.is_empty() {
        ranges.push((0, after.len()));
    }

    ranges
}

fn line_byte_bounds(source: &str) -> Vec<(usize, usize)> {
    let mut bounds = Vec::new();
    let mut offset = 0;
    for line in source.split_inclusive('\n') {
        let line_without_newline = line.strip_suffix('\n').unwrap_or(line);
        let start = offset;
        let end = offset + line_without_newline.len();
        bounds.push((start, end));
        offset += line.len();
    }
    if !source.ends_with('\n') && source.is_empty() {
        return vec![];
    }
    bounds
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
            CepErrorData::Validation(lattice_cep_protocol::error::ValidationErrorData {
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
            CepErrorData::Validation(lattice_cep_protocol::error::ValidationErrorData {
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
            CepErrorData::Execution(lattice_cep_protocol::error::ExecutionErrorData {
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
            CepErrorData::Execution(lattice_cep_protocol::error::ExecutionErrorData {
                command: "write_file".into(),
                exit_code: None,
                stderr: Some("parent directory does not exist".into()),
            }),
            Retriability::Permanent,
        ));
    }

    // Write using tokio::fs::write after Guard L1 containment checks.
    tokio::fs::write(&full_path, content).await.map_err(|e| {
        CepError::new(
            CepErrorCode::ExecutionFailed,
            format!("failed to write '{}': {e}", relative_path),
            CepErrorData::Execution(lattice_cep_protocol::error::ExecutionErrorData {
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
    use crate::handlers::register_submit_intent;
    use crate::invariants::InvariantCardIndex;
    use crate::registry::{Cancellation, EventSink};
    use lattice_cep_catalog::methods::envelope::{SubmitIntent, SubmitIntentParams};
    use lattice_cep_protocol::envelope::{
        BaseExecutionEnvelope, Capability, Constraints, ExecutionEnvelope, Impact,
        ImpactConfidence, ImpactKind, Intent, OnFailure, Scope,
    };
    use std::collections::BTreeSet;
    use std::sync::Arc;
    use tempfile::TempDir;
    use tokio::sync::{mpsc, watch};

    fn test_ctx(
        root: &str,
    ) -> (
        HandlerContext,
        mpsc::Receiver<lattice_cep_protocol::ToolEvent>,
    ) {
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
                base_envelope: BaseExecutionEnvelope::for_workspace_root(root, "mutation test")
                    .with_session_state(),
                cep_context: Arc::new(crate::policy::CepTurnContext::defaults_for_project_root(
                    root,
                )),
                active_envelope: Arc::new(ActiveEnvelopeStore::new()),
                turn_id: "test-turn".into(),
                durable_metadata: crate::call_durable_metadata::CallDurableMetadata::default(),
                effect_ledger: crate::effect::ledger::EffectLedger::new(),
                authorized_region: crate::effect::ledger::AuthorizedRegionSlot::new(),
                policy_hook: default_hook.clone()
                    as Arc<dyn lattice_cep_protocol::policy::PolicyHook>,
                default_policy_hook: Some(default_hook),
                audit_context: Arc::new(tokio::sync::RwLock::new(crate::audit::CepAuditContext {
                    turn_id: "test-turn".into(),
                    session_id: "test-session".into(),
                    ..Default::default()
                })),
                audit: Arc::new(crate::audit::CepAuditStore::in_memory().expect("audit store")),
                invariant_index: Arc::new(crate::invariants::InvariantCardIndex::new(vec![])),
                cep_mode: lattice_cep_protocol::envelope::CepMode::Off,
            },
            event_rx,
        )
    }

    fn test_ctx_with_store(
        root: &str,
        store: Arc<ActiveEnvelopeStore>,
    ) -> (
        HandlerContext,
        mpsc::Receiver<lattice_cep_protocol::ToolEvent>,
    ) {
        test_ctx_with_store_and_index(
            root,
            store,
            Arc::new(crate::invariants::InvariantCardIndex::new(vec![])),
            lattice_cep_protocol::envelope::CepMode::Off,
        )
    }

    fn test_ctx_with_store_and_index(
        root: &str,
        store: Arc<ActiveEnvelopeStore>,
        invariant_index: Arc<InvariantCardIndex>,
        cep_mode: CepMode,
    ) -> (
        HandlerContext,
        mpsc::Receiver<lattice_cep_protocol::ToolEvent>,
    ) {
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
                base_envelope: BaseExecutionEnvelope::for_workspace_root(root, "mutation test")
                    .with_session_state(),
                cep_context: Arc::new(crate::policy::CepTurnContext::defaults_for_project_root(
                    root,
                )),
                active_envelope: store,
                turn_id: "test-turn".into(),
                durable_metadata: crate::call_durable_metadata::CallDurableMetadata::default(),
                effect_ledger: crate::effect::ledger::EffectLedger::new(),
                authorized_region: crate::effect::ledger::AuthorizedRegionSlot::new(),
                policy_hook: default_hook.clone()
                    as Arc<dyn lattice_cep_protocol::policy::PolicyHook>,
                default_policy_hook: Some(default_hook),
                audit_context: Arc::new(tokio::sync::RwLock::new(crate::audit::CepAuditContext {
                    turn_id: "test-turn".into(),
                    session_id: "test-session".into(),
                    ..Default::default()
                })),
                audit: Arc::new(crate::audit::CepAuditStore::in_memory().expect("audit store")),
                invariant_index,
                cep_mode,
            },
            event_rx,
        )
    }

    fn mutation_registry() -> MethodRegistry {
        let mut registry = MethodRegistry::new();
        register_mutation_methods(&mut registry).unwrap();
        registry
    }

    fn mutation_and_intent_registry() -> MethodRegistry {
        let mut registry = mutation_registry();
        register_submit_intent(&mut registry).unwrap();
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
                scope: None,
                confidence: ImpactConfidence::Conservative,
                unknowns: vec![],
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
        envelope.verify = Some(lattice_cep_protocol::envelope::VerifyCommand {
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
    async fn test_replan_is_distinct_from_guard_denied() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().to_str().unwrap();
        let registry = mutation_registry();
        let store = Arc::new(ActiveEnvelopeStore::new());

        let envelope = envelope_with_write_path("src/allowed/**");
        store.declare(envelope, 0, "test-turn").await.unwrap();

        let (ctx1, _events1) = test_ctx_with_store(root, Arc::clone(&store));
        let out_of_scope = WriteFileParams {
            path: "src/other/file.rs".into(),
            content: "fn main() {}\n".into(),
        };
        let replan = registry
            .dispatch(
                WriteFile::NAME,
                serde_json::to_value(&out_of_scope).unwrap(),
                ctx1,
            )
            .await
            .unwrap_err();
        assert_eq!(replan.code, CepErrorCode::ReplanRequired);

        let (ctx2, _events2) = test_ctx_with_store(root, store);
        let traversal = WriteFileParams {
            path: "../../etc/passwd".into(),
            content: "bad".into(),
        };
        let guard = registry
            .dispatch(
                WriteFile::NAME,
                serde_json::to_value(&traversal).unwrap(),
                ctx2,
            )
            .await
            .unwrap_err();
        assert_ne!(guard.code, CepErrorCode::ReplanRequired);
    }

    #[tokio::test]
    async fn test_replan_flow_mutation_outside_scope_then_submit_intent_resumes() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().to_str().unwrap();
        std::fs::create_dir_all(dir.path().join("src/other")).unwrap();
        let registry = mutation_and_intent_registry();
        let store = Arc::new(ActiveEnvelopeStore::new());

        let envelope = envelope_with_write_path("src/allowed/**");
        store.declare(envelope, 0, "test-turn").await.unwrap();

        let write_params = WriteFileParams {
            path: "src/other/file.rs".into(),
            content: "fn main() {}\n".into(),
        };

        let (ctx1, _events1) = test_ctx_with_store(root, Arc::clone(&store));
        let err = registry
            .dispatch(
                WriteFile::NAME,
                serde_json::to_value(&write_params).unwrap(),
                ctx1,
            )
            .await
            .unwrap_err();
        assert_eq!(err.code, CepErrorCode::ReplanRequired);

        let (ctx2, _events2) = test_ctx_with_store(root, Arc::clone(&store));
        let intent_params = SubmitIntentParams {
            action: "write".into(),
            target: "src/other/file.rs".into(),
            params: None,
            supersede: true,
            reason: Some("expand scope for out-of-band file".into()),
        };
        registry
            .dispatch(
                SubmitIntent::NAME,
                serde_json::to_value(&intent_params).unwrap(),
                ctx2,
            )
            .await
            .unwrap();

        let (ctx3, _events3) = test_ctx_with_store(root, store);
        registry
            .dispatch(
                WriteFile::NAME,
                serde_json::to_value(&write_params).unwrap(),
                ctx3,
            )
            .await
            .unwrap();

        assert!(dir.path().join("src/other/file.rs").exists());
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
    use crate::invariants::{CardAnchor, ForbiddenEntry, InvariantCard, InvariantCardIndex};
    use crate::registry::{Cancellation, EventSink};
    use lattice_cep_protocol::envelope::{
        BaseExecutionEnvelope, Capability, Constraints, ExecutionEnvelope, Impact,
        ImpactConfidence, ImpactKind, Intent, OnFailure, Scope,
    };
    use std::collections::BTreeSet;
    use std::sync::Arc;
    use tempfile::TempDir;
    use tokio::sync::{mpsc, watch};

    fn test_ctx_with_store(
        root: &str,
        store: Arc<ActiveEnvelopeStore>,
    ) -> (
        HandlerContext,
        mpsc::Receiver<lattice_cep_protocol::ToolEvent>,
    ) {
        test_ctx_with_store_and_index(
            root,
            store,
            Arc::new(InvariantCardIndex::new(vec![])),
            CepMode::Off,
        )
    }

    fn test_ctx_with_store_and_index(
        root: &str,
        store: Arc<ActiveEnvelopeStore>,
        invariant_index: Arc<InvariantCardIndex>,
        cep_mode: CepMode,
    ) -> (
        HandlerContext,
        mpsc::Receiver<lattice_cep_protocol::ToolEvent>,
    ) {
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
                cep_context: Arc::new(crate::policy::CepTurnContext::defaults_for_project_root(
                    root,
                )),
                active_envelope: store,
                turn_id: "test-turn".into(),
                durable_metadata: crate::call_durable_metadata::CallDurableMetadata::default(),
                effect_ledger: crate::effect::ledger::EffectLedger::new(),
                authorized_region: crate::effect::ledger::AuthorizedRegionSlot::new(),
                policy_hook: default_hook.clone()
                    as Arc<dyn lattice_cep_protocol::policy::PolicyHook>,
                default_policy_hook: Some(default_hook),
                audit_context: Arc::new(tokio::sync::RwLock::new(crate::audit::CepAuditContext {
                    turn_id: "test-turn".into(),
                    session_id: "test-session".into(),
                    ..Default::default()
                })),
                audit: Arc::new(crate::audit::CepAuditStore::in_memory().expect("audit store")),
                invariant_index,
                cep_mode,
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
                scope: None,
                confidence: ImpactConfidence::Conservative,
                unknowns: vec![],
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
    async fn malformed_patch_returns_schema_mismatch_before_capability_check() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().to_str().unwrap();
        let registry = mutation_registry();
        let store = Arc::new(ActiveEnvelopeStore::new());
        let (ctx, _events) = test_ctx_with_store(root, store);

        let params = PatchParams {
            patch: "--- a/src/main.rs\n+++ b/src/main.rs\n@@ -1 +1 @@\n-old\n+new".into(),
        };
        let err = registry
            .dispatch(Patch::NAME, serde_json::to_value(&params).unwrap(), ctx)
            .await
            .unwrap_err();
        assert_eq!(err.code, CepErrorCode::SchemaMismatch);
        assert!(
            err.message.contains("*** Begin Patch") || err.message.contains("patch parse error"),
            "malformed patch should report patch syntax, got: {}",
            err.message
        );
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
    async fn patch_l2_checks_post_patch_new_file_content() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().to_str().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        let registry = mutation_registry();
        let store = Arc::new(ActiveEnvelopeStore::new());
        store
            .declare(envelope_with_write_path("src/**"), 0, "test-turn")
            .await
            .unwrap();

        let card = InvariantCard {
            id: "INV-STRUCT".into(),
            title: "No structs".into(),
            anchor: CardAnchor::Glob("src/**/*.rs".into()),
            forbidden: vec![ForbiddenEntry {
                category: "struct_def".into(),
                reason: "structs are frozen".into(),
            }],
            opaque: vec![],
        };
        let index = Arc::new(InvariantCardIndex::new(vec![card]));
        let (ctx, _events) = test_ctx_with_store_and_index(root, store, index, CepMode::Enforce);

        let params = PatchParams {
            patch: "*** Begin Patch\n*** Add File: src/new.rs\n+struct Added {}\n*** End Patch"
                .into(),
        };
        let err = registry
            .dispatch(Patch::NAME, serde_json::to_value(&params).unwrap(), ctx)
            .await
            .unwrap_err();
        assert_eq!(err.code, CepErrorCode::GuardDenied);
        assert!(!dir.path().join("src/new.rs").exists());
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
