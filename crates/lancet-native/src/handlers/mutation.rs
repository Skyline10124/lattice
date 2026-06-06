// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Native CEP workspace mutation handlers.
//!
//! Implements workspace mutation methods with Guard L1 scope
//! enforcement and verify flow.
//! All methods require an active `ExecutionEnvelope`; multi-file mutations
//! perform all-or-nothing scope preflight before applying changes.

use crate::edit::apply::PatchApplyLimits;
use crate::edit::commit_baseline::{
    commit_baseline_details_json, CommitBaselineFailureReason, CommitBaselineReport,
    CommitBaselineValidation,
};
use crate::edit::content_hash;
use crate::edit::hash::line_hash;
use crate::edit::parser::parse_patch_document;
use crate::edit::types::PatchOperation;
use crate::enforcement::guard_l1::{
    check_write_scope, evaluate_and_enforce_policy, extract_write_path_patterns,
};
use crate::enforcement::verify::{
    extract_verify_command, run_verify, validate_verify_command, verify_failed_error,
};
use crate::server::registry::{HandlerContext, MethodRegistry};
use lancet_authority::effect::ledger::{
    EffectSource, ObservedEffect, ObservedLineChange, ObservedLineChangeKind, ObservedScope,
};
use lancet_catalog::method::RequiredCapability;
use lancet_catalog::methods::workspace::{
    Batch, BatchOperation, BatchParams, BatchResult, Create, CreateParams, CreateResult, Edit,
    EditOperation, EditParams, EditResult,
};
use lancet_catalog::CepMethod;
use lancet_invariants::{guard_l2, InvariantCardIndex};
use lancet_protocol::envelope::{CepMode, ExecutionEnvelope, ImpactKind};
use lancet_protocol::error::{CepErrorData, GuardErrorData};
use lancet_protocol::{CepError, CepErrorCode, Retriability};
use serde_json::json;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// Register native workspace mutation methods.
pub fn register_mutation_methods(registry: &mut MethodRegistry) -> Result<(), CepError> {
    registry.register_method::<Create, _, _>(handle_create)?;
    registry.register_method::<Edit, _, _>(handle_edit)?;
    registry.register_method::<Batch, _, _>(handle_batch)?;
    Ok(())
}

struct MutationAuthorization {
    envelope_id: String,
    envelope: ExecutionEnvelope,
    project_root: String,
}

async fn handle_create(
    params: CreateParams,
    ctx: HandlerContext,
) -> Result<CreateResult, CepError> {
    handle_create_as(params, ctx, Create::NAME).await
}

/// Handle `mutate.create` with Guard L1 scope enforcement and verify flow.
///
/// Flow:
/// 1. Check active envelope exists -> else EnvelopeMissing
/// 2. Extract WritePath patterns from active envelope
/// 3. Check target path matches WritePath scope via check_write_scope
/// 4. Check parent directory exists and target file does not exist
/// 5. Create file using secure containment
/// 6. If envelope has verify -> run verify
/// 7. If verify fails -> VerifyFailed, retain diff, leave envelope valid
/// 8. Return CreateResult
async fn handle_create_as(
    params: CreateParams,
    ctx: HandlerContext,
    method: &'static str,
) -> Result<CreateResult, CepError> {
    let auth =
        authorize_workspace_mutation(&ctx, method, &params, std::slice::from_ref(&params.path))
            .await?;

    let create = preflight_create_file(
        &auth.project_root,
        &params,
        &ctx.invariant_index,
        ctx.cep_mode.clone(),
        method,
        &ctx,
    )?;

    write_preflighted_create(&auth.project_root, &create).await?;

    ctx.record_effect(ObservedEffect {
        kind: ImpactKind::WorkspaceMutation,
        scope: ObservedScope::Path(create.full_path.clone()),
        required_capability: RequiredCapability::WritePath,
        source: EffectSource::HandlerBackend,
    });

    // 7. Run verify if configured
    run_mutation_verify_if_configured(&ctx, method, &auth).await?;

    // 8. Record metadata
    let written_hash = content_hash(params.content.as_bytes());
    ctx.record_durable_metadata(json!({
        "method": method,
        "envelope_id": auth.envelope_id,
        "bytes_written": create.bytes_written,
        "content_hash": written_hash,
        "backend_invoked": true,
        "policy_enforced": true,
    }));

    Ok(CreateResult {
        path: params.path,
        bytes_written: create.bytes_written,
    })
}

async fn handle_edit(params: EditParams, ctx: HandlerContext) -> Result<EditResult, CepError> {
    let auth = authorize_workspace_mutation(
        &ctx,
        Edit::NAME,
        &params,
        std::slice::from_ref(&params.path),
    )
    .await?;
    let patch = compile_edit_patch(&auth.project_root, &params)?;
    let result = apply_compiled_patch(&ctx, Edit::NAME, &auth, &patch).await?;
    run_mutation_verify_if_configured(&ctx, Edit::NAME, &auth).await?;

    ctx.record_durable_metadata(json!({
        "method": Edit::NAME,
        "envelope_id": auth.envelope_id,
        "compiled_patch_backend": "hashline_patch",
        "changed_files": result.changed_files.clone(),
        "backend_invoked": true,
        "policy_enforced": true,
    }));

    Ok(EditResult {
        changed_files: result.changed_files,
    })
}

async fn handle_batch(params: BatchParams, ctx: HandlerContext) -> Result<BatchResult, CepError> {
    let changed_paths = batch_target_paths(&params)?;
    let auth = authorize_workspace_mutation(&ctx, Batch::NAME, &params, &changed_paths).await?;

    let mut create_inputs = Vec::new();
    let mut edit_inputs = Vec::new();
    let mut seen_paths = HashSet::new();

    for operation in &params.operations {
        match operation {
            BatchOperation::CreateFile { path, content } => {
                reject_duplicate_batch_path(&mut seen_paths, path)?;
                let params = CreateParams {
                    path: path.clone(),
                    content: content.clone(),
                };
                create_inputs.push(preflight_create_file(
                    &auth.project_root,
                    &params,
                    &ctx.invariant_index,
                    ctx.cep_mode.clone(),
                    Batch::NAME,
                    &ctx,
                )?);
            }
            BatchOperation::EditFile {
                path,
                expected_hash,
                edits,
            } => {
                reject_duplicate_batch_path(&mut seen_paths, path)?;
                let params = EditParams {
                    path: path.clone(),
                    expected_hash: expected_hash.clone(),
                    edits: edits.clone(),
                };
                edit_inputs.push(params);
            }
        }
    }

    let patch = if edit_inputs.is_empty() {
        None
    } else {
        Some(compile_batch_edit_patch(&auth.project_root, &edit_inputs)?)
    };

    let mut edited_files = Vec::new();
    if let Some(patch) = patch.as_deref() {
        edited_files = apply_compiled_patch(&ctx, Batch::NAME, &auth, patch)
            .await?
            .changed_files;
    }

    let mut created_files = Vec::new();
    for create in &create_inputs {
        write_preflighted_create(&auth.project_root, create).await?;
        ctx.record_effect(ObservedEffect {
            kind: ImpactKind::WorkspaceMutation,
            scope: ObservedScope::Path(create.full_path.clone()),
            required_capability: RequiredCapability::WritePath,
            source: EffectSource::HandlerBackend,
        });
        created_files.push(create.path.clone());
    }

    run_mutation_verify_if_configured(&ctx, Batch::NAME, &auth).await?;

    let mut changed_files = edited_files.clone();
    changed_files.extend(created_files.iter().cloned());

    ctx.record_durable_metadata(json!({
        "method": Batch::NAME,
        "envelope_id": auth.envelope_id,
        "compiled_patch_backend": "hashline_patch",
        "created_files": created_files.clone(),
        "edited_files": edited_files.clone(),
        "backend_invoked": true,
        "policy_enforced": true,
    }));

    Ok(BatchResult {
        changed_files,
        created_files,
        edited_files,
    })
}

struct CompiledPatchApply {
    changed_files: Vec<String>,
}

struct PreflightCreate {
    path: String,
    content: String,
    full_path: PathBuf,
    bytes_written: u64,
}

async fn authorize_workspace_mutation(
    ctx: &HandlerContext,
    method: &str,
    params: &impl serde::Serialize,
    changed_paths: &[String],
) -> Result<MutationAuthorization, CepError> {
    let _evaluation = evaluate_and_enforce_policy(ctx, method, params).await?;

    let (envelope_id, envelope) = match ctx.active_envelope.active_envelope().await {
        Some((id, env)) => (id, env),
        None => {
            emit_envelope_missing(ctx, method, "workspace_mutation").await;
            return Err(envelope_missing_error(method));
        }
    };

    let patterns = extract_write_path_patterns(&envelope.capabilities);
    if patterns.is_empty() {
        emit_envelope_missing(ctx, method, "workspace_mutation").await;
        return Err(envelope_missing_error(method));
    }

    let project_root = ctx
        .cep_context
        .policy
        .project_root
        .to_string_lossy()
        .to_string();

    let mut out_of_scope = Vec::new();
    for path in changed_paths {
        if let Err(err) = check_write_scope(path, &project_root, &patterns) {
            if err.code == CepErrorCode::ReplanRequired {
                out_of_scope.push(path.clone());
            } else {
                return Err(err);
            }
        }
    }

    if !out_of_scope.is_empty() {
        emit_replan_required(ctx, method, &envelope_id, &out_of_scope).await;
        return Err(CepError::new(
            CepErrorCode::ReplanRequired,
            format!(
                "{method} contains out-of-scope paths: {:?}; mutation not applied",
                out_of_scope
            ),
            CepErrorData::Guard(GuardErrorData {
                method: method.into(),
                reason: format!("ReplanRequired: out-of-scope paths {:?}", out_of_scope),
            }),
            Retriability::RequiresReplan,
        ));
    }

    Ok(MutationAuthorization {
        envelope_id,
        envelope,
        project_root,
    })
}

async fn run_mutation_verify_if_configured(
    ctx: &HandlerContext,
    method: &str,
    auth: &MutationAuthorization,
) -> Result<(), CepError> {
    let Some(verify_cmd) = extract_verify_command(&auth.envelope) else {
        return Ok(());
    };

    let policy = &ctx.cep_context.policy.command;
    validate_verify_command(&verify_cmd, policy, &auth.project_root)?;

    let mut cancellation = ctx.cancellation.clone();
    match run_verify(&verify_cmd, &auth.project_root, policy, &mut cancellation).await {
        Ok(outcome) if !outcome.success => {
            emit_verify_failed(ctx, method, &auth.envelope_id, outcome.exit_code).await;
            Err(verify_failed_error(&verify_cmd, &outcome))
        }
        Ok(_) => {
            ctx.record_effect(ObservedEffect {
                kind: ImpactKind::CommandExecution,
                scope: ObservedScope::Root(auth.project_root.clone()),
                required_capability: RequiredCapability::RunCommand,
                source: EffectSource::Verify,
            });
            Ok(())
        }
        Err(err) => {
            emit_verify_failed(ctx, method, &auth.envelope_id, -1).await;
            Err(err)
        }
    }
}

fn preflight_create_file(
    project_root: &str,
    params: &CreateParams,
    invariant_index: &InvariantCardIndex,
    cep_mode: CepMode,
    method: &str,
    ctx: &HandlerContext,
) -> Result<PreflightCreate, CepError> {
    let full_path = Path::new(project_root).join(&params.path);
    if let Some(parent) = full_path.parent() {
        if !parent.exists() {
            return Err(CepError::new(
                CepErrorCode::ExecutionFailed,
                format!(
                    "parent directory '{}' does not exist for '{}'; mutate.create does not create directories",
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
    if full_path.exists() {
        return Err(CepError::new(
            CepErrorCode::ExecutionFailed,
            format!(
                "target file '{}' already exists; mutate.create only creates new files",
                params.path
            ),
            CepErrorData::Validation(lancet_protocol::error::ValidationErrorData {
                field: "path".into(),
                reason: "target already exists".into(),
            }),
            Retriability::Permanent,
        ));
    }

    let cards = invariant_index.match_target(&params.path);
    if let Some(message) = lancet_invariants::opaque_human_review_message(&cards) {
        return Err(lancet_invariants::opaque_human_review_error(
            method, &message,
        ));
    }

    if params.path.ends_with(".rs") && !matches!(cep_mode, CepMode::Off) {
        let _status = guard_l2::l2_status_for_method(method);
        let has_forbidden = cards.iter().any(|c| !c.forbidden.is_empty());
        if has_forbidden {
            let result = guard_l2::check_l2(
                &params.path,
                params.content.as_bytes(),
                &[(0, params.content.len())],
                &cards,
            );
            enforce_l2_result(ctx, method, &params.path, &result)?;
        }
    }

    Ok(PreflightCreate {
        path: params.path.clone(),
        content: params.content.clone(),
        full_path,
        bytes_written: params.content.len() as u64,
    })
}

async fn write_preflighted_create(
    project_root: &str,
    create: &PreflightCreate,
) -> Result<(), CepError> {
    create_file_secure(project_root, &create.path, &create.content).await
}

fn batch_target_paths(params: &BatchParams) -> Result<Vec<String>, CepError> {
    if params.operations.is_empty() {
        return Err(validation_error(
            "operations",
            "mutate.batch requires at least one operation",
        ));
    }

    Ok(params
        .operations
        .iter()
        .map(|operation| match operation {
            BatchOperation::CreateFile { path, .. } | BatchOperation::EditFile { path, .. } => {
                path.clone()
            }
        })
        .collect())
}

fn reject_duplicate_batch_path(seen: &mut HashSet<String>, path: &str) -> Result<(), CepError> {
    if seen.insert(path.to_string()) {
        Ok(())
    } else {
        Err(validation_error(
            "operations",
            format!("mutate.batch contains duplicate target path '{path}'"),
        ))
    }
}

fn compile_edit_patch(project_root: &str, params: &EditParams) -> Result<String, CepError> {
    let fragment = compile_edit_operation_fragment(project_root, params)?;
    Ok(format!("*** Begin Patch\n{fragment}*** End Patch\n"))
}

fn compile_batch_edit_patch(project_root: &str, edits: &[EditParams]) -> Result<String, CepError> {
    let mut patch = String::from("*** Begin Patch\n");
    for edit in edits {
        patch.push_str(&compile_edit_operation_fragment(project_root, edit)?);
    }
    patch.push_str("*** End Patch\n");
    Ok(patch)
}

fn compile_edit_operation_fragment(
    project_root: &str,
    params: &EditParams,
) -> Result<String, CepError> {
    if params.edits.is_empty() {
        return Err(validation_error(
            "edits",
            "mutate.edit requires at least one edit operation",
        ));
    }
    if params.path.contains('\n') || params.path.contains('\r') {
        return Err(validation_error(
            "path",
            "path must not contain newline characters",
        ));
    }

    let before =
        std::fs::read_to_string(Path::new(project_root).join(&params.path)).map_err(|err| {
            CepError::new(
                CepErrorCode::ExecutionFailed,
                format!("cannot read edit target '{}': {err}", params.path),
                CepErrorData::Execution(lancet_protocol::error::ExecutionErrorData {
                    command: "mutate.edit.compile".into(),
                    exit_code: Some(1),
                    stderr: Some(err.to_string()),
                }),
                Retriability::Permanent,
            )
        })?;

    let current_hash = content_hash(before.as_bytes());
    if current_hash != params.expected_hash {
        return Err(CepError::new(
            CepErrorCode::ReplanRequired,
            format!(
                "expected_hash for '{}' is stale: expected {}, current {}; observe.workspace read_file again",
                params.path, params.expected_hash, current_hash
            ),
            CepErrorData::Guard(GuardErrorData {
                method: Edit::NAME.into(),
                reason: "ReplanRequired: stale expected_hash".into(),
            }),
            Retriability::RequiresReplan,
        ));
    }

    let after = apply_semantic_edit_operations(&before, &params.edits)?;
    compile_update_fragment(&params.path, &params.expected_hash, &before, &after)
}

fn apply_semantic_edit_operations(
    before: &str,
    edits: &[EditOperation],
) -> Result<String, CepError> {
    let mut content = before.to_string();
    for edit in edits {
        match edit {
            EditOperation::ReplaceText {
                old,
                new,
                occurrence,
            } => {
                let (start, end) = find_semantic_occurrence(&content, old, *occurrence, "old")?;
                content.replace_range(start..end, new);
            }
            EditOperation::InsertBefore {
                anchor,
                content: inserted,
                occurrence,
            } => {
                let (start, _) = find_semantic_occurrence(&content, anchor, *occurrence, "anchor")?;
                content.insert_str(start, inserted);
            }
            EditOperation::InsertAfter {
                anchor,
                content: inserted,
                occurrence,
            } => {
                let (_, end) = find_semantic_occurrence(&content, anchor, *occurrence, "anchor")?;
                content.insert_str(end, inserted);
            }
        }
    }
    Ok(content)
}

fn find_semantic_occurrence(
    haystack: &str,
    needle: &str,
    occurrence: Option<u64>,
    field: &str,
) -> Result<(usize, usize), CepError> {
    if needle.is_empty() {
        return Err(validation_error(field, "match text must not be empty"));
    }
    let matches = haystack
        .match_indices(needle)
        .map(|(start, value)| (start, start + value.len()))
        .collect::<Vec<_>>();

    let index = match occurrence {
        Some(0) => return Err(validation_error("occurrence", "occurrence is 1-based")),
        Some(value) => value as usize - 1,
        None if matches.len() == 1 => 0,
        None if matches.is_empty() => {
            return Err(semantic_replan_error(format!("{field} text was not found")));
        }
        None => {
            return Err(semantic_replan_error(format!(
                "{field} text matched {} occurrences; provide occurrence",
                matches.len()
            )));
        }
    };

    matches.get(index).copied().ok_or_else(|| {
        semantic_replan_error(format!("{field} occurrence {} was not found", index + 1))
    })
}

fn compile_update_fragment(
    path: &str,
    expected_hash: &str,
    before: &str,
    after: &str,
) -> Result<String, CepError> {
    if before == after {
        return Err(validation_error(
            "edits",
            "semantic edit produced no changes",
        ));
    }
    let (before_lines, before_final_newline) = patch_lines(before);
    let (after_lines, after_final_newline) = patch_lines(after);
    if before_final_newline != after_final_newline {
        return Err(validation_error(
            "edits",
            "semantic edits must preserve the file's trailing newline state",
        ));
    }

    let mut prefix = 0usize;
    while prefix < before_lines.len()
        && prefix < after_lines.len()
        && before_lines[prefix] == after_lines[prefix]
    {
        prefix += 1;
    }

    let mut suffix = 0usize;
    while suffix < before_lines.len().saturating_sub(prefix)
        && suffix < after_lines.len().saturating_sub(prefix)
        && before_lines[before_lines.len() - 1 - suffix]
            == after_lines[after_lines.len() - 1 - suffix]
    {
        suffix += 1;
    }

    let old_end = before_lines.len() - suffix;
    let new_end = after_lines.len() - suffix;
    let mut fragment = format!("*** Update File: {path}\n*** Old Hash: {expected_hash}\n@@\n");

    if prefix == old_end {
        render_insertion_hunk(
            &mut fragment,
            &before_lines,
            &after_lines[prefix..new_end],
            prefix,
        )?;
    } else {
        for (idx, line) in before_lines[prefix..old_end].iter().enumerate() {
            let line_number = prefix + idx + 1;
            fragment.push_str(&format!("-{}\t{}\n", line_hash(line_number, line), line));
        }
        for line in &after_lines[prefix..new_end] {
            fragment.push_str(&format!("+{line}\n"));
        }
    }

    Ok(fragment)
}

fn render_insertion_hunk(
    fragment: &mut String,
    before_lines: &[String],
    inserted: &[String],
    insert_at: usize,
) -> Result<(), CepError> {
    if inserted.is_empty() {
        return Err(validation_error(
            "edits",
            "semantic edit produced no changed lines",
        ));
    }

    if insert_at > 0 {
        let context_index = insert_at - 1;
        let context = &before_lines[context_index];
        fragment.push_str(&format!(
            " {}\t{}\n",
            line_hash(context_index + 1, context),
            context
        ));
        for line in inserted {
            fragment.push_str(&format!("+{line}\n"));
        }
        return Ok(());
    }

    if let Some(context) = before_lines.first() {
        for line in inserted {
            fragment.push_str(&format!("+{line}\n"));
        }
        fragment.push_str(&format!(" {}\t{}\n", line_hash(1, context), context));
        return Ok(());
    }

    Err(validation_error(
        "edits",
        "semantic insertion into an empty existing file is not supported by hashline patch yet",
    ))
}

fn patch_lines(content: &str) -> (Vec<String>, bool) {
    if content.is_empty() {
        return (Vec::new(), false);
    }
    let final_newline = content.ends_with('\n');
    let mut lines = content
        .split('\n')
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    if final_newline {
        lines.pop();
    }
    (lines, final_newline)
}

async fn apply_compiled_patch(
    ctx: &HandlerContext,
    method: &str,
    auth: &MutationAuthorization,
    patch: &str,
) -> Result<CompiledPatchApply, CepError> {
    let document = parse_patch_document(patch).map_err(|parse_err| {
        CepError::new(
            CepErrorCode::SchemaMismatch,
            format!("compiled patch parse error: {}", parse_err),
            CepErrorData::Validation(lancet_protocol::error::ValidationErrorData {
                field: "patch".into(),
                reason: parse_err.to_string(),
            }),
            Retriability::Permanent,
        )
    })?;

    for operation in &document.operations {
        let path = operation.path();
        let cards = ctx.invariant_index.match_target(path);
        if let Some(message) = lancet_invariants::opaque_human_review_message(&cards) {
            return Err(lancet_invariants::opaque_human_review_error(
                method, &message,
            ));
        }
        if path.ends_with(".rs")
            && !matches!(ctx.cep_mode, CepMode::Off)
            && cards.iter().any(|c| !c.forbidden.is_empty())
        {
            if let Some((before, after)) = projected_patch_content(&auth.project_root, operation)? {
                let ranges = changed_line_byte_ranges(before.as_deref(), &after);
                let result = guard_l2::check_l2(path, after.as_bytes(), &ranges, &cards);
                enforce_l2_result(ctx, method, path, &result)?;
            }
        }
    }

    let limits = PatchApplyLimits::default();
    let result = crate::edit::apply_patch(Path::new(&auth.project_root), &limits, patch).await;

    if let Some(report) = result.commit_baseline_report.as_ref() {
        let metadata = commit_baseline_metadata_json(report);
        ctx.record_durable_metadata(metadata);
    }

    if !result.ok {
        if let Some(report) = result.commit_baseline_report.as_ref() {
            if let CommitBaselineValidation::Failed(reason) = report.validation {
                return Err(commit_baseline_replan_error(method, report, reason));
            }
        }
        return Err(CepError::new(
            CepErrorCode::ExecutionFailed,
            format!("compiled patch application failed: {}", result.summary),
            CepErrorData::Execution(lancet_protocol::error::ExecutionErrorData {
                command: "compiled_patch".into(),
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

    let changed_files = result
        .changed_files
        .iter()
        .map(|f| f.path.clone())
        .collect::<Vec<_>>();

    for changed_path in &changed_files {
        let abs_path = if Path::new(changed_path).is_relative() {
            PathBuf::from(&auth.project_root).join(changed_path)
        } else {
            PathBuf::from(changed_path)
        };
        let changes = result
            .changed_files
            .iter()
            .find(|file| file.path == *changed_path)
            .map(|file| {
                file.line_changes
                    .iter()
                    .map(|change| ObservedLineChange {
                        kind: match change.kind {
                            crate::edit::types::LineChangeKind::Added => {
                                ObservedLineChangeKind::Added
                            }
                            crate::edit::types::LineChangeKind::Removed => {
                                ObservedLineChangeKind::Removed
                            }
                        },
                        old_hash: change.old_hash.clone(),
                        new_hash: change.new_hash.clone(),
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        ctx.record_effect(ObservedEffect {
            kind: ImpactKind::WorkspaceMutation,
            scope: ObservedScope::Lines {
                path: abs_path,
                changes,
            },
            required_capability: RequiredCapability::WritePath,
            source: EffectSource::HandlerBackend,
        });
    }

    Ok(CompiledPatchApply { changed_files })
}

fn validation_error(field: impl Into<String>, reason: impl Into<String>) -> CepError {
    let reason = reason.into();
    CepError::new(
        CepErrorCode::SchemaMismatch,
        reason.clone(),
        CepErrorData::Validation(lancet_protocol::error::ValidationErrorData {
            field: field.into(),
            reason,
        }),
        Retriability::Permanent,
    )
}

fn semantic_replan_error(reason: impl Into<String>) -> CepError {
    let reason = reason.into();
    CepError::new(
        CepErrorCode::ReplanRequired,
        reason.clone(),
        CepErrorData::Guard(GuardErrorData {
            method: Edit::NAME.into(),
            reason,
        }),
        Retriability::RequiresReplan,
    )
}

fn commit_baseline_metadata_json(report: &CommitBaselineReport) -> serde_json::Value {
    let details = commit_baseline_details_json(report);
    json!({
        "commit_baseline_validation": details["commit_baseline_validation"].clone(),
        "commit_baseline_failure_reason": details["commit_baseline_failure_reason"].clone(),
        "commit_baseline_details": details,
    })
}

fn commit_baseline_replan_error(
    method: &str,
    report: &CommitBaselineReport,
    reason: CommitBaselineFailureReason,
) -> CepError {
    let file_path = report
        .hunks
        .iter()
        .find(|hunk| hunk.match_result.as_str() == reason.as_str())
        .map(|hunk| hunk.file_path.clone())
        .or_else(|| report.lower_drift_paths.first().cloned())
        .unwrap_or_else(|| "<unknown>".into());
    let hunk_index = report
        .hunks
        .iter()
        .find(|hunk| hunk.match_result.as_str() == reason.as_str())
        .map(|hunk| hunk.hunk_index);
    let coordinate = hunk_index
        .map(|index| format!("file={file_path}, hunk_index={index}"))
        .unwrap_or_else(|| format!("file={file_path}"));

    CepError::new(
        CepErrorCode::ReplanRequired,
        format!(
            "patch commit baseline validation failed: {} ({coordinate})",
            reason.as_str()
        ),
        CepErrorData::Guard(GuardErrorData {
            method: method.into(),
            reason: format!(
                "ReplanRequired: commit_baseline_failure_reason={} ({coordinate})",
                reason.as_str()
            ),
        }),
        Retriability::RequiresReplan,
    )
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
        PatchOperation::UpdateFile { path, hunks, .. } => {
            let full_path = std::path::Path::new(project_root).join(path);
            let before = std::fs::read_to_string(&full_path).map_err(|err| {
                CepError::new(
                    CepErrorCode::ExecutionFailed,
                    format!("cannot read patch target '{path}' for L2 preflight: {err}"),
                    CepErrorData::Execution(lancet_protocol::error::ExecutionErrorData {
                        command: "patch_l2_preflight".into(),
                        exit_code: Some(1),
                        stderr: Some(err.to_string()),
                    }),
                    Retriability::Permanent,
                )
            })?;
            let after = crate::edit::apply::apply_hunks(path, &before, hunks, 0)
                .map_err(|diagnostic| {
                    CepError::new(
                        CepErrorCode::SchemaMismatch,
                        format!("patch L2 preflight hunk error: {}", diagnostic.message),
                        CepErrorData::Validation(lancet_protocol::error::ValidationErrorData {
                            field: "patch".into(),
                            reason: diagnostic.message,
                        }),
                        Retriability::Permanent,
                    )
                })?
                .after_text;
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

/// Create a file securely using native containment checks.
async fn create_file_secure(
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
                command: "create".into(),
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
                command: "create".into(),
                exit_code: None,
                stderr: Some("parent directory does not exist".into()),
            }),
            Retriability::Permanent,
        ));
    }

    use tokio::io::AsyncWriteExt;

    let mut file = tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&full_path)
        .await
        .map_err(|e| {
            CepError::new(
                CepErrorCode::ExecutionFailed,
                format!("failed to create '{}': {e}", relative_path),
                CepErrorData::Execution(lancet_protocol::error::ExecutionErrorData {
                    command: "create".into(),
                    exit_code: None,
                    stderr: Some(e.to_string()),
                }),
                Retriability::Transient,
            )
        })?;

    file.write_all(content.as_bytes()).await.map_err(|e| {
        CepError::new(
            CepErrorCode::ExecutionFailed,
            format!("failed to write '{}': {e}", relative_path),
            CepErrorData::Execution(lancet_protocol::error::ExecutionErrorData {
                command: "create".into(),
                exit_code: None,
                stderr: Some(e.to_string()),
            }),
            Retriability::Transient,
        )
    })?;
    file.sync_all().await.map_err(|e| {
        CepError::new(
            CepErrorCode::ExecutionFailed,
            format!("failed to sync '{}': {e}", relative_path),
            CepErrorData::Execution(lancet_protocol::error::ExecutionErrorData {
                command: "create".into(),
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
mod create_tests {
    use super::*;
    use crate::handlers::register_envelope_methods;
    use crate::server::registry::{Cancellation, EventSink};
    use lancet_authority::envelope_store::ActiveEnvelopeStore;
    use lancet_catalog::methods::envelope::{Envelope, EnvelopeParams, SubmitIntentParams};
    use lancet_invariants::InvariantCardIndex;
    use lancet_protocol::envelope::{
        BaseExecutionEnvelope, Capability, Constraints, ExecutionEnvelope, Impact,
        ImpactConfidence, ImpactKind, Intent, OnFailure, Scope,
    };
    use std::collections::BTreeSet;
    use std::sync::Arc;
    use tempfile::TempDir;
    use tokio::sync::{mpsc, watch};

    fn test_ctx(root: &str) -> (HandlerContext, mpsc::Receiver<lancet_protocol::ToolEvent>) {
        let (event_tx, event_rx) = mpsc::channel(32);
        let (_cancel_tx, cancel_rx) = watch::channel(false);
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
                base_envelope: BaseExecutionEnvelope::for_workspace_root(root, "mutation test")
                    .with_session_state(),
                cep_context: Arc::new(
                    lancet_authority::policy::CepTurnContext::defaults_for_project_root(root),
                ),
                active_envelope: Arc::new(ActiveEnvelopeStore::new()),
                turn_id: "test-turn".into(),
                durable_metadata:
                    crate::server::call_durable_metadata::CallDurableMetadata::default(),
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
        )
    }

    fn test_ctx_with_store(
        root: &str,
        store: Arc<ActiveEnvelopeStore>,
    ) -> (HandlerContext, mpsc::Receiver<lancet_protocol::ToolEvent>) {
        test_ctx_with_store_and_index(
            root,
            store,
            Arc::new(lancet_invariants::InvariantCardIndex::new(vec![])),
            lancet_protocol::envelope::CepMode::Off,
        )
    }

    fn test_ctx_with_store_and_index(
        root: &str,
        store: Arc<ActiveEnvelopeStore>,
        invariant_index: Arc<InvariantCardIndex>,
        cep_mode: CepMode,
    ) -> (HandlerContext, mpsc::Receiver<lancet_protocol::ToolEvent>) {
        let (event_tx, event_rx) = mpsc::channel(32);
        let (_cancel_tx, cancel_rx) = watch::channel(false);
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
                base_envelope: BaseExecutionEnvelope::for_workspace_root(root, "mutation test")
                    .with_session_state(),
                cep_context: Arc::new(
                    lancet_authority::policy::CepTurnContext::defaults_for_project_root(root),
                ),
                active_envelope: store,
                turn_id: "test-turn".into(),
                durable_metadata:
                    crate::server::call_durable_metadata::CallDurableMetadata::default(),
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
                invariant_index,
                cep_mode,
                command_isolation:
                    crate::enforcement::isolation::CommandIsolationRuntimeConfig::default(),
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
        register_envelope_methods(&mut registry).unwrap();
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
    async fn create_no_envelope_returns_capability_not_granted() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().to_str().unwrap();
        let registry = mutation_registry();
        let (ctx, _events) = test_ctx(root);

        let params = CreateParams {
            path: "test.txt".into(),
            content: "hello".into(),
        };
        let err = registry
            .dispatch(Create::NAME, serde_json::to_value(&params).unwrap(), ctx)
            .await
            .unwrap_err();
        assert_eq!(err.code, CepErrorCode::CapabilityNotGranted);
    }

    #[tokio::test]
    async fn create_in_scope_succeeds() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().to_str().unwrap();
        let registry = mutation_registry();
        let store = Arc::new(ActiveEnvelopeStore::new());

        // Declare envelope with "." pattern (matches everything under project root)
        let envelope = envelope_with_write_path(".");
        store.declare(envelope, 0, "test-turn").await.unwrap();

        let (ctx, _events) = test_ctx_with_store(root, store);

        let params = CreateParams {
            path: "test.txt".into(),
            content: "hello world".into(),
        };
        let result = registry
            .dispatch(Create::NAME, serde_json::to_value(&params).unwrap(), ctx)
            .await
            .unwrap();
        let parsed: CreateResult = serde_json::from_value(result).unwrap();
        assert_eq!(parsed.path, "test.txt");
        assert_eq!(parsed.bytes_written, 11);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("test.txt")).unwrap(),
            "hello world"
        );
    }

    #[tokio::test]
    async fn create_verify_failure_returns_verify_failed_not_replan() {
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
        let params = CreateParams {
            path: "test.txt".into(),
            content: "hello".into(),
        };
        let err = registry
            .dispatch(Create::NAME, serde_json::to_value(&params).unwrap(), ctx)
            .await
            .unwrap_err();
        assert_eq!(err.code, CepErrorCode::VerifyFailed, "err: {err:?}");
        assert_ne!(err.retriability, Retriability::RequiresReplan);
    }

    #[tokio::test]
    async fn create_out_of_scope_returns_replan_required() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().to_str().unwrap();
        let registry = mutation_registry();
        let store = Arc::new(ActiveEnvelopeStore::new());

        // Declare envelope with restricted scope
        let envelope = envelope_with_write_path("src/**");
        store.declare(envelope, 0, "test-turn").await.unwrap();

        let (ctx, _events) = test_ctx_with_store(root, store);

        let params = CreateParams {
            path: "README.md".into(),
            content: "out of scope".into(),
        };
        let err = registry
            .dispatch(Create::NAME, serde_json::to_value(&params).unwrap(), ctx)
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
        let out_of_scope = CreateParams {
            path: "src/other/file.rs".into(),
            content: "fn main() {}\n".into(),
        };
        let replan = registry
            .dispatch(
                Create::NAME,
                serde_json::to_value(&out_of_scope).unwrap(),
                ctx1,
            )
            .await
            .unwrap_err();
        assert_eq!(replan.code, CepErrorCode::ReplanRequired);

        let (ctx2, _events2) = test_ctx_with_store(root, store);
        let traversal = CreateParams {
            path: "../../etc/passwd".into(),
            content: "bad".into(),
        };
        let guard = registry
            .dispatch(
                Create::NAME,
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

        let create_params = CreateParams {
            path: "src/other/file.rs".into(),
            content: "fn main() {}\n".into(),
        };

        let (ctx1, _events1) = test_ctx_with_store(root, Arc::clone(&store));
        let err = registry
            .dispatch(
                Create::NAME,
                serde_json::to_value(&create_params).unwrap(),
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
                Envelope::NAME,
                serde_json::to_value(EnvelopeParams::DeriveIntent(intent_params)).unwrap(),
                ctx2,
            )
            .await
            .unwrap();

        let (ctx3, _events3) = test_ctx_with_store(root, store);
        registry
            .dispatch(
                Create::NAME,
                serde_json::to_value(&create_params).unwrap(),
                ctx3,
            )
            .await
            .unwrap();

        assert!(dir.path().join("src/other/file.rs").exists());
    }

    #[tokio::test]
    async fn create_missing_parent_returns_error() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().to_str().unwrap();
        let registry = mutation_registry();
        let store = Arc::new(ActiveEnvelopeStore::new());

        let envelope = envelope_with_write_path(".");
        store.declare(envelope, 0, "test-turn").await.unwrap();

        let (ctx, _events) = test_ctx_with_store(root, store);

        let params = CreateParams {
            path: "nonexistent/dir/file.txt".into(),
            content: "hello".into(),
        };
        let err = registry
            .dispatch(Create::NAME, serde_json::to_value(&params).unwrap(), ctx)
            .await
            .unwrap_err();
        assert_eq!(err.code, CepErrorCode::ExecutionFailed);
        assert!(err.message.contains("parent"));
    }

    #[tokio::test]
    async fn create_emits_guard_verdict_event() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().to_str().unwrap();
        let registry = mutation_registry();
        let (ctx, mut events) = test_ctx(root);

        let params = CreateParams {
            path: "test.txt".into(),
            content: "hello".into(),
        };
        let _ = registry
            .dispatch(Create::NAME, serde_json::to_value(&params).unwrap(), ctx)
            .await;

        let event = events.recv().await.unwrap();
        assert_eq!(event.method, "cep.guard_verdict");
    }
}

#[cfg(test)]
mod edit_tests {
    use super::*;
    use crate::server::registry::{Cancellation, EventSink};
    use lancet_authority::envelope_store::ActiveEnvelopeStore;
    use lancet_protocol::envelope::{
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
    ) -> (HandlerContext, mpsc::Receiver<lancet_protocol::ToolEvent>) {
        let (event_tx, event_rx) = mpsc::channel(32);
        let (_cancel_tx, cancel_rx) = watch::channel(false);
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
                base_envelope: BaseExecutionEnvelope::for_workspace_root(root, "edit test"),
                cep_context: Arc::new(
                    lancet_authority::policy::CepTurnContext::defaults_for_project_root(root),
                ),
                active_envelope: store,
                turn_id: "test-turn".into(),
                durable_metadata:
                    crate::server::call_durable_metadata::CallDurableMetadata::default(),
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
                cep_mode: CepMode::Off,
                command_isolation:
                    crate::enforcement::isolation::CommandIsolationRuntimeConfig::default(),
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
                description: "test edit".into(),
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
    async fn edit_semantic_replace_computes_hashline_in_runtime() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().to_str().unwrap();
        let before = "alpha\nbeta\n";
        std::fs::write(dir.path().join("target.txt"), before).unwrap();
        let registry = mutation_registry();
        let store = Arc::new(ActiveEnvelopeStore::new());
        store
            .declare(envelope_with_write_path("."), 0, "test-turn")
            .await
            .unwrap();
        let (ctx, _events) = test_ctx_with_store(root, store);

        let params = EditParams {
            path: "target.txt".into(),
            expected_hash: content_hash(before.as_bytes()),
            edits: vec![EditOperation::ReplaceText {
                old: "beta".into(),
                new: "gamma".into(),
                occurrence: None,
            }],
        };
        let result = registry
            .dispatch(Edit::NAME, serde_json::to_value(&params).unwrap(), ctx)
            .await
            .unwrap();
        let parsed: EditResult = serde_json::from_value(result).unwrap();

        assert_eq!(parsed.changed_files, vec!["target.txt"]);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("target.txt")).unwrap(),
            "alpha\ngamma\n"
        );
    }

    #[tokio::test]
    async fn edit_ambiguous_text_requires_occurrence() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().to_str().unwrap();
        let before = "same\nsame\n";
        std::fs::write(dir.path().join("target.txt"), before).unwrap();
        let registry = mutation_registry();
        let store = Arc::new(ActiveEnvelopeStore::new());
        store
            .declare(envelope_with_write_path("."), 0, "test-turn")
            .await
            .unwrap();
        let (ctx, _events) = test_ctx_with_store(root, store);

        let params = EditParams {
            path: "target.txt".into(),
            expected_hash: content_hash(before.as_bytes()),
            edits: vec![EditOperation::ReplaceText {
                old: "same".into(),
                new: "other".into(),
                occurrence: None,
            }],
        };
        let err = registry
            .dispatch(Edit::NAME, serde_json::to_value(&params).unwrap(), ctx)
            .await
            .unwrap_err();

        assert_eq!(err.code, CepErrorCode::ReplanRequired);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("target.txt")).unwrap(),
            before
        );
    }

    #[tokio::test]
    async fn batch_create_and_edit_without_delete_succeeds() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().to_str().unwrap();
        let before = "workspace\nold\n";
        std::fs::write(dir.path().join("Cargo.toml"), before).unwrap();
        let registry = mutation_registry();
        let store = Arc::new(ActiveEnvelopeStore::new());
        store
            .declare(envelope_with_write_path("."), 0, "test-turn")
            .await
            .unwrap();
        let (ctx, _events) = test_ctx_with_store(root, store);

        let params = BatchParams {
            operations: vec![
                BatchOperation::EditFile {
                    path: "Cargo.toml".into(),
                    expected_hash: content_hash(before.as_bytes()),
                    edits: vec![EditOperation::ReplaceText {
                        old: "old".into(),
                        new: "new".into(),
                        occurrence: None,
                    }],
                },
                BatchOperation::CreateFile {
                    path: "new.txt".into(),
                    content: "created\n".into(),
                },
            ],
        };
        let result = registry
            .dispatch(Batch::NAME, serde_json::to_value(&params).unwrap(), ctx)
            .await
            .unwrap();
        let parsed: BatchResult = serde_json::from_value(result).unwrap();

        assert_eq!(parsed.created_files, vec!["new.txt"]);
        assert_eq!(parsed.edited_files, vec!["Cargo.toml"]);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("Cargo.toml")).unwrap(),
            "workspace\nnew\n"
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("new.txt")).unwrap(),
            "created\n"
        );
    }
}
