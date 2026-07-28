// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Native CEP workspace mutation handlers.
//!
//! Implements workspace mutation methods with Guard L1 scope enforcement
//! against the one concrete [`crate::workspace::LocalWorkspace`]: secure
//! resolution, Mutation Preconditions (expected hash / no-replace), and
//! atomic installation all live behind that module.

use crate::edit::apply::{PatchApplyLimits, apply_parsed_patch};
use crate::edit::content_hash;
use crate::edit::hash::line_hash;
use crate::edit::parser::parse_patch_document;
use crate::edit::storage::EditStorage;
use crate::enforcement::guard_l1::evaluate_and_enforce_policy;
use crate::enforcement::preflight::{MutationPreflight, mutation_preflight_metadata};
use crate::server::registry::{HandlerContext, MethodRegistry};
use crate::workspace::{LocalWorkspace, LocalWorkspaceError, PathIntent, WorkspacePath};
use lancet_audit::{RejectionRecord, RejectionStage};
use lancet_authority::effect::ledger::{
    EffectSource, ObservedEffect, ObservedLineChange, ObservedLineChangeKind, ObservedScope,
};
use lancet_authority::effect::region::AuthorizedEffectRegion;
use lancet_authority::guard::validate_path_facts;
use lancet_catalog::CepMethod;
use lancet_catalog::method::RequiredCapability;
use lancet_catalog::methods::workspace::{
    Create, CreateParams, CreateResult, Edit, EditOperation, EditParams, EditResult,
};
use lancet_protocol::effect_types::ImpactKind;

use lancet_protocol::error::{CepErrorData, GuardErrorData};
use lancet_protocol::{CepError, CepErrorCode, Retriability};
use serde_json::json;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

/// Register native workspace mutation methods.
pub fn register_mutation_methods(registry: &mut MethodRegistry) -> Result<(), CepError> {
    registry.register_method::<Create, _, _>(handle_create)?;
    registry.register_method::<Edit, _, _>(handle_edit)?;
    Ok(())
}

struct MutationAuth {
    workspace: Arc<LocalWorkspace>,
    authorized_region: AuthorizedEffectRegion,
}

impl MutationAuth {
    fn edit_storage(&self) -> EditStorage {
        EditStorage::new(Arc::clone(&self.workspace), self.authorized_region.clone())
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

    let created = write_create(&auth, &create).await?;

    // #63: the mutation is installed. Everything from here on is evidence —
    // a later failure never un-installs it and never fakes a rollback.
    let history = record_mutation_history(
        &ctx,
        &auth,
        MutationHistoryInput {
            method,
            path: created.path.as_str(),
            before: None,
            after: create.content.as_bytes(),
            content_hash: &created.content_hash,
        },
    )
    .await;

    record_created_dir_effects(&ctx, &create);
    ctx.record_effect(mutation_path_effect(&create.path));

    ctx.record_durable_metadata(json!({
        "method": method,
        "bytes_written": created.bytes_written,
        "content_hash": created.content_hash,
        "created_dirs": create.created_dirs.clone(),
        "guarded_path": created.path.as_str(),
        "policy_enforced": true,
        "workspace_history": history,
    }));

    Ok(CreateResult {
        path: params.path,
        bytes_written: created.bytes_written,
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
        Err(failure) => {
            record_mutation_replan_metadata(
                &ctx,
                Edit::NAME,
                std::slice::from_ref(&params.path),
                failure.local_revision.as_deref(),
                replan_reason_for_error(&failure.error),
            );
            return Err(failure.error);
        }
    };
    let result = apply_compiled_patch(&ctx, &auth, &patch.patch).await?;

    // Installation evidence: the post-install Local Revision of the target.
    // The hashline apply re-verifies the old hash at write time, so a
    // successful apply installed exactly the compiled `after` content — its
    // hash is the post-install Local Revision, with no re-read and no
    // race window.
    let ws_path = WorkspacePath::parse(&params.path)
        .map_err(|err| validation_error("path", err.to_string()))?;

    // #63: the compiled `before`/`after` content is exactly what the apply
    // installed, so the history records the real before/after state without
    // re-reading the Local Workspace.
    let history = record_mutation_history(
        &ctx,
        &auth,
        MutationHistoryInput {
            method: Edit::NAME,
            path: ws_path.as_str(),
            before: Some(patch.before.as_bytes()),
            after: patch.after.as_bytes(),
            content_hash: &patch.post_install_revision,
        },
    )
    .await;

    ctx.record_durable_metadata(json!({
        "method": Edit::NAME,
        "compiled_patch_format": "hashline_patch",
        "changed_files": result.changed_files.clone(),
        "guarded_path": ws_path.as_str(),
        "content_hash": patch.post_install_revision,
        "policy_enforced": true,
        "workspace_history": history,
    }));

    Ok(EditResult {
        changed_files: result.changed_files,
    })
}

struct CompiledPatchApply {
    changed_files: Vec<String>,
}

struct PreflightCreate {
    path: String,
    content: String,
    created_dirs: Vec<String>,
}

/// A failed edit compile. Carries the Local Revision observed at the
/// Mutation Precondition check when the failure is a stale expected hash,
/// so the replan evidence records the real precondition determination.
struct EditCompileFailure {
    error: CepError,
    local_revision: Option<String>,
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
    let workspace = Arc::clone(&ctx.workspace);
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
        let intent = if method == Create::NAME {
            PathIntent::Create
        } else {
            PathIntent::Overwrite
        };
        let facts = workspace
            .resolve_facts(&ws_path, intent)
            .await
            .map_err(|err| {
                ctx.record_rejection(RejectionRecord {
                    rejection_stage: RejectionStage::PathGuard,
                    denial_source: "local_workspace".into(),
                });
                mutation_workspace_error(method)(err)
            })?;
        let guard_path = if ws_path.is_project_root() {
            ""
        } else {
            ws_path.as_str()
        };
        if let Err(err) = validate_path_facts(guard_path, &facts.to_guard(), &authorized_region) {
            ctx.record_rejection(RejectionRecord {
                rejection_stage: RejectionStage::PathGuard,
                denial_source: "guard_l1".into(),
            });
            return Err(err);
        }
    }

    Ok(MutationAuth {
        workspace,
        authorized_region,
    })
}

fn mutation_workspace_error(method: &str) -> impl FnOnce(LocalWorkspaceError) -> CepError {
    move |err| {
        CepError::new(
            CepErrorCode::ExecutionFailed,
            format!("{method} workspace preflight failed: {err}"),
            CepErrorData::Execution(lancet_protocol::error::ExecutionErrorData {
                command: method.into(),
                exit_code: None,
                stderr: Some(err.to_string()),
            }),
            Retriability::Permanent,
        )
    }
}

fn record_mutation_replan_metadata(
    ctx: &HandlerContext,
    method: &str,
    changed_paths: &[String],
    local_revision: Option<&str>,
    replan_reason: Option<&str>,
) {
    ctx.record_durable_metadata(mutation_preflight_metadata(MutationPreflight {
        method,
        changed_paths,
        write_patterns: &[],
        out_of_scope_paths: &[],
        local_revision,
        replan_reason,
    }));
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
        .map_err(|diag| create_preflight_diag_to_cep(diag, &params.path))?;
    let created_dirs = if params.create_parents {
        missing_parent_dirs(auth, &params.path).await?
    } else {
        let parent = std::path::Path::new(&params.path)
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
        created_dirs,
    })
}

async fn parent_exists(auth: &MutationAuth, path: &str) -> Result<bool, CepError> {
    let ws_path =
        WorkspacePath::parse(path).map_err(|err| validation_error("path", err.to_string()))?;
    let facts = auth
        .workspace
        .resolve_facts(&ws_path, PathIntent::ListDirectory)
        .await
        .map_err(mutation_workspace_error("mutate.create"))?;
    Ok(facts.exists)
}

/// Walk ancestor directories of `relative_path` through the concrete Local
/// Workspace, returning the list of missing ancestors. All filesystem
/// probing goes through secure resolution — never the raw host filesystem.
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
            .workspace
            .resolve_facts(&ancestor, PathIntent::ListDirectory)
            .await
            .map_err(mutation_workspace_error("mutate.create"))?;

        if !facts.exists {
            missing.push(current.clone());
            continue;
        }

        if matches!(facts.kind, Some(crate::workspace::PathKind::Symlink)) {
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

        if !matches!(facts.kind, Some(crate::workspace::PathKind::Directory)) {
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
    auth: &MutationAuth,
    create: &PreflightCreate,
) -> Result<crate::workspace::CreatedFile, CepError> {
    let storage = auth.edit_storage();
    let ws_path = WorkspacePath::parse(&create.path)
        .map_err(|err| validation_error("path", err.to_string()))?;
    // ADR 0003 decision 8: no-replace install. A concurrent writer between
    // `preflight_create` and this install surfaces as a create-target-exists
    // error, which the Agent must observe and replan rather than have
    // silently overwritten.
    storage
        .create_edit_target(&create.path, &ws_path, create.content.as_bytes())
        .await
        .map_err(create_diag_to_cep)
}

/// Map a create-install diagnostic to a CEP error.
///
/// A "target already exists" diagnostic (the no-replace precondition failed
/// at install time) maps to `ReplanRequired`: the Agent must observe current
/// Local state and plan again (ADR 0003 decision 7). Other install failures
/// remain permanent execution failures.
fn create_diag_to_cep(diag: crate::edit::types::PatchDiagnostic) -> CepError {
    if diag.message.contains("already exists") {
        return CepError::new(
            CepErrorCode::ReplanRequired,
            format!(
                "mutate.create target '{}' already exists at install time; observe.workspace and replan",
                diag.path.as_deref().unwrap_or("")
            ),
            CepErrorData::Guard(lancet_protocol::error::GuardErrorData {
                method: Create::NAME.into(),
                reason: "ReplanRequired: create target exists at install".into(),
            }),
            Retriability::RequiresReplan,
        );
    }
    patch_diag_to_cep(diag)
}

/// Map a create preflight diagnostic to a CEP error.
///
/// A "target already exists" preflight diagnostic is a stale create
/// precondition (ADR 0003 decision 7) and maps to `ReplanRequired`. Other
/// preflight failures (path validation, guard L1) remain permanent validation
/// errors.
fn create_preflight_diag_to_cep(diag: crate::edit::types::PatchDiagnostic, path: &str) -> CepError {
    if diag.message.contains("already exists") {
        return CepError::new(
            CepErrorCode::ReplanRequired,
            format!("mutate.create target '{path}' already exists; observe.workspace and replan"),
            CepErrorData::Guard(lancet_protocol::error::GuardErrorData {
                method: Create::NAME.into(),
                reason: "ReplanRequired: create target exists".into(),
            }),
            Retriability::RequiresReplan,
        );
    }
    CepError::new(
        CepErrorCode::ExecutionFailed,
        diag.message.clone(),
        CepErrorData::Validation(lancet_protocol::error::ValidationErrorData {
            field: "path".into(),
            reason: diag.message,
        }),
        Retriability::Permanent,
    )
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

fn mutation_path_effect(path: &str) -> ObservedEffect {
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

/// A successfully compiled edit patch. `post_install_revision` is the
/// content hash of the compiled `after` content — the Local Revision a
/// successful apply installs (the hashline apply re-verifies the old hash
/// before writing, so a success means exactly this content was installed).
///
/// `before`/`after` are the exact pre- and post-install content, carried so
/// the private mutation history (#63) records the real before/after state
/// without re-reading the Local Workspace.
struct CompiledEditPatch {
    patch: String,
    post_install_revision: String,
    before: String,
    after: String,
}

/// One successfully installed mutation, ready to append to the private
/// history.
struct MutationHistoryInput<'a> {
    method: &'a str,
    path: &'a str,
    /// Pre-mutation bytes; `None` when the mutation created the file.
    before: Option<&'a [u8]>,
    after: &'a [u8],
    content_hash: &'a str,
}

/// Append an installed mutation to the private workspace history and return
/// the audit evidence for it.
///
/// The install already succeeded, so a history failure must never be reported
/// as a failed mutation and must never trigger a compensating write: it is
/// recorded as evidence and the call still succeeds. A failure is also logged,
/// because an operator who never queries the audit database would otherwise
/// have no signal that a mutation is missing from the history a Rewind
/// depends on.
async fn record_mutation_history(
    ctx: &HandlerContext,
    auth: &MutationAuth,
    input: MutationHistoryInput<'_>,
) -> serde_json::Value {
    let record = lancet_runtime::MutationRecord {
        method: input.method.to_string(),
        turn_id: ctx.turn_id.clone(),
        call_id: ctx.call_id,
        path: input.path.to_string(),
        before: input.before.map(<[u8]>::to_vec),
        after: input.after.to_vec(),
        content_hash: input.content_hash.to_string(),
        recorded_at: unix_seconds_now(),
    };
    match auth.workspace.record_mutation(record).await {
        Ok(Some(recorded)) => json!({
            "recorded": true,
            "commit_id": recorded.commit_id,
            "parent_commit_id": recorded.parent_commit_id,
            "baseline_commit_id": recorded.baseline_commit_id,
        }),
        Ok(None) => json!({ "recorded": false, "reason": "history_disabled" }),
        Err(err) => {
            tracing::error!(
                method = input.method,
                path = input.path,
                turn_id = %ctx.turn_id,
                call_id = ctx.call_id,
                error = %err,
                "installed mutation was not recorded in the private workspace history"
            );
            json!({ "recorded": false, "reason": "history_error", "error": err.to_string() })
        }
    }
}

fn unix_seconds_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or_default()
}

async fn compile_edit_patch(
    auth: &MutationAuth,
    params: &EditParams,
) -> Result<CompiledEditPatch, EditCompileFailure> {
    validate_edit_params(params).map_err(|error| EditCompileFailure {
        error,
        local_revision: None,
    })?;

    let storage = auth.edit_storage();
    let before = read_edit_target_for_compile(&storage, params)
        .await
        .map_err(|error| EditCompileFailure {
            error,
            local_revision: None,
        })?;

    // Mutation Precondition: the expected hash must match the current Local
    // Revision of the target. On drift the failure carries the observed
    // Local Revision so replan evidence records the real determination.
    let current_hash = content_hash(before.as_bytes());
    if current_hash != params.expected_hash {
        return Err(EditCompileFailure {
            error: CepError::new(
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
            ),
            local_revision: Some(current_hash),
        });
    }

    let (fragment, post_install_revision, after) = compile_edit_operation_fragment(params, &before)
        .map_err(|error| EditCompileFailure {
            error,
            local_revision: None,
        })?;
    Ok(CompiledEditPatch {
        patch: format!("*** Begin Patch\n{fragment}*** End Patch\n"),
        post_install_revision,
        before,
        after,
    })
}

fn validate_edit_params(params: &EditParams) -> Result<(), CepError> {
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
    Ok(())
}

async fn read_edit_target_for_compile(
    storage: &EditStorage,
    params: &EditParams,
) -> Result<String, CepError> {
    storage
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
        })
}

fn compile_edit_operation_fragment(
    params: &EditParams,
    before: &str,
) -> Result<(String, String, String), CepError> {
    let after = apply_semantic_edit_operations(before, &params.edits)?;
    let post_install_revision = content_hash(after.as_bytes());
    let fragment = compile_update_fragment(&params.path, &params.expected_hash, before, &after)?;
    Ok((fragment, post_install_revision, after))
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
    use crate::server::registry::{Cancellation, EventSink};
    use lancet_authority::policy::CepTurnContext;
    use lancet_authority::policy::default::WorkspacePolicy;

    use std::sync::Arc;
    use tempfile::TempDir;
    use tokio::sync::{mpsc, watch};

    fn test_ctx(root: &str) -> (HandlerContext, mpsc::Receiver<lancet_protocol::ToolEvent>) {
        ctx_with_denied_patterns(root, &[])
    }

    fn local_workspace(root: &str) -> Arc<LocalWorkspace> {
        Arc::new(LocalWorkspace::new(std::path::Path::new(root)))
    }

    fn ctx_with_denied_patterns(
        root: &str,
        denied_patterns: &[&str],
    ) -> (HandlerContext, mpsc::Receiver<lancet_protocol::ToolEvent>) {
        let (event_tx, event_rx) = mpsc::channel(32);
        let (_cancel_tx, cancel_rx) = watch::channel(false);
        let mut policy_cfg =
            lancet_authority::policy::default::PolicyConfig::defaults_for_project_root(
                std::path::PathBuf::from(root),
            );
        policy_cfg.workspace = WorkspacePolicy {
            denied_write_patterns: denied_patterns.iter().map(|s| (*s).into()).collect(),
            ..Default::default()
        };
        let cep_context = Arc::new(CepTurnContext::from_policy(
            lancet_authority::policy::CepPolicyConfig::defaults_for_project_root(root),
        ));
        let default_hook = Arc::new(lancet_authority::policy::default::DefaultPolicyHook::new(
            policy_cfg,
        ));
        (
            HandlerContext {
                call_id: 1,
                events: EventSink::new(event_tx, 1),
                cancellation: Cancellation::new(cancel_rx),
                cep_context,
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
                workspace: local_workspace(root),
            },
            event_rx,
        )
    }

    fn mutation_registry() -> MethodRegistry {
        let mut registry = MethodRegistry::new();
        register_mutation_methods(&mut registry).unwrap();
        registry
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn create_through_symlinked_parent_fails_path_guard() {
        let dir = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        std::os::unix::fs::symlink(outside.path(), dir.path().join("linkdir")).unwrap();
        let root = dir.path().to_str().unwrap();
        let registry = mutation_registry();
        let (ctx, _events) = test_ctx(root);
        let durable = ctx.durable_metadata.clone();

        let params = CreateParams {
            path: "linkdir/file.txt".into(),
            content: "x".into(),
            create_parents: false,
        };
        let err = registry
            .dispatch(Create::NAME, serde_json::to_value(&params).unwrap(), ctx)
            .await
            .unwrap_err();
        // The concrete Local Workspace rejects the symlinked parent during
        // secure resolution; the denial is recorded with the path-guard stage.
        assert_eq!(err.code, CepErrorCode::ExecutionFailed);
        let metadata: serde_json::Value =
            serde_json::from_str(&durable.take_json().expect("durable metadata")).unwrap();
        assert_eq!(metadata["rejection_stage"], "path_guard");
        assert_eq!(metadata["denial_source"], "local_workspace");
    }

    #[tokio::test]
    async fn create_in_scope_succeeds() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().to_str().unwrap();
        let registry = mutation_registry();
        let (ctx, _events) = test_ctx(root);

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
        // The install lands on the real Local Workspace.
        let on_disk = std::fs::read(dir.path().join("test.txt")).unwrap();
        assert_eq!(on_disk, b"hello world");
    }

    #[tokio::test]
    async fn create_install_injected_failure_leaves_no_target() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().to_str().unwrap();
        let registry = mutation_registry();
        let (ctx, _events) = test_ctx(root);
        // Prime the narrow one-shot install failure on the workspace the
        // handler will use, proving the install failure boundary leaves the
        // target absent.
        ctx.workspace.fail_next_install();

        let params = CreateParams {
            path: "fragile.txt".into(),
            content: "partial".into(),
            create_parents: false,
        };
        let err = registry
            .dispatch(Create::NAME, serde_json::to_value(&params).unwrap(), ctx)
            .await
            .unwrap_err();
        assert_eq!(err.code, CepErrorCode::ExecutionFailed);
        assert!(
            !dir.path().join("fragile.txt").exists(),
            "injected install failure must leave no target on disk"
        );
    }

    #[tokio::test]
    async fn create_out_of_scope_returns_guard_denied() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().to_str().unwrap();
        let registry = mutation_registry();
        let (ctx, _events) = ctx_with_denied_patterns(root, &["README.md"]);

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
        assert!(!dir.path().join("README.md").exists());
    }

    #[tokio::test]
    async fn create_existing_target_returns_replan_required() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("exists.txt"), "first\n").unwrap();
        let root = dir.path().to_str().unwrap();
        let registry = mutation_registry();
        let (ctx, _events) = test_ctx(root);

        let params = CreateParams {
            path: "exists.txt".into(),
            content: "second".into(),
            create_parents: false,
        };
        let err = registry
            .dispatch(Create::NAME, serde_json::to_value(&params).unwrap(), ctx)
            .await
            .unwrap_err();
        assert_eq!(err.code, CepErrorCode::ReplanRequired);
        let on_disk = std::fs::read(dir.path().join("exists.txt")).unwrap();
        assert_eq!(on_disk, b"first\n");
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
    use crate::server::registry::{Cancellation, EventSink};
    use lancet_authority::policy::CepTurnContext;
    use lancet_authority::policy::default::WorkspacePolicy;

    use std::sync::Arc;
    use tempfile::TempDir;
    use tokio::sync::{mpsc, watch};

    fn mutation_registry() -> MethodRegistry {
        let mut registry = MethodRegistry::new();
        register_mutation_methods(&mut registry).unwrap();
        registry
    }

    fn edit_ctx(root: &str) -> HandlerContext {
        let (event_tx, _rx) = mpsc::channel(8);
        let (_cancel_tx, cancel_rx) = watch::channel(false);
        let mut policy_cfg =
            lancet_authority::policy::default::PolicyConfig::defaults_for_project_root(
                std::path::PathBuf::from(root),
            );
        policy_cfg.workspace = WorkspacePolicy {
            denied_write_patterns: Vec::new(),
            ..Default::default()
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
            workspace: Arc::new(LocalWorkspace::new(std::path::Path::new(root))),
        }
    }

    #[tokio::test]
    async fn edit_semantic_replace_computes_hashline_in_runtime() {
        let dir = TempDir::new().unwrap();
        let before = "alpha\nbeta\n";
        std::fs::write(dir.path().join("target.txt"), before).unwrap();
        let root = dir.path().to_str().unwrap();
        let registry = mutation_registry();
        let ctx = edit_ctx(root);

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
        let updated = std::fs::read_to_string(dir.path().join("target.txt")).unwrap();
        assert_eq!(updated, "alpha\ngamma\n");
    }

    #[tokio::test]
    async fn edit_success_records_post_install_local_revision() {
        let dir = TempDir::new().unwrap();
        let before = "one\ntwo\n";
        std::fs::write(dir.path().join("f.txt"), before).unwrap();
        let root = dir.path().to_str().unwrap();
        let registry = mutation_registry();
        let ctx = edit_ctx(root);
        let durable = ctx.durable_metadata.clone();

        let params = EditParams {
            path: "f.txt".into(),
            expected_hash: content_hash(before.as_bytes()),
            edits: vec![EditOperation::ReplaceText {
                old: "two".into(),
                new: "TWO".into(),
                occurrence: None,
            }],
        };
        registry
            .dispatch(Edit::NAME, serde_json::to_value(&params).unwrap(), ctx)
            .await
            .unwrap();

        let metadata: serde_json::Value =
            serde_json::from_str(&durable.take_json().expect("durable metadata")).unwrap();
        let after = "one\nTWO\n";
        assert_eq!(
            metadata["content_hash"],
            serde_json::json!(content_hash(after.as_bytes())),
            "post-install content hash is the new Local Revision"
        );
        assert_eq!(metadata["guarded_path"], "f.txt");
        assert_eq!(metadata["compiled_patch_format"], "hashline_patch");
    }

    #[tokio::test]
    async fn edit_ambiguous_text_requires_occurrence() {
        let dir = TempDir::new().unwrap();
        let before = "same\nsame\n";
        std::fs::write(dir.path().join("target.txt"), before).unwrap();
        let root = dir.path().to_str().unwrap();
        let registry = mutation_registry();
        let ctx = edit_ctx(root);

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
        let on_disk = std::fs::read_to_string(dir.path().join("target.txt")).unwrap();
        assert_eq!(on_disk, before);
    }

    #[tokio::test]
    async fn edit_stale_expected_hash_records_preflight_replan_metadata() {
        let dir = TempDir::new().unwrap();
        let before = "alpha\nbeta\n";
        std::fs::write(dir.path().join("target.txt"), before).unwrap();
        let root = dir.path().to_str().unwrap();
        let registry = mutation_registry();
        let ctx = edit_ctx(root);
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
        // The replan evidence carries the Local Revision observed at the
        // Mutation Precondition check (the target's current content hash).
        assert_eq!(
            metadata["cep_preflight"]["local_revision"],
            serde_json::json!(content_hash(before.as_bytes()))
        );
        assert_eq!(
            metadata["cep_preflight"]["replan_reason"],
            "expected_hash_stale"
        );
        assert_eq!(
            metadata["cep_preflight"]["changed_paths"],
            serde_json::json!(["target.txt"])
        );
        assert!(metadata["cep_preflight"].get("backend_invoked").is_none());
        let on_disk = std::fs::read_to_string(dir.path().join("target.txt")).unwrap();
        assert_eq!(
            on_disk, before,
            "stale-hash edit must not modify the target"
        );
    }
}
