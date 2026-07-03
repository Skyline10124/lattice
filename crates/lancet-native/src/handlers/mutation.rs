// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Native CEP workspace mutation handlers.
//!
//! Implements workspace mutation methods with Guard L1 scope enforcement.
//! All methods require an active `ExecutionEnvelope`; multi-file mutations
//! perform all-or-nothing scope preflight before applying changes.

use crate::edit::apply::{PatchApplyLimits, apply_parsed_patch};
use crate::edit::content_hash;
use crate::edit::hash::line_hash;
use crate::edit::parser::parse_patch_document;
use crate::edit::storage::EditStorage;
use crate::enforcement::guard_l1::evaluate_and_enforce_policy;
use crate::enforcement::preflight::{MutationPreflight, mutation_preflight_metadata};
use crate::execution_backend::{ExecutionRouting, PathIntent, WorkspacePath};
use crate::server::registry::{HandlerContext, MethodRegistry};
use lancet_audit::{RejectionRecord, RejectionStage};
use lancet_authority::effect::ledger::{
    EffectSource, ObservedEffect, ObservedLineChange, ObservedLineChangeKind, ObservedScope,
};
use lancet_authority::effect::region::AuthorizedEffectRegion;
use lancet_authority::guard::validate_path_facts;
use lancet_authority::guard::{PathFacts, PathKind};
use lancet_catalog::CepMethod;
use lancet_catalog::method::RequiredCapability;
use lancet_catalog::methods::workspace::{
    Batch, BatchOperation, BatchParams, BatchResult, Create, CreateParams, CreateResult, Edit,
    EditOperation, EditParams, EditResult,
};
use lancet_protocol::effect_types::ImpactKind;

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

struct MutationAuth {
    execution: ExecutionRouting,
    authorized_region: AuthorizedEffectRegion,
}

impl MutationAuth {
    fn edit_storage(&self) -> EditStorage {
        EditStorage::new(self.execution.clone(), self.authorized_region.clone())
    }
}

async fn handle_create(
    params: CreateParams,
    ctx: HandlerContext,
) -> Result<CreateResult, CepError> {
    handle_create_as(params, ctx, Create::NAME).await
}

/// Handle `mutate.create` with Guard L1 scope enforcement.
///
/// Flow:
/// 1. Check WritePath scope via check_write_scope -> else ReplanRequired
/// 2. Check parent directory exists and target file does not exist
/// 3. Create file using secure containment
/// 4. Record effects and durable metadata
/// 5. Return CreateResult
async fn handle_create_as(
    params: CreateParams,
    ctx: HandlerContext,
    method: &'static str,
) -> Result<CreateResult, CepError> {
    let auth =
        authorize_workspace_mutation(&ctx, method, &params, std::slice::from_ref(&params.path))
            .await?;

    let create = preflight_create(&auth, &params, method).await?;

    write_create(&ctx, &auth, &create).await?;

    record_created_dir_effects(&ctx, &create);
    ctx.record_effect(mutation_path_effect(&auth, &create.path, &create.full_path));

    let written_hash = content_hash(params.content.as_bytes());
    ctx.record_durable_metadata(json!({
        "method": method,
        "bytes_written": create.bytes_written,
        "content_hash": written_hash,
        "created_dirs": create.created_dirs.clone(),
        "backend_invoked": true,
        "policy_enforced": true,
    }));

    Ok(CreateResult {
        path: params.path,
        bytes_written: create.bytes_written,
        created_dirs: create.created_dirs,
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
    let patch = match compile_edit_patch(&auth, &params).await {
        Ok(patch) => patch,
        Err(err) => {
            record_mutation_replan_metadata(
                &ctx,
                Edit::NAME,
                &auth,
                std::slice::from_ref(&params.path),
                expected_hash_replan_status(&err),
                replan_reason_for_error(&err),
            );
            return Err(err);
        }
    };
    let result = apply_compiled_patch(&ctx, &auth, &patch).await?;

    ctx.record_durable_metadata(json!({
        "method": Edit::NAME,
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
            BatchOperation::CreateFile {
                path,
                content,
                create_parents,
            } => {
                reject_duplicate_batch_path(&mut seen_paths, path)?;
                let params = CreateParams {
                    path: path.clone(),
                    content: content.clone(),
                    create_parents: *create_parents,
                };
                create_inputs.push(preflight_create(&auth, &params, Batch::NAME).await?);
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
        match compile_batch_edit_patch(&auth, &edit_inputs).await {
            Ok(patch) => Some(patch),
            Err(err) => {
                record_mutation_replan_metadata(
                    &ctx,
                    Batch::NAME,
                    &auth,
                    &changed_paths,
                    expected_hash_replan_status(&err),
                    replan_reason_for_error(&err),
                );
                return Err(err);
            }
        }
    };

    let mut edited_files = Vec::new();
    if let Some(patch) = patch.as_deref() {
        edited_files = apply_compiled_patch(&ctx, &auth, patch)
            .await?
            .changed_files;
    }

    let mut created_files = Vec::new();
    let mut created_dirs = Vec::new();
    let mut seen_created_dirs = HashSet::new();
    for create in &create_inputs {
        write_create(&ctx, &auth, create).await?;
        record_created_dir_effects(&ctx, create);
        for dir in &create.created_dirs {
            if seen_created_dirs.insert(dir.clone()) {
                created_dirs.push(dir.clone());
            }
        }
        ctx.record_effect(mutation_path_effect(&auth, &create.path, &create.full_path));
        created_files.push(create.path.clone());
    }

    let mut changed_files = edited_files.clone();
    changed_files.extend(created_files.iter().cloned());

    ctx.record_durable_metadata(json!({
        "method": Batch::NAME,
        "compiled_patch_backend": "hashline_patch",
        "created_files": created_files.clone(),
        "edited_files": edited_files.clone(),
        "created_dirs": created_dirs.clone(),
        "backend_invoked": true,
        "policy_enforced": true,
    }));

    Ok(BatchResult {
        changed_files,
        created_files,
        edited_files,
        created_dirs,
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
    created_dirs: Vec<String>,
}

async fn authorize_workspace_mutation(
    ctx: &HandlerContext,
    method: &str,
    params: &impl serde::Serialize,
    changed_paths: &[String],
) -> Result<MutationAuth, CepError> {
    let _evaluation = evaluate_and_enforce_policy(ctx, method, params).await?;

    authorize_mutation(ctx, method, changed_paths).await
}

async fn authorize_mutation(
    ctx: &HandlerContext,
    method: &str,
    changed_paths: &[String],
) -> Result<MutationAuth, CepError> {
    let execution = ctx.execution.clone();
    let authorized_region = ctx
        .authorized_region
        .get()
        .unwrap_or_else(AuthorizedEffectRegion::empty);

    let mut seen = HashSet::new();
    for path in changed_paths {
        if !seen.insert(path.clone()) {
            continue;
        }
        let ws_path = WorkspacePath::parse(path).map_err(|err| {
            CepError::new(
                CepErrorCode::PathContainment,
                format!("mutation path rejected: {err}"),
                CepErrorData::Validation(lancet_protocol::error::ValidationErrorData {
                    field: "path".into(),
                    reason: err.to_string(),
                }),
                Retriability::Permanent,
            )
        })?;
        ctx.mark_adapter_preflight_invoked();
        let intent = if method == Create::NAME {
            PathIntent::Create
        } else {
            PathIntent::Overwrite
        };
        let facts = execution
            .backend
            .resolve_path(&ws_path, intent)
            .await
            .map_err(|err| {
                ctx.record_rejection(RejectionRecord {
                    rejection_stage: RejectionStage::AdapterPreflight,
                    denial_source: "adapter_preflight".into(),
                });
                mutation_backend_error(method)(err)
            })?;
        let guard_path = if ws_path.is_project_root() {
            ""
        } else {
            ws_path.as_str()
        };
        let path_facts = PathFacts {
            normalized_path: facts.normalized_path.clone(),
            exists: facts.exists,
            kind: facts.kind.map(path_kind),
            parent_exists: facts.parent_exists,
            parent_kind: facts.parent_kind.map(path_kind),
            canonical_under_workspace: facts.canonical_under_workspace,
        };
        if let Err(err) = validate_path_facts(guard_path, &path_facts, &authorized_region) {
            ctx.record_rejection(RejectionRecord {
                rejection_stage: RejectionStage::AdapterPreflight,
                denial_source: "guard_l1".into(),
            });
            return Err(err);
        }
    }

    Ok(MutationAuth {
        execution,
        authorized_region,
    })
}

fn path_kind(kind: crate::execution_backend::PathKind) -> PathKind {
    match kind {
        crate::execution_backend::PathKind::File => PathKind::File,
        crate::execution_backend::PathKind::Directory => PathKind::Directory,
        crate::execution_backend::PathKind::Symlink => PathKind::Symlink,
    }
}

fn mutation_backend_error(
    method: &str,
) -> impl FnOnce(crate::execution_backend::ExecutionBackendError) -> CepError {
    move |err| {
        CepError::new(
            CepErrorCode::ExecutionFailed,
            format!("{method} backend preflight failed: {err}"),
            CepErrorData::Execution(lancet_protocol::error::ExecutionErrorData {
                command: method.into(),
                exit_code: None,
                stderr: Some(err.to_string()),
            }),
            Retriability::Permanent,
        )
    }
}

#[allow(clippy::too_many_arguments)]
fn record_mutation_preflight(
    ctx: &HandlerContext,
    method: &str,
    changed_paths: &[String],
    write_patterns: &[String],
    out_of_scope_paths: &[String],
    expected_hash_status: Option<&str>,
    replan_reason: Option<&str>,
    backend_invoked: bool,
) {
    ctx.record_durable_metadata(mutation_preflight_metadata(MutationPreflight {
        method,
        changed_paths,
        write_patterns,
        out_of_scope_paths,
        expected_hash_status,
        replan_reason,
        backend_invoked,
    }));
}

fn record_mutation_replan_metadata(
    ctx: &HandlerContext,
    method: &str,
    _auth: &MutationAuth,
    changed_paths: &[String],
    expected_hash_status: Option<&str>,
    replan_reason: Option<&str>,
) {
    record_mutation_preflight(
        ctx,
        method,
        changed_paths,
        &[],
        &[],
        expected_hash_status,
        replan_reason,
        false,
    );
}

fn expected_hash_replan_status(err: &CepError) -> Option<&'static str> {
    if err.code == CepErrorCode::ReplanRequired && err.message.contains("expected_hash") {
        Some("stale")
    } else {
        None
    }
}

fn replan_reason_for_error(err: &CepError) -> Option<&'static str> {
    if err.code != CepErrorCode::ReplanRequired {
        return None;
    }
    if err.message.contains("expected_hash") {
        Some("expected_hash_stale")
    } else {
        Some("semantic_replan")
    }
}

async fn preflight_create(
    auth: &MutationAuth,
    params: &CreateParams,
    method: &str,
) -> Result<PreflightCreate, CepError> {
    let storage = auth.edit_storage();
    storage
        .preflight_create(&params.path)
        .await
        .map_err(|diag| {
            CepError::new(
                CepErrorCode::ExecutionFailed,
                diag.message.clone(),
                CepErrorData::Validation(lancet_protocol::error::ValidationErrorData {
                    field: "path".into(),
                    reason: diag.message,
                }),
                Retriability::Permanent,
            )
        })?;
    let created_dirs = if params.create_parents {
        missing_parent_dirs(auth, &params.path).await?
    } else {
        let parent = Path::new(&params.path)
            .parent()
            .and_then(|p| p.to_str())
            .filter(|p| !p.is_empty());
        if let Some(parent_path) = parent
            && !parent_exists(auth, parent_path).await?
        {
            return Err(CepError::new(
                CepErrorCode::ExecutionFailed,
                format!(
                    "parent directory for '{}' does not exist; set create_parents=true on {method} to create missing parent directories",
                    params.path
                ),
                CepErrorData::Validation(lancet_protocol::error::ValidationErrorData {
                    field: "create_parents".into(),
                    reason: "missing parent directories require create_parents=true".into(),
                }),
                Retriability::Permanent,
            ));
        }
        Vec::new()
    };
    Ok(PreflightCreate {
        path: params.path.clone(),
        content: params.content.clone(),
        full_path: PathBuf::from(&params.path),
        bytes_written: params.content.len() as u64,
        created_dirs,
    })
}

async fn parent_exists(auth: &MutationAuth, path: &str) -> Result<bool, CepError> {
    let ws_path =
        WorkspacePath::parse(path).map_err(|err| validation_error("path", err.to_string()))?;
    let facts = auth
        .execution
        .backend
        .resolve_path(&ws_path, PathIntent::ListDirectory)
        .await
        .map_err(mutation_backend_error("mutate.create"))?;
    Ok(facts.exists)
}

/// Walk ancestor directories of `relative_path` via the Upper backend, returning
/// the list of missing ancestors.  All filesystem probing goes through
/// `ExecutionBackend::resolve_path` — never the host filesystem.
async fn missing_parent_dirs(
    auth: &MutationAuth,
    relative_path: &str,
) -> Result<Vec<String>, CepError> {
    let ws_path = WorkspacePath::parse(relative_path)
        .map_err(|err| validation_error("path", err.to_string()))?;

    // Root-level files (no parent directory) have no missing parents.
    let normalized = ws_path.as_str();
    if !normalized.contains('/') {
        return Ok(Vec::new());
    }

    // Walk ancestor directories from root to the file's parent.
    let components: Vec<&str> = normalized.split('/').collect();
    let mut missing = Vec::new();
    let mut current = String::new();

    // All components except the last (the file itself).
    for (i, component) in components.iter().enumerate() {
        if i == 0 {
            current = component.to_string();
        } else {
            current = format!("{current}/{component}");
        }

        // Skip the last component (the file itself).
        if i == components.len() - 1 {
            break;
        }

        let ancestor = WorkspacePath::parse(&current)
            .map_err(|err| validation_error("path", err.to_string()))?;
        let facts = auth
            .execution
            .backend
            .resolve_path(&ancestor, PathIntent::ListDirectory)
            .await
            .map_err(mutation_backend_error("mutate.create"))?;

        if !facts.exists {
            missing.push(current.clone());
            continue;
        }

        if matches!(
            facts.kind,
            Some(crate::execution_backend::PathKind::Symlink)
        ) {
            return Err(CepError::new(
                CepErrorCode::PathContainment,
                format!("parent directory '{current}' is a symlink"),
                CepErrorData::Validation(lancet_protocol::error::ValidationErrorData {
                    field: "path".into(),
                    reason: "parent directory is a symlink".into(),
                }),
                Retriability::Permanent,
            ));
        }

        if !matches!(
            facts.kind,
            Some(crate::execution_backend::PathKind::Directory)
        ) {
            return Err(CepError::new(
                CepErrorCode::ExecutionFailed,
                format!("parent path '{current}' is not a directory"),
                CepErrorData::Validation(lancet_protocol::error::ValidationErrorData {
                    field: "path".into(),
                    reason: "parent path is not a directory".into(),
                }),
                Retriability::Permanent,
            ));
        }
    }

    Ok(missing)
}

async fn write_create(
    ctx: &HandlerContext,
    auth: &MutationAuth,
    create: &PreflightCreate,
) -> Result<(), CepError> {
    let storage = auth.edit_storage();
    let ws_path = WorkspacePath::parse(&create.path)
        .map_err(|err| validation_error("path", err.to_string()))?;
    ctx.mark_backend_invoked();
    storage
        .write_edit_target(&create.path, &ws_path, create.content.as_bytes(), 0)
        .await
        .map_err(patch_diag_to_cep)
}

fn patch_diag_to_cep(diag: crate::edit::types::PatchDiagnostic) -> CepError {
    CepError::new(
        CepErrorCode::ExecutionFailed,
        diag.message.clone(),
        CepErrorData::Execution(lancet_protocol::error::ExecutionErrorData {
            command: "mutate.create".into(),
            exit_code: Some(1),
            stderr: Some(diag.message),
        }),
        Retriability::Permanent,
    )
}

fn mutation_path_effect(_auth: &MutationAuth, path: &str, _full_path: &Path) -> ObservedEffect {
    let scope = ObservedScope::Path(PathBuf::from(path));
    ObservedEffect {
        kind: ImpactKind::WorkspaceMutation,
        scope,
        required_capability: RequiredCapability::WritePath,
        source: EffectSource::HandlerBackend,
    }
}

fn record_created_dir_effects(ctx: &HandlerContext, create: &PreflightCreate) {
    for dir in &create.created_dirs {
        ctx.record_effect(ObservedEffect {
            kind: ImpactKind::WorkspaceMutation,
            scope: ObservedScope::Path(PathBuf::from(dir)),
            required_capability: RequiredCapability::WritePath,
            source: EffectSource::HandlerBackend,
        });
    }
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

async fn compile_edit_patch(auth: &MutationAuth, params: &EditParams) -> Result<String, CepError> {
    let fragment = compile_edit_operation_fragment(&auth.edit_storage(), params).await?;
    Ok(format!("*** Begin Patch\n{fragment}*** End Patch\n"))
}

async fn compile_batch_edit_patch(
    auth: &MutationAuth,
    edits: &[EditParams],
) -> Result<String, CepError> {
    let mut patch = String::from("*** Begin Patch\n");
    for edit in edits {
        let fragment = compile_edit_operation_fragment(&auth.edit_storage(), edit).await?;
        patch.push_str(&fragment);
    }
    patch.push_str("*** End Patch\n");
    Ok(patch)
}

async fn compile_edit_operation_fragment(
    storage: &EditStorage,
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

    let before = storage
        .read_for_compile(&params.path)
        .await
        .map_err(|diag| {
            CepError::new(
                CepErrorCode::ExecutionFailed,
                format!(
                    "cannot read edit target '{}': {}",
                    params.path, diag.message
                ),
                CepErrorData::Execution(lancet_protocol::error::ExecutionErrorData {
                    command: "mutate.edit.compile".into(),
                    exit_code: Some(1),
                    stderr: Some(diag.message),
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
    auth: &MutationAuth,
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

    let limits = PatchApplyLimits::default();
    let storage = auth.edit_storage();
    let result = apply_parsed_patch(&storage, &limits, document).await;

    if !result.ok {
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
        let abs_path = PathBuf::from(changed_path);
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

#[cfg(test)]
mod create_tests {
    use super::*;
    use crate::execution_backend::{
        DirectoryEntry, ExecutionBackend, ExecutionBackendError, ExecutionRouting, FindRequest,
        GrepMatch, GrepRequest, PathFacts, PathIntent, UpperContext, WorkspacePath,
    };
    use crate::server::registry::{Cancellation, EventSink};
    use lancet_authority::policy::CepTurnContext;
    use lancet_authority::policy::default::WorkspacePolicy;

    use std::collections::BTreeMap;
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::{Arc, Mutex};
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
                cep_context: Arc::new(
                    lancet_authority::policy::CepTurnContext::defaults_for_project_root(root),
                ),
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
                cep_mode: lancet_protocol::policy::CepMode::Off,
                execution: crate::execution_backend::ExecutionRouting {
                    backend: std::sync::Arc::new(crate::execution_backend::NullExecutionBackend),
                    upper: crate::execution_backend::UpperContext {
                        execution_backend: "null".into(),
                        upper_workspace_id: "null".into(),
                    },
                },
                routing_invocation: crate::execution_backend::RoutingInvocationState::default(),
            },
            event_rx,
        )
    }

    fn mutation_registry() -> MethodRegistry {
        let mut registry = MethodRegistry::new();
        register_mutation_methods(&mut registry).unwrap();
        registry
    }

    /// A spy backend that simulates a filesystem in memory for Direct-mode
    /// mutation tests.
    #[derive(Default)]
    struct CreateSpyBackend {
        files: Mutex<BTreeMap<String, Vec<u8>>>,
        writes: Mutex<Vec<String>>,
    }

    impl CreateSpyBackend {
        fn file_facts(
            path: &str,
            exists: bool,
            kind: crate::execution_backend::PathKind,
        ) -> PathFacts {
            PathFacts {
                normalized_path: path.to_string(),
                exists,
                kind: Some(kind),
                parent_exists: true,
                parent_kind: Some(crate::execution_backend::PathKind::Directory),
                byte_size: None,
                canonical_under_workspace: true,
            }
        }
    }

    impl ExecutionBackend for CreateSpyBackend {
        fn backend_name(&self) -> &str {
            "create-spy"
        }

        fn resolve_path(
            &self,
            path: &WorkspacePath,
            intent: PathIntent,
        ) -> Pin<Box<dyn Future<Output = Result<PathFacts, ExecutionBackendError>> + Send + '_>>
        {
            let path_str = path.as_str().to_string();
            let files = self.files.lock().unwrap();
            let exists = files.contains_key(&path_str);
            let kind = if exists {
                crate::execution_backend::PathKind::File
            } else {
                crate::execution_backend::PathKind::Directory
            };
            let facts = match intent {
                PathIntent::Create => Self::file_facts(&path_str, false, kind),
                _ => Self::file_facts(&path_str, exists, kind),
            };
            Box::pin(async move { Ok(facts) })
        }

        fn read_file(
            &self,
            path: &WorkspacePath,
        ) -> Pin<Box<dyn Future<Output = Result<Vec<u8>, ExecutionBackendError>> + Send + '_>>
        {
            let files = self.files.lock().unwrap();
            let bytes = files.get(path.as_str()).cloned().unwrap_or_default();
            Box::pin(async move { Ok(bytes) })
        }

        fn write_file(
            &self,
            path: &WorkspacePath,
            content: &[u8],
        ) -> Pin<Box<dyn Future<Output = Result<(), ExecutionBackendError>> + Send + '_>> {
            self.writes.lock().unwrap().push(path.as_str().to_string());
            self.files
                .lock()
                .unwrap()
                .insert(path.as_str().to_string(), content.to_vec());
            Box::pin(async { Ok(()) })
        }

        fn remove_file(
            &self,
            _path: &WorkspacePath,
        ) -> Pin<Box<dyn Future<Output = Result<(), ExecutionBackendError>> + Send + '_>> {
            Box::pin(async { Ok(()) })
        }

        fn list_dir(
            &self,
            _path: &WorkspacePath,
        ) -> Pin<
            Box<
                dyn Future<Output = Result<Vec<DirectoryEntry>, ExecutionBackendError>> + Send + '_,
            >,
        > {
            Box::pin(async { Ok(Vec::new()) })
        }

        fn grep(
            &self,
            _request: &GrepRequest,
        ) -> Pin<Box<dyn Future<Output = Result<Vec<GrepMatch>, ExecutionBackendError>> + Send + '_>>
        {
            Box::pin(async { Ok(Vec::new()) })
        }

        fn find_files(
            &self,
            _request: &FindRequest,
        ) -> Pin<
            Box<dyn Future<Output = Result<Vec<WorkspacePath>, ExecutionBackendError>> + Send + '_>,
        > {
            Box::pin(async { Ok(Vec::new()) })
        }

        fn changed_paths(
            &self,
        ) -> Pin<
            Box<dyn Future<Output = Result<Vec<WorkspacePath>, ExecutionBackendError>> + Send + '_>,
        > {
            Box::pin(async { Ok(Vec::new()) })
        }
    }

    fn ctx_with_denied_patterns(
        root: &str,
        backend: Arc<CreateSpyBackend>,
        denied_patterns: &[&str],
    ) -> HandlerContext {
        let (event_tx, _rx) = mpsc::channel(8);
        let (_cancel_tx, cancel_rx) = watch::channel(false);
        let mut policy_cfg =
            lancet_authority::policy::default::PolicyConfig::defaults_for_project_root(
                std::path::PathBuf::from(root),
            );
        policy_cfg.workspace = WorkspacePolicy {
            denied_write_patterns: denied_patterns.iter().map(|s| (*s).into()).collect(),
        };
        let cep_context = Arc::new(CepTurnContext::from_policy(
            lancet_authority::policy::CepPolicyConfig::defaults_for_project_root(root),
        ));
        let default_hook = Arc::new(lancet_authority::policy::default::DefaultPolicyHook::new(
            policy_cfg,
        ));
        HandlerContext {
            call_id: 1,
            events: EventSink::new(event_tx, 1),
            cancellation: Cancellation::new(cancel_rx),
            cep_context,
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
            execution: ExecutionRouting {
                backend,
                upper: UpperContext {
                    execution_backend: "create-spy".into(),
                    upper_workspace_id: "upper-1".into(),
                },
            },
            routing_invocation: crate::execution_backend::RoutingInvocationState::default(),
        }
    }

    #[tokio::test]
    async fn create_with_null_backend_returns_execution_failed() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().to_str().unwrap();
        let registry = mutation_registry();
        let (ctx, _events) = test_ctx(root);

        let params = CreateParams {
            path: "test.txt".into(),
            content: "hello".into(),
            create_parents: false,
        };
        let err = registry
            .dispatch(Create::NAME, serde_json::to_value(&params).unwrap(), ctx)
            .await
            .unwrap_err();
        // With a NullExecutionBackend, the backend returns an error during
        // preflight path resolution.
        assert_eq!(err.code, CepErrorCode::ExecutionFailed);
    }

    #[tokio::test]
    async fn create_in_scope_succeeds() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().to_str().unwrap();
        let registry = mutation_registry();
        let backend = Arc::new(CreateSpyBackend::default());
        let ctx = ctx_with_denied_patterns(root, Arc::clone(&backend), &[]);

        let params = CreateParams {
            path: "test.txt".into(),
            content: "hello world".into(),
            create_parents: false,
        };
        let result = registry
            .dispatch(Create::NAME, serde_json::to_value(&params).unwrap(), ctx)
            .await
            .unwrap();
        let parsed: CreateResult = serde_json::from_value(result).unwrap();
        assert_eq!(parsed.path, "test.txt");
        assert_eq!(parsed.bytes_written, 11);
        assert!(
            backend
                .writes
                .lock()
                .unwrap()
                .contains(&"test.txt".to_string())
        );
    }

    #[tokio::test]
    async fn create_out_of_scope_returns_replan_required() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().to_str().unwrap();
        let registry = mutation_registry();
        let backend = Arc::new(CreateSpyBackend::default());
        let ctx = ctx_with_denied_patterns(root, backend, &["README.md"]);

        let params = CreateParams {
            path: "README.md".into(),
            content: "out of scope".into(),
            create_parents: false,
        };
        let err = registry
            .dispatch(Create::NAME, serde_json::to_value(&params).unwrap(), ctx)
            .await
            .unwrap_err();
        // Blacklist semantics: explicitly denied paths are rejected by the guard.
        assert_eq!(err.code, CepErrorCode::GuardDenied);
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
            create_parents: false,
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
    use crate::execution_backend::{
        DirectoryEntry, ExecutionBackend, ExecutionBackendError, ExecutionRouting, FindRequest,
        GrepMatch, GrepRequest, PathFacts, PathIntent, UpperContext, WorkspacePath,
    };
    use crate::server::registry::{Cancellation, EventSink};
    use lancet_authority::policy::CepTurnContext;
    use lancet_authority::policy::default::WorkspacePolicy;

    use std::collections::BTreeMap;
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::{Arc, Mutex};
    use tempfile::TempDir;
    use tokio::sync::{mpsc, watch};

    fn mutation_registry() -> MethodRegistry {
        let mut registry = MethodRegistry::new();
        register_mutation_methods(&mut registry).unwrap();
        registry
    }

    /// A spy backend that simulates a filesystem in memory for Direct-mode
    /// edit/batch mutation tests.
    #[derive(Default)]
    struct MutSpyBackend {
        files: Mutex<BTreeMap<String, Vec<u8>>>,
        writes: Mutex<Vec<String>>,
    }

    impl MutSpyBackend {
        fn file_facts(
            path: &str,
            exists: bool,
            kind: crate::execution_backend::PathKind,
        ) -> PathFacts {
            PathFacts {
                normalized_path: path.to_string(),
                exists,
                kind: Some(kind),
                parent_exists: true,
                parent_kind: Some(crate::execution_backend::PathKind::Directory),
                byte_size: None,
                canonical_under_workspace: true,
            }
        }
    }

    impl ExecutionBackend for MutSpyBackend {
        fn backend_name(&self) -> &str {
            "mut-spy"
        }

        fn resolve_path(
            &self,
            path: &WorkspacePath,
            intent: PathIntent,
        ) -> Pin<Box<dyn Future<Output = Result<PathFacts, ExecutionBackendError>> + Send + '_>>
        {
            let path_str = path.as_str().to_string();
            let files = self.files.lock().unwrap();
            let exists = files.contains_key(&path_str);
            let kind = if exists {
                crate::execution_backend::PathKind::File
            } else {
                crate::execution_backend::PathKind::Directory
            };
            let facts = match intent {
                PathIntent::Create => Self::file_facts(&path_str, false, kind),
                _ => Self::file_facts(&path_str, exists, kind),
            };
            Box::pin(async move { Ok(facts) })
        }

        fn read_file(
            &self,
            path: &WorkspacePath,
        ) -> Pin<Box<dyn Future<Output = Result<Vec<u8>, ExecutionBackendError>> + Send + '_>>
        {
            let files = self.files.lock().unwrap();
            let bytes = files.get(path.as_str()).cloned().unwrap_or_default();
            Box::pin(async move { Ok(bytes) })
        }

        fn write_file(
            &self,
            path: &WorkspacePath,
            content: &[u8],
        ) -> Pin<Box<dyn Future<Output = Result<(), ExecutionBackendError>> + Send + '_>> {
            self.writes.lock().unwrap().push(path.as_str().to_string());
            self.files
                .lock()
                .unwrap()
                .insert(path.as_str().to_string(), content.to_vec());
            Box::pin(async { Ok(()) })
        }

        fn remove_file(
            &self,
            _path: &WorkspacePath,
        ) -> Pin<Box<dyn Future<Output = Result<(), ExecutionBackendError>> + Send + '_>> {
            Box::pin(async { Ok(()) })
        }

        fn list_dir(
            &self,
            _path: &WorkspacePath,
        ) -> Pin<
            Box<
                dyn Future<Output = Result<Vec<DirectoryEntry>, ExecutionBackendError>> + Send + '_,
            >,
        > {
            Box::pin(async { Ok(Vec::new()) })
        }

        fn grep(
            &self,
            _request: &GrepRequest,
        ) -> Pin<Box<dyn Future<Output = Result<Vec<GrepMatch>, ExecutionBackendError>> + Send + '_>>
        {
            Box::pin(async { Ok(Vec::new()) })
        }

        fn find_files(
            &self,
            _request: &FindRequest,
        ) -> Pin<
            Box<dyn Future<Output = Result<Vec<WorkspacePath>, ExecutionBackendError>> + Send + '_>,
        > {
            Box::pin(async { Ok(Vec::new()) })
        }

        fn changed_paths(
            &self,
        ) -> Pin<
            Box<dyn Future<Output = Result<Vec<WorkspacePath>, ExecutionBackendError>> + Send + '_>,
        > {
            let writes = self.writes.lock().unwrap();
            let paths: Vec<WorkspacePath> = writes
                .iter()
                .filter_map(|p| WorkspacePath::parse(p).ok())
                .collect();
            Box::pin(async move { Ok(paths) })
        }
    }

    fn mut_ctx(root: &str, backend: Arc<MutSpyBackend>) -> HandlerContext {
        let (event_tx, _rx) = mpsc::channel(8);
        let (_cancel_tx, cancel_rx) = watch::channel(false);
        let mut policy_cfg =
            lancet_authority::policy::default::PolicyConfig::defaults_for_project_root(
                std::path::PathBuf::from(root),
            );
        policy_cfg.workspace = WorkspacePolicy {
            denied_write_patterns: Vec::new(),
        };
        let cep_context = Arc::new(CepTurnContext::from_policy(
            lancet_authority::policy::CepPolicyConfig::defaults_for_project_root(root),
        ));
        let default_hook = Arc::new(lancet_authority::policy::default::DefaultPolicyHook::new(
            policy_cfg,
        ));
        HandlerContext {
            call_id: 1,
            events: EventSink::new(event_tx, 1),
            cancellation: Cancellation::new(cancel_rx),
            cep_context,
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
            audit: Arc::new(crate::audit::CepAuditStore::in_memory().expect("audit")),
            cep_mode: lancet_protocol::policy::CepMode::Off,
            execution: ExecutionRouting {
                backend,
                upper: UpperContext {
                    execution_backend: "mut-spy".into(),
                    upper_workspace_id: "upper-1".into(),
                },
            },
            routing_invocation: crate::execution_backend::RoutingInvocationState::default(),
        }
    }

    #[tokio::test]
    async fn edit_semantic_replace_computes_hashline_in_runtime() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().to_str().unwrap();
        let before = "alpha\nbeta\n";
        let registry = mutation_registry();
        let backend = Arc::new(MutSpyBackend::default());
        backend
            .files
            .lock()
            .unwrap()
            .insert("target.txt".into(), before.as_bytes().to_vec());
        let ctx = mut_ctx(root, Arc::clone(&backend));

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
        let updated = String::from_utf8(
            backend
                .files
                .lock()
                .unwrap()
                .get("target.txt")
                .cloned()
                .unwrap_or_default(),
        )
        .unwrap();
        assert_eq!(updated, "alpha\ngamma\n");
    }

    #[tokio::test]
    async fn edit_ambiguous_text_requires_occurrence() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().to_str().unwrap();
        let before = "same\nsame\n";
        let registry = mutation_registry();
        let backend = Arc::new(MutSpyBackend::default());
        backend
            .files
            .lock()
            .unwrap()
            .insert("target.txt".into(), before.as_bytes().to_vec());
        let ctx = mut_ctx(root, Arc::clone(&backend));

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
    }

    #[tokio::test]
    async fn edit_stale_expected_hash_records_preflight_replan_metadata() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().to_str().unwrap();
        let before = "alpha\nbeta\n";
        let registry = mutation_registry();
        let backend = Arc::new(MutSpyBackend::default());
        backend
            .files
            .lock()
            .unwrap()
            .insert("target.txt".into(), before.as_bytes().to_vec());
        let ctx = mut_ctx(root, Arc::clone(&backend));
        let durable = ctx.durable_metadata.clone();

        let params = EditParams {
            path: "target.txt".into(),
            expected_hash:
                "blake3:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
            edits: vec![EditOperation::ReplaceText {
                old: "beta".into(),
                new: "gamma".into(),
                occurrence: None,
            }],
        };
        let err = registry
            .dispatch(Edit::NAME, serde_json::to_value(&params).unwrap(), ctx)
            .await
            .unwrap_err();

        assert_eq!(err.code, CepErrorCode::ReplanRequired);
        let metadata: serde_json::Value =
            serde_json::from_str(&durable.take_json().expect("durable metadata")).unwrap();
        assert_eq!(metadata["cep_preflight"]["expected_hash_status"], "stale");
        assert_eq!(
            metadata["cep_preflight"]["replan_reason"],
            "expected_hash_stale"
        );
        assert_eq!(
            metadata["cep_preflight"]["changed_paths"],
            serde_json::json!(["target.txt"])
        );
    }

    #[tokio::test]
    async fn batch_create_and_edit_without_delete_succeeds() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().to_str().unwrap();
        let before = "workspace\nold\n";
        let registry = mutation_registry();
        let backend = Arc::new(MutSpyBackend::default());
        backend
            .files
            .lock()
            .unwrap()
            .insert("Cargo.toml".into(), before.as_bytes().to_vec());
        let ctx = mut_ctx(root, Arc::clone(&backend));

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
                    create_parents: false,
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
        let updated = String::from_utf8(
            backend
                .files
                .lock()
                .unwrap()
                .get("Cargo.toml")
                .cloned()
                .unwrap_or_default(),
        )
        .unwrap();
        assert_eq!(updated, "workspace\nnew\n");
    }

    #[tokio::test]
    async fn batch_create_file_with_create_parents_creates_missing_dirs() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().to_str().unwrap();
        let registry = mutation_registry();
        let backend = Arc::new(MutSpyBackend::default());
        let ctx = mut_ctx(root, Arc::clone(&backend));

        let params = BatchParams {
            operations: vec![
                BatchOperation::CreateFile {
                    path: "crates/lattice-tui/src/lib.rs".into(),
                    content: "pub fn run() {}\n".into(),
                    create_parents: true,
                },
                BatchOperation::CreateFile {
                    path: "crates/lattice-tui/src/main.rs".into(),
                    content: "fn main() {}\n".into(),
                    create_parents: true,
                },
            ],
        };
        let result = registry
            .dispatch(Batch::NAME, serde_json::to_value(&params).unwrap(), ctx)
            .await
            .unwrap();
        let parsed: BatchResult = serde_json::from_value(result).unwrap();

        assert_eq!(
            parsed.created_files,
            vec![
                "crates/lattice-tui/src/lib.rs",
                "crates/lattice-tui/src/main.rs"
            ]
        );
    }

    #[tokio::test]
    async fn mutation_create_uses_backend_only() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().to_str().unwrap();
        let registry = mutation_registry();
        let backend = Arc::new(MutSpyBackend::default());
        let ctx = mut_ctx(root, Arc::clone(&backend));
        let params = CreateParams {
            path: "new.txt".into(),
            content: "created-on-upper".into(),
            create_parents: false,
        };
        registry
            .dispatch(Create::NAME, serde_json::to_value(&params).unwrap(), ctx)
            .await
            .unwrap();
        assert!(
            backend
                .writes
                .lock()
                .unwrap()
                .contains(&"new.txt".to_string())
        );
        assert!(!dir.path().join("new.txt").exists());
    }
}
