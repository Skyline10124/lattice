// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Native CEP workspace mutation handlers.
//!
//! Implements workspace mutation methods with Guard L1 scope
//! enforcement and verify flow.
//! All methods require an active `ExecutionEnvelope`; multi-file mutations
//! perform all-or-nothing scope preflight before applying changes.

use crate::edit::apply::{PatchApplyLimits, apply_parsed_patch};
use crate::edit::content_hash;
use crate::edit::hash::line_hash;
use crate::edit::parser::parse_patch_document;
use crate::edit::storage::EditStorage;
use crate::enforcement::guard_l1::evaluate_and_enforce_policy;
use crate::enforcement::preflight::{MutationPreflight, mutation_preflight_metadata};
use crate::enforcement::verify::{
    VerifyCommandSpec, run_verify_with_backend, verify_failed_error,
};
use crate::execution_backend::{ExecutionRouting, PathIntent, WorkspacePath};
use crate::server::registry::{HandlerContext, MethodRegistry};
use lancet_audit::{RejectionRecord, RejectionStage};
use lancet_authority::effect::ledger::{
    EffectSource, ObservedEffect, ObservedLineChange, ObservedLineChangeKind, ObservedScope,
};
use lancet_authority::effect::region::AuthorizedEffectRegion;
use lancet_authority::guard::validate_direct_path_facts;
use lancet_authority::guard::{DirectPathFacts, DirectPathKind};
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
use std::path::{Component, Path, PathBuf};

/// Register native workspace mutation methods.
pub fn register_mutation_methods(registry: &mut MethodRegistry) -> Result<(), CepError> {
    registry.register_method::<Create, _, _>(handle_create)?;
    registry.register_method::<Edit, _, _>(handle_edit)?;
    registry.register_method::<Batch, _, _>(handle_batch)?;
    Ok(())
}

struct MutationAuthorization {
    project_root: Option<String>,
    direct: Option<DirectMutationAuth>,
}

struct DirectMutationAuth {
    execution: ExecutionRouting,
    authorized_region: AuthorizedEffectRegion,
}

impl MutationAuthorization {
    fn project_root(&self) -> &str {
        self.project_root
            .as_deref()
            .expect("project_root required in Envelope mode")
    }

    fn edit_storage(&self) -> EditStorage {
        if let Some(direct) = &self.direct {
            EditStorage::direct(direct.execution.clone(), direct.authorized_region.clone())
        } else {
            EditStorage::envelope_host(self.project_root())
        }
    }

    fn is_direct(&self) -> bool {
        self.direct.is_some()
    }

    fn direct(&self) -> &DirectMutationAuth {
        self.direct.as_ref().expect("direct auth required")
    }
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

    let create = if auth.is_direct() {
        preflight_direct_create(&auth, &params, method).await?
    } else {
        preflight_create_file(auth.project_root(), &params, method)?
    };

    if auth.is_direct() {
        write_direct_create(&ctx, &auth, &create).await?;
    } else {
        write_preflighted_create(auth.project_root(), &create).await?;
    }

    if auth.is_direct() {
        record_direct_created_dir_effects(&ctx, &create);
    } else {
        record_created_dir_effects(&ctx, auth.project_root(), &create);
    }
    ctx.record_effect(direct_mutation_path_effect(
        &auth,
        &create.path,
        &create.full_path,
    ));

    run_mutation_verify_if_configured(&ctx, method, &auth).await?;

    let written_hash = content_hash(params.content.as_bytes());
    ctx.record_durable_metadata(json!({
        "method": method,
        "envelope_id": null,
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
    run_mutation_verify_if_configured(&ctx, Edit::NAME, &auth).await?;

    ctx.record_durable_metadata(json!({
        "method": Edit::NAME,
        "envelope_id": null,
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
                create_inputs.push(if auth.is_direct() {
                    preflight_direct_create(&auth, &params, Batch::NAME).await?
                } else {
                    preflight_create_file(auth.project_root(), &params, Batch::NAME)?
                });
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
        if auth.is_direct() {
            write_direct_create(&ctx, &auth, create).await?;
            record_direct_created_dir_effects(&ctx, create);
        } else {
            write_preflighted_create(auth.project_root(), create).await?;
            for dir in &create.created_dirs {
                if seen_created_dirs.insert(dir.clone()) {
                    record_created_dir_effect(&ctx, auth.project_root(), dir);
                    created_dirs.push(dir.clone());
                }
            }
        }
        if auth.is_direct() {
            for dir in &create.created_dirs {
                if seen_created_dirs.insert(dir.clone()) {
                    created_dirs.push(dir.clone());
                }
            }
        }
        ctx.record_effect(direct_mutation_path_effect(
            &auth,
            &create.path,
            &create.full_path,
        ));
        created_files.push(create.path.clone());
    }

    run_mutation_verify_if_configured(&ctx, Batch::NAME, &auth).await?;

    let mut changed_files = edited_files.clone();
    changed_files.extend(created_files.iter().cloned());

    ctx.record_durable_metadata(json!({
        "method": Batch::NAME,
        "envelope_id": null,
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
) -> Result<MutationAuthorization, CepError> {
    let _evaluation = evaluate_and_enforce_policy(ctx, method, params).await?;

    authorize_direct_mutation(ctx, method, changed_paths).await
}

async fn authorize_direct_mutation(
    ctx: &HandlerContext,
    method: &str,
    changed_paths: &[String],
) -> Result<MutationAuthorization, CepError> {
    let execution = ctx.execution.clone().ok_or_else(|| {
        CepError::new(
            CepErrorCode::ExecutionFailed,
            "Direct mutation requires an Upper execution backend",
            CepErrorData::Execution(lancet_protocol::error::ExecutionErrorData {
                command: method.into(),
                exit_code: None,
                stderr: Some("missing execution backend".into()),
            }),
            Retriability::Permanent,
        )
    })?;
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
                direct_mutation_backend_error(method)(err)
            })?;
        let guard_path = if ws_path.is_project_root() {
            ""
        } else {
            ws_path.as_str()
        };
        let direct_facts = DirectPathFacts {
            normalized_path: facts.normalized_path.clone(),
            exists: facts.exists,
            kind: facts.kind.map(direct_path_kind),
            parent_exists: facts.parent_exists,
            parent_kind: facts.parent_kind.map(direct_path_kind),
            canonical_under_workspace: facts.canonical_under_workspace,
        };
        if let Err(err) = validate_direct_path_facts(guard_path, &direct_facts, &authorized_region)
        {
            ctx.record_rejection(RejectionRecord {
                rejection_stage: RejectionStage::AdapterPreflight,
                denial_source: "guard_l1".into(),
            });
            return Err(err);
        }
    }

    Ok(MutationAuthorization {
        project_root: None,
        direct: Some(DirectMutationAuth {
            execution,
            authorized_region,
        }),
    })
}

fn direct_path_kind(kind: crate::execution_backend::PathKind) -> DirectPathKind {
    match kind {
        crate::execution_backend::PathKind::File => DirectPathKind::File,
        crate::execution_backend::PathKind::Directory => DirectPathKind::Directory,
        crate::execution_backend::PathKind::Symlink => DirectPathKind::Symlink,
    }
}

fn direct_mutation_backend_error(
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
    verify_configured: bool,
    replan_reason: Option<&str>,
    backend_invoked: bool,
) {
    ctx.record_durable_metadata(mutation_preflight_metadata(MutationPreflight {
        method,
        changed_paths,
        write_patterns,
        out_of_scope_paths,
        expected_hash_status,
        verify_configured,
        replan_reason,
        backend_invoked,
    }));
}

fn record_mutation_replan_metadata(
    ctx: &HandlerContext,
    method: &str,
    _auth: &MutationAuthorization,
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
        ctx.direct_verify.is_some(),
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

async fn run_mutation_verify_if_configured(
    ctx: &HandlerContext,
    method: &str,
    _auth: &MutationAuthorization,
) -> Result<(), CepError> {
    let Some(verify_config) = ctx.direct_verify.as_ref() else {
        return Ok(());
    };
    let policy = &ctx.cep_context.policy.command;
    let mut cancellation = ctx.cancellation.clone();
    ctx.mark_backend_invoked();
    let execution = ctx.execution.as_ref().ok_or_else(|| {
        CepError::new(
            CepErrorCode::ExecutionFailed,
            "verify requires an Upper execution backend",
            CepErrorData::Execution(lancet_protocol::error::ExecutionErrorData {
                command: method.into(),
                exit_code: None,
                stderr: Some("missing execution backend".into()),
            }),
            Retriability::Permanent,
        )
    })?;
    let authorized_region = ctx
        .authorized_region
        .get()
        .unwrap_or_else(AuthorizedEffectRegion::empty);
    match run_verify_with_backend(
        verify_config,
        execution,
        policy,
        &mut cancellation,
        &ctx.routing_invocation,
        &authorized_region,
    )
    .await
    {
        Ok(outcome) if !outcome.success => {
            ctx.record_rejection(RejectionRecord {
                rejection_stage: RejectionStage::VerificationFailure,
                denial_source: "direct_verify".into(),
            });
            emit_verify_failed(ctx, method, "", outcome.exit_code).await;
            let spec = direct_verify_spec(verify_config);
            Err(verify_failed_error(&spec, &outcome))
        }
        Ok(_) => {
            ctx.record_effect(ObservedEffect {
                kind: ImpactKind::CommandExecution,
                scope: ObservedScope::Root(String::new()),
                required_capability: RequiredCapability::RunCommand,
                source: EffectSource::Verify,
            });
            Ok(())
        }
        Err(err) => {
            emit_verify_failed(ctx, method, "", -1).await;
            Err(err)
        }
    }
}

fn preflight_create_file(
    project_root: &str,
    params: &CreateParams,
    method: &str,
) -> Result<PreflightCreate, CepError> {
    let (full_path, _parent) = resolve_create_target(project_root, &params.path)?;
    let created_dirs = missing_parent_dirs(project_root, &params.path)?;
    if !created_dirs.is_empty() && !params.create_parents {
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

    Ok(PreflightCreate {
        path: params.path.clone(),
        content: params.content.clone(),
        full_path,
        bytes_written: params.content.len() as u64,
        created_dirs,
    })
}

async fn write_preflighted_create(
    project_root: &str,
    create: &PreflightCreate,
) -> Result<(), CepError> {
    if !create.created_dirs.is_empty() {
        create_parent_dirs_secure(project_root, &create.path).await?;
    }
    create_file_secure(project_root, &create.path, &create.content).await
}

async fn preflight_direct_create(
    auth: &MutationAuthorization,
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
        missing_parent_dirs_direct(auth, &params.path).await?
    } else {
        let parent = Path::new(&params.path)
            .parent()
            .and_then(|p| p.to_str())
            .filter(|p| !p.is_empty());
        if let Some(parent_path) = parent
            && !parent_exists_direct(auth, parent_path).await?
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

async fn parent_exists_direct(auth: &MutationAuthorization, path: &str) -> Result<bool, CepError> {
    let ws_path =
        WorkspacePath::parse(path).map_err(|err| validation_error("path", err.to_string()))?;
    let direct = auth.direct();
    let facts = direct
        .execution
        .backend
        .resolve_path(&ws_path, PathIntent::ListDirectory)
        .await
        .map_err(direct_mutation_backend_error("mutate.create"))?;
    Ok(facts.exists)
}

/// Walk ancestor directories of `relative_path` via the Upper backend, returning
/// the list of missing ancestors.  All filesystem probing goes through
/// `ExecutionBackend::resolve_path` — never the host filesystem (D-02).
async fn missing_parent_dirs_direct(
    auth: &MutationAuthorization,
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
    let direct = auth.direct();
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
        let facts = direct
            .execution
            .backend
            .resolve_path(&ancestor, PathIntent::ListDirectory)
            .await
            .map_err(direct_mutation_backend_error("mutate.create"))?;

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

async fn write_direct_create(
    ctx: &HandlerContext,
    auth: &MutationAuthorization,
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

fn direct_mutation_path_effect(
    auth: &MutationAuthorization,
    path: &str,
    full_path: &Path,
) -> ObservedEffect {
    let scope = if auth.is_direct() {
        ObservedScope::Path(PathBuf::from(path))
    } else {
        ObservedScope::Path(full_path.to_path_buf())
    };
    ObservedEffect {
        kind: ImpactKind::WorkspaceMutation,
        scope,
        required_capability: RequiredCapability::WritePath,
        source: EffectSource::HandlerBackend,
    }
}

fn record_direct_created_dir_effects(ctx: &HandlerContext, create: &PreflightCreate) {
    for dir in &create.created_dirs {
        ctx.record_effect(ObservedEffect {
            kind: ImpactKind::WorkspaceMutation,
            scope: ObservedScope::Path(PathBuf::from(dir)),
            required_capability: RequiredCapability::WritePath,
            source: EffectSource::HandlerBackend,
        });
    }
}

fn direct_verify_spec(config: &crate::execution_backend::DirectVerifyConfig) -> VerifyCommandSpec {
    VerifyCommandSpec {
        argv: config.argv.clone(),
        cwd: Some(config.cwd.as_str().to_string()),
        timeout_ms: config.timeout_ms,
        expected_exit_code: Some(config.expected_exit_code),
    }
}

fn record_created_dir_effects(ctx: &HandlerContext, project_root: &str, create: &PreflightCreate) {
    for dir in &create.created_dirs {
        record_created_dir_effect(ctx, project_root, dir);
    }
}

fn record_created_dir_effect(ctx: &HandlerContext, project_root: &str, dir: &str) {
    ctx.record_effect(ObservedEffect {
        kind: ImpactKind::WorkspaceMutation,
        scope: ObservedScope::Path(Path::new(project_root).join(dir)),
        required_capability: RequiredCapability::WritePath,
        source: EffectSource::HandlerBackend,
    });
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

async fn compile_edit_patch(
    auth: &MutationAuthorization,
    params: &EditParams,
) -> Result<String, CepError> {
    let fragment = if auth.is_direct() {
        compile_edit_operation_fragment_direct(&auth.edit_storage(), params).await?
    } else {
        compile_edit_operation_fragment(auth.project_root(), params)?
    };
    Ok(format!("*** Begin Patch\n{fragment}*** End Patch\n"))
}

async fn compile_batch_edit_patch(
    auth: &MutationAuthorization,
    edits: &[EditParams],
) -> Result<String, CepError> {
    let mut patch = String::from("*** Begin Patch\n");
    for edit in edits {
        let fragment = if auth.is_direct() {
            compile_edit_operation_fragment_direct(&auth.edit_storage(), edit).await?
        } else {
            compile_edit_operation_fragment(auth.project_root(), edit)?
        };
        patch.push_str(&fragment);
    }
    patch.push_str("*** End Patch\n");
    Ok(patch)
}

async fn compile_edit_operation_fragment_direct(
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
        let abs_path = if auth.is_direct() {
            PathBuf::from(changed_path)
        } else if Path::new(changed_path).is_relative() {
            PathBuf::from(auth.project_root()).join(changed_path)
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

fn resolve_create_target(
    project_root: &str,
    relative_path: &str,
) -> Result<(PathBuf, PathBuf), CepError> {
    validate_relative_workspace_path(relative_path)?;

    let full_path = Path::new(project_root).join(relative_path);
    let parent = full_path.parent().map(Path::to_path_buf).ok_or_else(|| {
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

    Ok((full_path, parent))
}

fn validate_relative_workspace_path(relative_path: &str) -> Result<(), CepError> {
    if relative_path.is_empty() {
        return Err(validation_error("path", "path must not be empty"));
    }

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

    let path = Path::new(relative_path);
    if path.is_absolute() {
        return Err(CepError::new(
            CepErrorCode::PathContainment,
            format!("absolute path denied: '{relative_path}'"),
            CepErrorData::Validation(lancet_protocol::error::ValidationErrorData {
                field: "path".into(),
                reason: "absolute path is outside workspace scope".into(),
            }),
            Retriability::Permanent,
        ));
    }

    for component in path.components() {
        match component {
            Component::Normal(_) | Component::CurDir => {}
            Component::ParentDir => {
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
            Component::RootDir | Component::Prefix(_) => {
                return Err(CepError::new(
                    CepErrorCode::PathContainment,
                    format!("path escapes workspace scope: '{relative_path}'"),
                    CepErrorData::Validation(lancet_protocol::error::ValidationErrorData {
                        field: "path".into(),
                        reason: "path escapes workspace scope".into(),
                    }),
                    Retriability::Permanent,
                ));
            }
        }
    }

    Ok(())
}

fn missing_parent_dirs(project_root: &str, relative_path: &str) -> Result<Vec<String>, CepError> {
    validate_relative_workspace_path(relative_path)?;

    let root = Path::new(project_root);
    let relative = Path::new(relative_path);
    let parent = relative.parent().unwrap_or_else(|| Path::new(""));
    let mut current = root.to_path_buf();
    let mut missing = Vec::new();

    for component in parent.components() {
        match component {
            Component::Normal(name) => current.push(name),
            Component::CurDir => continue,
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(CepError::new(
                    CepErrorCode::PathContainment,
                    format!("path escapes workspace scope: '{relative_path}'"),
                    CepErrorData::Validation(lancet_protocol::error::ValidationErrorData {
                        field: "path".into(),
                        reason: "path escapes workspace scope".into(),
                    }),
                    Retriability::Permanent,
                ));
            }
        }

        match std::fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(CepError::new(
                    CepErrorCode::PathContainment,
                    format!("parent directory '{}' is a symlink", current.display()),
                    CepErrorData::Validation(lancet_protocol::error::ValidationErrorData {
                        field: "path".into(),
                        reason: "parent directory is a symlink".into(),
                    }),
                    Retriability::Permanent,
                ));
            }
            Ok(metadata) if !metadata.is_dir() => {
                return Err(CepError::new(
                    CepErrorCode::ExecutionFailed,
                    format!("parent path '{}' is not a directory", current.display()),
                    CepErrorData::Validation(lancet_protocol::error::ValidationErrorData {
                        field: "path".into(),
                        reason: "parent path is not a directory".into(),
                    }),
                    Retriability::Permanent,
                ));
            }
            Ok(_) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                missing.push(workspace_relative_path(root, &current));
            }
            Err(err) => {
                return Err(CepError::new(
                    CepErrorCode::ExecutionFailed,
                    format!(
                        "cannot inspect parent directory '{}': {err}",
                        current.display()
                    ),
                    CepErrorData::Execution(lancet_protocol::error::ExecutionErrorData {
                        command: "create".into(),
                        exit_code: None,
                        stderr: Some(err.to_string()),
                    }),
                    Retriability::Permanent,
                ));
            }
        }
    }

    Ok(missing)
}

fn workspace_relative_path(project_root: &Path, path: &Path) -> String {
    path.strip_prefix(project_root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

fn ensure_existing_parent_chain_safe(
    project_root: &str,
    relative_path: &str,
) -> Result<(), CepError> {
    let missing = missing_parent_dirs(project_root, relative_path)?;
    if missing.is_empty() {
        Ok(())
    } else {
        Err(CepError::new(
            CepErrorCode::ExecutionFailed,
            format!(
                "parent directory for '{}' does not exist: {}",
                relative_path,
                missing.join(", ")
            ),
            CepErrorData::Execution(lancet_protocol::error::ExecutionErrorData {
                command: "create".into(),
                exit_code: None,
                stderr: Some("parent directory does not exist".into()),
            }),
            Retriability::Permanent,
        ))
    }
}

async fn create_parent_dirs_secure(
    project_root: &str,
    relative_path: &str,
) -> Result<(), CepError> {
    let (_full_path, parent) = resolve_create_target(project_root, relative_path)?;

    tokio::fs::create_dir_all(&parent).await.map_err(|err| {
        CepError::new(
            CepErrorCode::ExecutionFailed,
            format!(
                "failed to create parent directories for '{}': {err}",
                relative_path
            ),
            CepErrorData::Execution(lancet_protocol::error::ExecutionErrorData {
                command: "create_parents".into(),
                exit_code: None,
                stderr: Some(err.to_string()),
            }),
            Retriability::Transient,
        )
    })?;

    ensure_existing_parent_chain_safe(project_root, relative_path)
}

/// Create a file securely using native containment checks.
async fn create_file_secure(
    project_root: &str,
    relative_path: &str,
    content: &str,
) -> Result<(), CepError> {
    let (full_path, _parent) = resolve_create_target(project_root, relative_path)?;
    ensure_existing_parent_chain_safe(project_root, relative_path)?;

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
    use crate::server::registry::{Cancellation, EventSink};
    use lancet_protocol::effect_types::ImpactKind;

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
                command_isolation:
                    crate::enforcement::isolation::CommandIsolationRuntimeConfig::default(),
                execution: None,
                direct_verify: None,
                routing_invocation: crate::execution_backend::RoutingInvocationState::default(),
            },
            event_rx,
        )
    }

    fn test_ctx_with_store(
        root: &str,
        store: Arc<ActiveEnvelopeStore>,
    ) -> (HandlerContext, mpsc::Receiver<lancet_protocol::ToolEvent>) {
        test_ctx_with_store_and_mode(root, store, lancet_protocol::policy::CepMode::Off)
    }

    fn test_ctx_with_store_and_mode(
        root: &str,
        store: Arc<ActiveEnvelopeStore>,
        cep_mode: lancet_protocol::policy::CepMode,
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
                cep_mode,
                command_isolation:
                    crate::enforcement::isolation::CommandIsolationRuntimeConfig::default(),
                execution: None,
                direct_verify: None,
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

    fn mutation_and_envelope_registry() -> MethodRegistry {
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
            create_parents: false,
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
            create_parents: false,
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
        envelope.verify = Some(lancet_protocol::policy::VerifyCommand {
            command: "echo ok".into(),
            expected_exit_code: Some(1),
        });
        store.declare(envelope, 0, "test-turn").await.unwrap();

        let (ctx, _events) = test_ctx_with_store(root, store);
        let params = CreateParams {
            path: "test.txt".into(),
            content: "hello".into(),
            create_parents: false,
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
        let durable = ctx.durable_metadata.clone();

        let params = CreateParams {
            path: "README.md".into(),
            content: "out of scope".into(),
            create_parents: false,
        };
        let err = registry
            .dispatch(Create::NAME, serde_json::to_value(&params).unwrap(), ctx)
            .await
            .unwrap_err();
        assert_eq!(err.code, CepErrorCode::ReplanRequired);
        let metadata: serde_json::Value =
            serde_json::from_str(&durable.take_json().expect("durable metadata")).unwrap();
        assert_eq!(
            metadata["cep_preflight"]["replan_reason"],
            "out_of_scope_paths"
        );
        assert_eq!(
            metadata["cep_preflight"]["out_of_scope_paths"],
            serde_json::json!(["README.md"])
        );
        assert_eq!(metadata["backend_invoked"], false);
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
            create_parents: false,
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
            create_parents: false,
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
    async fn test_replan_flow_mutation_outside_scope_then_explicit_envelope_resumes() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().to_str().unwrap();
        std::fs::create_dir_all(dir.path().join("src/other")).unwrap();
        let registry = mutation_and_envelope_registry();
        let store = Arc::new(ActiveEnvelopeStore::new());

        let envelope = envelope_with_write_path("src/allowed/**");
        store.declare(envelope, 0, "test-turn").await.unwrap();

        let create_params = CreateParams {
            path: "src/other/file.rs".into(),
            content: "fn main() {}\n".into(),
            create_parents: false,
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
        let declare_params = DeclareParams {
            envelope: envelope_with_write_path("src/other/file.rs"),
            supersede: true,
            reason: Some("expand scope for out-of-band file".into()),
        };
        registry
            .dispatch(
                Envelope::NAME,
                serde_json::to_value(EnvelopeParams::Declare(declare_params)).unwrap(),
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
            create_parents: false,
        };
        let err = registry
            .dispatch(Create::NAME, serde_json::to_value(&params).unwrap(), ctx)
            .await
            .unwrap_err();
        assert_eq!(err.code, CepErrorCode::ExecutionFailed);
        assert!(err.message.contains("parent"));
        assert!(err.message.contains("create_parents=true"));
    }

    #[tokio::test]
    async fn create_with_create_parents_creates_missing_dirs() {
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
            create_parents: true,
        };
        let result = registry
            .dispatch(Create::NAME, serde_json::to_value(&params).unwrap(), ctx)
            .await
            .unwrap();
        let parsed: CreateResult = serde_json::from_value(result).unwrap();

        assert_eq!(parsed.path, "nonexistent/dir/file.txt");
        assert_eq!(
            parsed.created_dirs,
            vec!["nonexistent".to_string(), "nonexistent/dir".to_string()]
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("nonexistent/dir/file.txt")).unwrap(),
            "hello"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn create_rejects_symlink_parent() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().to_str().unwrap();
        std::fs::create_dir_all(dir.path().join("real")).unwrap();
        std::os::unix::fs::symlink(dir.path().join("real"), dir.path().join("linked")).unwrap();

        let registry = mutation_registry();
        let store = Arc::new(ActiveEnvelopeStore::new());
        store
            .declare(envelope_with_write_path("."), 0, "test-turn")
            .await
            .unwrap();

        let (ctx, _events) = test_ctx_with_store(root, store);
        let params = CreateParams {
            path: "linked/file.txt".into(),
            content: "hello".into(),
            create_parents: false,
        };
        let err = registry
            .dispatch(Create::NAME, serde_json::to_value(&params).unwrap(), ctx)
            .await
            .unwrap_err();

        assert_eq!(err.code, CepErrorCode::PathContainment);
        assert!(err.message.contains("symlink"));
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
    use lancet_protocol::effect_types::ImpactKind;

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
                command_isolation:
                    crate::enforcement::isolation::CommandIsolationRuntimeConfig::default(),
                execution: None,
                direct_verify: None,
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
    async fn edit_stale_expected_hash_records_preflight_replan_metadata() {
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
        assert_eq!(
            std::fs::read_to_string(dir.path().join("Cargo.toml")).unwrap(),
            "workspace\nnew\n"
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("new.txt")).unwrap(),
            "created\n"
        );
    }

    #[tokio::test]
    async fn batch_create_file_with_create_parents_creates_missing_dirs() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().to_str().unwrap();
        let registry = mutation_registry();
        let store = Arc::new(ActiveEnvelopeStore::new());
        store
            .declare(envelope_with_write_path("."), 0, "test-turn")
            .await
            .unwrap();
        let (ctx, _events) = test_ctx_with_store(root, store);

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
        assert_eq!(
            parsed.created_dirs,
            vec![
                "crates".to_string(),
                "crates/lattice-tui".to_string(),
                "crates/lattice-tui/src".to_string()
            ]
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("crates/lattice-tui/src/lib.rs")).unwrap(),
            "pub fn run() {}\n"
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("crates/lattice-tui/src/main.rs")).unwrap(),
            "fn main() {}\n"
        );
    }

    mod phase34_direct_mutation {
        use super::*;
        use crate::execution_backend::{
            DirectoryEntry, ExecutionBackend, ExecutionBackendError, ExecutionRouting, FindRequest,
            GrepMatch, GrepRequest, PathFacts, PathIntent, UpperContext, WorkspacePath,
        };
        use lancet_authority::policy::CepTurnContext;
        use lancet_authority::policy::default::WorkspacePolicy;
        use std::collections::BTreeMap;
        use std::future::Future;
        use std::pin::Pin;
        use std::sync::{Arc, Mutex};

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
            ) -> Pin<Box<dyn Future<Output = Result<(), ExecutionBackendError>> + Send + '_>>
            {
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
            ) -> Pin<Box<dyn Future<Output = Result<(), ExecutionBackendError>> + Send + '_>>
            {
                Box::pin(async { Ok(()) })
            }

            fn list_dir(
                &self,
                _path: &WorkspacePath,
            ) -> Pin<
                Box<
                    dyn Future<Output = Result<Vec<DirectoryEntry>, ExecutionBackendError>>
                        + Send
                        + '_,
                >,
            > {
                Box::pin(async { Ok(Vec::new()) })
            }

            fn grep(
                &self,
                _request: &GrepRequest,
            ) -> Pin<
                Box<dyn Future<Output = Result<Vec<GrepMatch>, ExecutionBackendError>> + Send + '_>,
            > {
                Box::pin(async { Ok(Vec::new()) })
            }

            fn find_files(
                &self,
                _request: &FindRequest,
            ) -> Pin<
                Box<
                    dyn Future<Output = Result<Vec<WorkspacePath>, ExecutionBackendError>>
                        + Send
                        + '_,
                >,
            > {
                Box::pin(async { Ok(Vec::new()) })
            }

            fn exec(
                &self,
                _request: &crate::execution_backend::ExecRequest,
            ) -> Pin<
                Box<
                    dyn Future<
                            Output = Result<
                                crate::execution_backend::ExecOutput,
                                ExecutionBackendError,
                            >,
                        > + Send
                        + '_,
                >,
            > {
                Box::pin(async {
                    Ok(crate::execution_backend::ExecOutput {
                        exit_code: 0,
                        stdout: String::new(),
                        stderr: String::new(),
                    })
                })
            }

            fn changed_paths(
                &self,
            ) -> Pin<
                Box<
                    dyn Future<Output = Result<Vec<WorkspacePath>, ExecutionBackendError>>
                        + Send
                        + '_,
                >,
            > {
                Box::pin(async { Ok(Vec::new()) })
            }
        }

        fn direct_mut_ctx(root: &str, backend: Arc<MutSpyBackend>) -> HandlerContext {
            let (event_tx, _rx) = mpsc::channel(8);
            let (_cancel_tx, cancel_rx) = watch::channel(false);
            let mut policy_cfg =
                lancet_authority::policy::default::PolicyConfig::defaults_for_project_root(
                    PathBuf::from(root),
                );
            policy_cfg.workspace = WorkspacePolicy {
                allowed_write_paths: vec![".".into()],
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
                audit: Arc::new(crate::audit::CepAuditStore::in_memory().expect("audit")),
                cep_mode: lancet_protocol::policy::CepMode::Off,
                command_isolation:
                    crate::enforcement::isolation::CommandIsolationRuntimeConfig::default(),
                execution: Some(ExecutionRouting {
                    backend,
                    upper: UpperContext {
                        execution_backend: "mut-spy".into(),
                        upper_workspace_id: "upper-1".into(),
                    },
                }),
                direct_verify: None,
                routing_invocation: crate::execution_backend::RoutingInvocationState::default(),
            }
        }

        #[tokio::test]
        async fn phase34_direct_mutation_create_uses_backend_only() {
            let dir = TempDir::new().unwrap();
            let root = dir.path().to_str().unwrap();
            let registry = mutation_registry();
            let backend = Arc::new(MutSpyBackend::default());
            let ctx = direct_mut_ctx(root, Arc::clone(&backend));
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
}
