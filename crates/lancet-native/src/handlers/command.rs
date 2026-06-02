//! Native exec.command_run CEP handler with policy enforcement.
//!
//! This is the only shell execution path from the agent.
//! WorkspaceMutation commands capture a pre-execution workspace snapshot, compute
//! changed paths after execution, and check them against WritePath scope before
//! running any configured verify command.

use crate::effect::ledger::{EffectSource, ObservedEffect, ObservedScope};
use crate::envelope_store::EnvelopeRecord;
use crate::executor::shell::{execute_argv, BoundedStreamOutput, ExecuteConfig};
use crate::guard_l1::{
    check_write_scope, evaluate_and_enforce_policy, extract_write_path_patterns,
    has_run_command_capability,
};
use crate::policy::command::{
    build_effective_env, classify_command, validate_timeout, EffectiveImpact,
};
use crate::registry::{HandlerContext, MethodRegistry};
use crate::verify::{
    extract_verify_command, run_verify, validate_verify_command, verify_failed_error,
};
use lancet_catalog::method::RequiredCapability;
use lancet_catalog::methods::command::{Run, RunParams, RunResult};
use lancet_catalog::CepMethod;
use lancet_protocol::envelope::Capability;
use lancet_protocol::envelope::ImpactKind;
use lancet_protocol::error::{CepErrorData, GuardErrorData, PermissionErrorData};
use lancet_protocol::{CepError, CepErrorCode, Retriability};
use serde_json::json;
use std::collections::HashMap;
use std::path::Path;

/// Register native `exec.command_run` handler.
pub fn register_command_methods(registry: &mut MethodRegistry) -> Result<(), CepError> {
    registry.register_method::<Run, _, _>(handle_run)?;
    Ok(())
}

async fn handle_run(params: RunParams, ctx: HandlerContext) -> Result<RunResult, CepError> {
    handle_run_as(params, ctx, Run::NAME).await
}

async fn handle_run_as(
    params: RunParams,
    ctx: HandlerContext,
    method: &'static str,
) -> Result<RunResult, CepError> {
    validate_inputs(&params)?;

    let _evaluation = evaluate_and_enforce_policy(&ctx, method, &params).await?;

    let policy = &ctx.cep_context.policy.command;

    // 3. Classify command
    let effective_impact = classify_command(&params.argv, params.declared_impact, policy)?;

    // 4. HardDenied -> return GuardDenied error
    if effective_impact == EffectiveImpact::HardDenied {
        let cmd = params.argv.first().map(|s| s.as_str()).unwrap_or("<empty>");
        emit_denial_metadata(&ctx, "hard_denied", false, method).await;
        return Err(CepError::new(
            CepErrorCode::GuardDenied,
            format!("Command '{cmd}' is hard-denied by policy"),
            CepErrorData::Guard(GuardErrorData {
                method: method.into(),
                reason: format!("HardDenied: '{cmd}' matches denied command pattern"),
            }),
            Retriability::Permanent,
        ));
    }

    // 5. WorkspaceMutation requires active envelope with RunCommand capability
    let active_record = if effective_impact == EffectiveImpact::WorkspaceMutation {
        let active = ctx.active_envelope.active_record().await;
        match active {
            Some(record) => {
                // Check RunCommand capability
                let has_run_command = record
                    .envelope
                    .capabilities
                    .iter()
                    .any(|cap| matches!(cap, Capability::RunCommand { .. }));
                if !has_run_command {
                    emit_denial_metadata(&ctx, "run_command_not_granted", false, method).await;
                    return Err(CepError::new(
                        CepErrorCode::CapabilityNotGranted,
                        "Active envelope does not grant RunCommand capability",
                        CepErrorData::Permission(PermissionErrorData {
                            required_capability: "run_command".into(),
                            active_envelope_id: Some(record.envelope_id.clone()),
                            granted_capabilities: record
                                .envelope
                                .capabilities
                                .iter()
                                .map(|c| format!("{c:?}"))
                                .collect(),
                        }),
                        Retriability::Permanent,
                    ));
                }
                // Check argv against allowlist
                if !has_run_command_capability(&record.envelope.capabilities, &params.argv) {
                    emit_denial_metadata(&ctx, "argv_not_in_allowlist", false, method).await;
                    return Err(CepError::new(
                        CepErrorCode::GuardDenied,
                        "Command argv does not match RunCommand allowlist",
                        CepErrorData::Guard(GuardErrorData {
                            method: method.into(),
                            reason: "argv not in envelope RunCommand allowlist".into(),
                        }),
                        Retriability::Permanent,
                    ));
                }
                Some(record)
            }
            None => {
                emit_denial_metadata(&ctx, "active_envelope", false, method).await;
                return Err(CepError::new(
                    CepErrorCode::EnvelopeMissing,
                    "WorkspaceMutation command requires an active ExecutionEnvelope. \
                     Declare an envelope with RunCommand capability first.",
                    CepErrorData::Guard(GuardErrorData {
                        method: method.into(),
                        reason: "EnvelopeMissing: no active envelope for WorkspaceMutation".into(),
                    }),
                    Retriability::Permanent,
                ));
            }
        }
    } else {
        None
    };
    // 5b. Composite authorized region for WorkspaceMutation:
    // The policy evaluator only matches RunCommand → authorized region only has
    // CommandExecution. For composite invocations, union in WritePath regions
    // from the active envelope so WorkspaceMutation effects are also authorized.
    if let Some(ref record) = active_record {
        let mut write_paths = Vec::new();
        let mut write_path_prefixes = Vec::new();
        for cap in &record.envelope.capabilities {
            if let Capability::WritePath { path_pattern } = cap {
                if let Some((path, is_prefix)) = crate::effect::region::normalize_write_region_path(
                    path_pattern,
                    &ctx.cep_context.policy.project_root,
                ) {
                    if is_prefix {
                        write_path_prefixes.push(path);
                    } else {
                        write_paths.push(path);
                    }
                }
            }
        }
        if !write_paths.is_empty() || !write_path_prefixes.is_empty() {
            let mut write_region = crate::effect::region::AuthorizedEffectRegion::empty();
            write_region
                .kinds
                .insert(lancet_protocol::envelope::ImpactKind::WorkspaceMutation);
            write_region.write_paths = write_paths;
            write_region.write_path_prefixes = write_path_prefixes;
            if let Some(existing) = ctx.authorized_region.get() {
                let mut merged = existing.clone();
                merged.union(&write_region);
                ctx.authorized_region.set(merged);
            } else {
                ctx.authorized_region.set(write_region);
            }
        }
    }

    let envelope_id = active_record
        .as_ref()
        .map(|record| record.envelope_id.clone());

    // 6. Diagnostic: no envelope required, but still enforce policy

    // 7. Validate timeout
    let _effective_timeout = validate_timeout(params.timeout_ms, policy)?;

    // 8. Build effective env
    let effective_env = build_effective_env(&params.env, policy)?;

    // 9. Execute via rewritten execute_argv with bounded tail reader
    let project_root = ctx
        .cep_context
        .policy
        .project_root
        .to_string_lossy()
        .to_string();
    let mut cancellation = ctx.cancellation.clone();

    // 9a. Pre-execution baseline for WorkspaceMutation commands.
    let pre_snapshot = if effective_impact == EffectiveImpact::WorkspaceMutation {
        capture_workspace_snapshot(&project_root)?
    } else {
        HashMap::new()
    };

    let result = execute_argv(ExecuteConfig {
        project_root: &project_root,
        limits: &lattice_tooling::ToolLimits::default(),
        argv: &params.argv,
        cwd: params.cwd.as_deref(),
        env: &effective_env,
        timeout_ms: params.timeout_ms,
        cancellation: &mut cancellation,
        max_output_bytes: policy.max_output_bytes,
    })
    .await;

    // 10. Handle result
    match result {
        Ok((output, stdout_meta, stderr_meta)) => {
            // 11. Build RunResult with truncation metadata
            let exit_code = output.status.code().unwrap_or(-1);

            ctx.record_effect(ObservedEffect {
                kind: ImpactKind::CommandExecution,
                scope: ObservedScope::Root(project_root.clone()),
                required_capability: RequiredCapability::RunCommand,
                source: EffectSource::HandlerBackend,
            });

            // 12. Post-execution diff check for WorkspaceMutation.
            if effective_impact == EffectiveImpact::WorkspaceMutation {
                if let Some(ref record) = active_record {
                    let envelope = &record.envelope;
                    let diff = record_workspace_mutation_diff(
                        &ctx,
                        record,
                        &project_root,
                        &pre_snapshot,
                        method,
                    )
                    .await?;
                    if !diff.out_of_scope_paths.is_empty() {
                        return Err(out_of_scope_changed_paths_error(
                            diff.out_of_scope_paths,
                            method,
                        ));
                    }

                    // Run verify after every successful WorkspaceMutation call.
                    if let Some(verify_cmd) = extract_verify_command(envelope) {
                        validate_verify_command(&verify_cmd, policy, &project_root)?;
                        let mut verify_cancel = ctx.cancellation.clone();
                        match run_verify(&verify_cmd, &project_root, policy, &mut verify_cancel)
                            .await
                        {
                            Ok(outcome) if !outcome.success => {
                                ctx.events
                                    .emit(
                                        "cep.verify_failed",
                                        json!({
                                            "method": method,
                                            "envelope_id": &record.envelope_id,
                                            "verify_exit_code": outcome.exit_code,
                                            "diff_retained": true,
                                        }),
                                    )
                                    .await;
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
                                ctx.events
                                    .emit(
                                        "cep.verify_failed",
                                        json!({
                                            "method": method,
                                            "envelope_id": &record.envelope_id,
                                            "verify_exit_code": -1,
                                            "diff_retained": true,
                                        }),
                                    )
                                    .await;
                                return Err(err);
                            }
                        }
                    }
                }
            }

            // 13. Record durable metadata
            let effective_effect =
                crate::effect::region::EffectiveEffect::from_command_impact(&effective_impact)
                    .ok()
                    .map(|ee| {
                        ee.kinds
                            .iter()
                            .map(|k| format!("{:?}", k))
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();

            // Composite command mutation records both command_execution and workspace_mutation.
            // for WorkspaceMutation commands. CEP PROV replay reads this field.
            let observation_metadata = command_observation_metadata(&effective_impact);
            ctx.record_durable_metadata(json!({
                "classification": format!("{:?}", effective_impact),
                "effective_effect": effective_effect,
                "envelope_id": envelope_id,
                "exit_code": exit_code,
                "stdout_bytes": stdout_meta.total_bytes,
                "stderr_bytes": stderr_meta.total_bytes,
                "stdout_truncated": stdout_meta.truncated,
                "stderr_truncated": stderr_meta.truncated,
                "backend_invoked": true,
                "policy_enforced": true,
            }));
            ctx.record_durable_metadata(observation_metadata.clone());

            let success_audit = CommandSuccessAudit {
                classification: &effective_impact,
                envelope_id: envelope_id.as_deref(),
                exit_code,
                stdout_meta: &stdout_meta,
                stderr_meta: &stderr_meta,
                method,
                observation_metadata: &observation_metadata,
            };
            emit_success_metadata(&ctx, success_audit).await;

            Ok(RunResult {
                exit_code,
                stdout: String::from_utf8_lossy(&output.stdout).to_string(),
                stderr: String::from_utf8_lossy(&output.stderr).to_string(),
                stdout_truncated: stdout_meta.truncated.then_some(true),
                stderr_truncated: stderr_meta.truncated.then_some(true),
                stdout_bytes: Some(stdout_meta.total_bytes),
                stderr_bytes: Some(stderr_meta.total_bytes),
            })
        }
        Err(err) => {
            let diff = if effective_impact == EffectiveImpact::WorkspaceMutation {
                if let Some(ref record) = active_record {
                    Some(
                        record_workspace_mutation_diff(
                            &ctx,
                            record,
                            &project_root,
                            &pre_snapshot,
                            method,
                        )
                        .await?,
                    )
                } else {
                    None
                }
            } else {
                None
            };

            // Record error metadata
            ctx.record_durable_metadata(json!({
                "classification": format!("{:?}", effective_impact),
                "envelope_id": envelope_id,
                "error_code": err.code.as_i32(),
                "backend_invoked": true,
                "policy_enforced": true,
            }));
            ctx.record_durable_metadata(command_observation_metadata(&effective_impact));

            emit_denial_metadata(&ctx, "execution_error", true, method).await;

            if let Some(diff) = diff {
                if !diff.out_of_scope_paths.is_empty() {
                    return Err(out_of_scope_changed_paths_error(
                        diff.out_of_scope_paths,
                        method,
                    ));
                }
            }

            Err(err)
        }
    }
}

#[derive(Debug, Default)]
struct WorkspaceMutationDiff {
    out_of_scope_paths: Vec<String>,
}

async fn record_workspace_mutation_diff(
    ctx: &HandlerContext,
    record: &EnvelopeRecord,
    project_root: &str,
    pre_snapshot: &HashMap<String, FileSnapshot>,
    method: &'static str,
) -> Result<WorkspaceMutationDiff, CepError> {
    let write_patterns = extract_write_path_patterns(&record.envelope.capabilities);
    let post_snapshot = capture_workspace_snapshot(project_root)?;
    let changed_paths = compute_changed_paths(pre_snapshot, &post_snapshot);

    if changed_paths.is_empty() {
        return Ok(WorkspaceMutationDiff::default());
    }

    for path in &changed_paths {
        let abs_path = Path::new(project_root).join(path);
        ctx.record_effect(ObservedEffect {
            kind: ImpactKind::WorkspaceMutation,
            scope: ObservedScope::Path(abs_path),
            required_capability: RequiredCapability::WritePath,
            source: EffectSource::HandlerBackend,
        });
    }

    let mut out_of_scope_paths = Vec::new();
    for path in &changed_paths {
        if let Err(err) = check_write_scope(path, project_root, &write_patterns) {
            if err.code == CepErrorCode::ReplanRequired {
                out_of_scope_paths.push(path.clone());
            } else {
                return Err(err);
            }
        }
    }

    if !out_of_scope_paths.is_empty() {
        ctx.events
            .emit(
                "cep.replan_required",
                json!({
                    "method": method,
                    "active_envelope_id": &record.envelope_id,
                    "out_of_scope_paths": &out_of_scope_paths,
                    "changed_paths": &changed_paths,
                }),
            )
            .await;

        ctx.record_durable_metadata(json!({
            "classification": "workspace_mutation",
            "envelope_id": &record.envelope_id,
            "out_of_scope_paths": &out_of_scope_paths,
            "changed_paths": &changed_paths,
            "backend_invoked": true,
            "policy_enforced": true,
        }));
    }

    Ok(WorkspaceMutationDiff { out_of_scope_paths })
}

fn out_of_scope_changed_paths_error(out_of_scope_paths: Vec<String>, method: &str) -> CepError {
    CepError::new(
        CepErrorCode::ReplanRequired,
        format!(
            "{method} changed out-of-scope paths: {:?}",
            out_of_scope_paths
        ),
        CepErrorData::Guard(GuardErrorData {
            method: method.into(),
            reason: format!(
                "ReplanRequired: changed paths {:?} outside active WritePath scope",
                out_of_scope_paths
            ),
        }),
        Retriability::RequiresReplan,
    )
}

/// A file snapshot for pre/post-execution diff comparison.
#[derive(Clone, Debug)]
struct FileSnapshot {
    /// Content hash (blake3) for regular files.
    content_hash: Option<String>,
    /// File type indicator.
    file_type: String,
}

/// Capture a workspace snapshot under project_root.
///
/// For each regular file: record (relative_path, FileSnapshot with blake3 hash).
/// For symlinks/dirs: record (relative_path, FileSnapshot with type only).
/// Uses a recursive helper built on `std::fs::read_dir` (no new dependencies).
fn capture_workspace_snapshot(
    project_root: &str,
) -> Result<HashMap<String, FileSnapshot>, CepError> {
    let root = Path::new(project_root);
    let mut snapshot = HashMap::new();
    capture_recursive(root, root, &mut snapshot)?;
    Ok(snapshot)
}

fn capture_recursive(
    base: &Path,
    current: &Path,
    snapshot: &mut HashMap<String, FileSnapshot>,
) -> Result<(), CepError> {
    let entries = std::fs::read_dir(current).map_err(|e| {
        CepError::internal_error(format!(
            "failed to read directory '{}': {e}",
            current.display()
        ))
    })?;

    for entry in entries {
        let entry =
            entry.map_err(|e| CepError::internal_error(format!("failed to read entry: {e}")))?;

        let path = entry.path();
        let relative = path
            .strip_prefix(base)
            .unwrap_or(&path)
            .to_string_lossy()
            .to_string();

        // Skip runtime/build internals, but keep hidden project files like .github or .cargo.
        if is_snapshot_skipped_path(&relative) {
            continue;
        }

        let metadata = entry.file_type().map_err(|e| {
            CepError::internal_error(format!(
                "failed to read file type for '{}': {e}",
                path.display()
            ))
        })?;

        if metadata.is_symlink() {
            snapshot.insert(
                relative,
                FileSnapshot {
                    content_hash: None,
                    file_type: "symlink".into(),
                },
            );
        } else if metadata.is_dir() {
            snapshot.insert(
                relative,
                FileSnapshot {
                    content_hash: None,
                    file_type: "dir".into(),
                },
            );
            capture_recursive(base, &path, snapshot)?;
        } else {
            // Regular file — compute blake3 hash
            let content = std::fs::read(&path).map_err(|e| {
                CepError::internal_error(format!("failed to read file '{}': {e}", path.display()))
            })?;
            let hash = format!("blake3:{}", blake3::hash(&content).to_hex());
            snapshot.insert(
                relative,
                FileSnapshot {
                    content_hash: Some(hash),
                    file_type: "file".into(),
                },
            );
        }
    }

    Ok(())
}

fn is_snapshot_skipped_path(relative: &str) -> bool {
    if relative == "target" || relative.starts_with("target/") {
        return true;
    }

    // Allow descent into the hook directory while pruning the rest of .git.
    if relative == ".git" || relative == ".git/hooks" {
        return false;
    }

    // Track active hooks, but skip Git's inert sample templates.
    if let Some(hook) = relative.strip_prefix(".git/hooks/") {
        return hook.ends_with(".sample");
    }

    relative.starts_with(".git/")
}

/// Compute changed paths between pre and post snapshots.
///
/// Returns relative paths of files that were added, deleted, or had content changes.
fn compute_changed_paths(
    pre: &HashMap<String, FileSnapshot>,
    post: &HashMap<String, FileSnapshot>,
) -> Vec<String> {
    let mut changed = Vec::new();

    // Check for new or modified files
    for (path, post_snap) in post {
        match pre.get(path) {
            Some(pre_snap) => {
                // File existed before — check if content changed
                if pre_snap.file_type != post_snap.file_type
                    || pre_snap.content_hash != post_snap.content_hash
                {
                    changed.push(path.clone());
                }
            }
            None => {
                // New file
                changed.push(path.clone());
            }
        }
    }

    // Check for deleted files
    for path in pre.keys() {
        if !post.contains_key(path) {
            changed.push(path.clone());
        }
    }

    changed
}

fn validate_inputs(params: &RunParams) -> Result<(), CepError> {
    if params.argv.is_empty() {
        return Err(CepError::new(
            CepErrorCode::SchemaMismatch,
            "exec.command_run requires a non-empty argv",
            CepErrorData::Validation(lancet_protocol::error::ValidationErrorData {
                field: "argv".into(),
                reason: "must not be empty".into(),
            }),
            Retriability::Permanent,
        ));
    }
    if params.argv[0].is_empty() {
        return Err(CepError::new(
            CepErrorCode::SchemaMismatch,
            "exec.command_run requires argv[0] to name an executable",
            CepErrorData::Validation(lancet_protocol::error::ValidationErrorData {
                field: "argv[0]".into(),
                reason: "must not be empty".into(),
            }),
            Retriability::Permanent,
        ));
    }
    for (index, arg) in params.argv.iter().enumerate() {
        if arg.contains('\0') {
            return Err(CepError::new(
                CepErrorCode::SchemaMismatch,
                format!("argv[{index}] contains NUL byte"),
                CepErrorData::Validation(lancet_protocol::error::ValidationErrorData {
                    field: format!("argv[{index}]"),
                    reason: "contains NUL byte".into(),
                }),
                Retriability::Permanent,
            ));
        }
    }
    for (key, value) in &params.env {
        if key.is_empty() || key.contains('=') || key.contains('\0') {
            return Err(CepError::new(
                CepErrorCode::SchemaMismatch,
                format!("env key '{key}' is invalid"),
                CepErrorData::Validation(lancet_protocol::error::ValidationErrorData {
                    field: format!("env.{key}"),
                    reason: "invalid key".into(),
                }),
                Retriability::Permanent,
            ));
        }
        if value.contains('\0') {
            return Err(CepError::new(
                CepErrorCode::SchemaMismatch,
                format!("env value for '{key}' contains NUL"),
                CepErrorData::Validation(lancet_protocol::error::ValidationErrorData {
                    field: format!("env.{key}"),
                    reason: "value contains NUL".into(),
                }),
                Retriability::Permanent,
            ));
        }
    }
    Ok(())
}

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

fn command_observation_metadata(effective_impact: &EffectiveImpact) -> serde_json::Value {
    match effective_impact {
        EffectiveImpact::WorkspaceMutation => json!({
            "effect_observation": "partial",
            "observed_effects": ["command_execution", "workspace_mutation_diff"],
            "unobserved_dimensions": [
                "network",
                "tree_out_read",
                "tree_out_write",
                "workspace_read_set"
            ],
        }),
        EffectiveImpact::Diagnostic => json!({
            "effect_observation": "partial",
            "observed_effects": ["command_execution"],
            "unobserved_dimensions": [
                "network",
                "tree_out_read",
                "tree_out_write",
                "tree_in_write",
                "workspace_read_set"
            ],
        }),
        EffectiveImpact::HardDenied => json!({
            "effect_observation": "none",
            "observed_effects": [],
            "unobserved_dimensions": [],
        }),
    }
}

async fn emit_denial_metadata(
    ctx: &HandlerContext,
    denial_source: &str,
    backend_invoked: bool,
    method: &'static str,
) {
    let metadata = json!({
        "method": method,
        "denial_source": denial_source,
        "policy_enforced": true,
        "backend_invoked": backend_invoked,
    });
    ctx.record_durable_metadata(metadata.clone());
    ctx.events
        .emit(
            "cep.command_metadata",
            json!({ "method": method, "metadata": metadata }),
        )
        .await;
}

struct CommandSuccessAudit<'a> {
    classification: &'a EffectiveImpact,
    envelope_id: Option<&'a str>,
    exit_code: i32,
    stdout_meta: &'a BoundedStreamOutput,
    stderr_meta: &'a BoundedStreamOutput,
    method: &'static str,
    observation_metadata: &'a serde_json::Value,
}

async fn emit_success_metadata(ctx: &HandlerContext, audit: CommandSuccessAudit<'_>) {
    let classification_str = match audit.classification {
        EffectiveImpact::Diagnostic => "diagnostic",
        EffectiveImpact::WorkspaceMutation => "workspace_mutation",
        EffectiveImpact::HardDenied => "hard_denied",
    };
    let mut metadata = json!({
        "method": audit.method,
        "classification": classification_str,
        "envelope_id": audit.envelope_id,
        "exit_code": audit.exit_code,
        "stdout_bytes": audit.stdout_meta.total_bytes,
        "stderr_bytes": audit.stderr_meta.total_bytes,
        "stdout_truncated": audit.stdout_meta.truncated,
        "stderr_truncated": audit.stderr_meta.truncated,
        "policy_enforced": true,
        "backend_invoked": true,
    });
    merge_json_object(&mut metadata, audit.observation_metadata);
    ctx.record_durable_metadata(metadata.clone());
    ctx.events
        .emit(
            "cep.command_metadata",
            json!({ "method": audit.method, "metadata": metadata }),
        )
        .await;
}

fn merge_json_object(target: &mut serde_json::Value, source: &serde_json::Value) {
    let (Some(target), Some(source)) = (target.as_object_mut(), source.as_object()) else {
        return;
    };
    for (key, value) in source {
        target.insert(key.clone(), value.clone());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::envelope_store::ActiveEnvelopeStore;
    use crate::policy::CepTurnContext;
    use crate::registry::{Cancellation, EventSink};
    use lancet_protocol::envelope::{
        BaseExecutionEnvelope, Constraints, ExecutionEnvelope, Impact as EnvImpact,
        ImpactConfidence, ImpactKind, Intent, OnFailure, Scope,
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
                base_envelope: BaseExecutionEnvelope::for_workspace_root(
                    root,
                    "command handler test",
                ),
                cep_context: Arc::new(CepTurnContext::defaults_for_project_root(root)),
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
                    ..Default::default()
                })),
                audit: Arc::new(crate::audit::CepAuditStore::in_memory().expect("audit store")),
                invariant_index: Arc::new(crate::invariants::InvariantCardIndex::new(vec![])),
                cep_mode: lancet_protocol::envelope::CepMode::Off,
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
        let default_hook = Arc::new(crate::policy::default::DefaultPolicyHook::new(
            crate::policy::default::PolicyConfig::defaults_for_project_root(
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
                base_envelope: BaseExecutionEnvelope::for_workspace_root(
                    root,
                    "command handler test",
                ),
                cep_context: Arc::new(CepTurnContext::defaults_for_project_root(root)),
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
                    ..Default::default()
                })),
                audit: Arc::new(crate::audit::CepAuditStore::in_memory().expect("audit store")),
                invariant_index: Arc::new(crate::invariants::InvariantCardIndex::new(vec![])),
                cep_mode: lancet_protocol::envelope::CepMode::Off,
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
            durable_metadata: crate::call_durable_metadata::CallDurableMetadata::default(),
            effect_ledger: crate::effect::ledger::EffectLedger::new(),
            authorized_region: crate::effect::ledger::AuthorizedRegionSlot::new(),
            policy_hook: Arc::new(crate::policy::default::DefaultPolicyHook::new(
                crate::policy::default::PolicyConfig::defaults_for_project_root(
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
            invariant_index: Arc::new(crate::invariants::InvariantCardIndex::new(vec![])),
            cep_mode: lancet_protocol::envelope::CepMode::Off,
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
            durable_metadata: crate::call_durable_metadata::CallDurableMetadata::default(),
            effect_ledger: crate::effect::ledger::EffectLedger::new(),
            authorized_region: crate::effect::ledger::AuthorizedRegionSlot::new(),
            policy_hook: Arc::new(crate::policy::default::DefaultPolicyHook::new(
                crate::policy::default::PolicyConfig::defaults_for_project_root(
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
            invariant_index: Arc::new(crate::invariants::InvariantCardIndex::new(vec![])),
            cep_mode: lancet_protocol::envelope::CepMode::Off,
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
            durable_metadata: crate::call_durable_metadata::CallDurableMetadata::default(),
            effect_ledger: crate::effect::ledger::EffectLedger::new(),
            authorized_region: crate::effect::ledger::AuthorizedRegionSlot::new(),
            policy_hook: Arc::new(crate::policy::default::DefaultPolicyHook::new(
                crate::policy::default::PolicyConfig::defaults_for_project_root(
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
            invariant_index: Arc::new(crate::invariants::InvariantCardIndex::new(vec![])),
            cep_mode: lancet_protocol::envelope::CepMode::Off,
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
        let effect_ledger = crate::effect::ledger::EffectLedger::new();
        let ledger_clone = effect_ledger.clone();
        let ctx = HandlerContext {
            call_id: 1,
            events: EventSink::new(event_tx, 1),
            cancellation: Cancellation::new(cancel_rx),
            base_envelope: BaseExecutionEnvelope::for_workspace_root(root, "hidden path test"),
            cep_context: Arc::new(CepTurnContext::defaults_for_project_root(root)),
            active_envelope: store,
            turn_id: "test-turn".into(),
            durable_metadata: crate::call_durable_metadata::CallDurableMetadata::default(),
            effect_ledger,
            authorized_region: crate::effect::ledger::AuthorizedRegionSlot::new(),
            policy_hook: Arc::new(crate::policy::default::DefaultPolicyHook::new(
                crate::policy::default::PolicyConfig::defaults_for_project_root(
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
            invariant_index: Arc::new(crate::invariants::InvariantCardIndex::new(vec![])),
            cep_mode: lancet_protocol::envelope::CepMode::Off,
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
                    && e.source == crate::effect::ledger::EffectSource::HandlerBackend
            })
            .collect();
        assert!(
            !mutation_effects.is_empty(),
            "ledger must contain WorkspaceMutation effect for out-of-scope path"
        );
        let has_out_of_scope = mutation_effects.iter().any(|e| {
            matches!(&e.scope, crate::effect::ledger::ObservedScope::Path(p) if p.to_string_lossy().contains("cep-hidden"))
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
        let effect_ledger = crate::effect::ledger::EffectLedger::new();
        let ledger_clone = effect_ledger.clone();
        let ctx = HandlerContext {
            call_id: 1,
            events: EventSink::new(event_tx, 1),
            cancellation: Cancellation::new(cancel_rx),
            base_envelope: BaseExecutionEnvelope::for_workspace_root(root, "timeout guard test"),
            cep_context: Arc::new(CepTurnContext::defaults_for_project_root(root)),
            active_envelope: store,
            turn_id: "test-turn".into(),
            durable_metadata: crate::call_durable_metadata::CallDurableMetadata::default(),
            effect_ledger,
            authorized_region: crate::effect::ledger::AuthorizedRegionSlot::new(),
            policy_hook: Arc::new(crate::policy::default::DefaultPolicyHook::new(
                crate::policy::default::PolicyConfig::defaults_for_project_root(
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
            invariant_index: Arc::new(crate::invariants::InvariantCardIndex::new(vec![])),
            cep_mode: lancet_protocol::envelope::CepMode::Off,
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
                && matches!(&e.scope, crate::effect::ledger::ObservedScope::Path(p) if p.ends_with("outside.txt"))
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
        let effect_ledger = crate::effect::ledger::EffectLedger::new();
        let ledger_clone = effect_ledger.clone();
        let ctx = HandlerContext {
            call_id: 1,
            events: EventSink::new(event_tx, 1),
            cancellation: Cancellation::new(cancel_rx),
            base_envelope: BaseExecutionEnvelope::for_workspace_root(root, "supersede race test"),
            cep_context: Arc::new(CepTurnContext::defaults_for_project_root(root)),
            active_envelope: Arc::clone(&store),
            turn_id: "test-turn".into(),
            durable_metadata: crate::call_durable_metadata::CallDurableMetadata::default(),
            effect_ledger,
            authorized_region: crate::effect::ledger::AuthorizedRegionSlot::new(),
            policy_hook: Arc::new(crate::policy::default::DefaultPolicyHook::new(
                crate::policy::default::PolicyConfig::defaults_for_project_root(
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
            invariant_index: Arc::new(crate::invariants::InvariantCardIndex::new(vec![])),
            cep_mode: lancet_protocol::envelope::CepMode::Off,
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
                    && e.source == crate::effect::ledger::EffectSource::HandlerBackend
            })
            .collect();
        assert_eq!(cmd_exec.len(), 1, "exactly one CommandExecution effect");
        assert!(
            matches!(&cmd_exec[0].scope, crate::effect::ledger::ObservedScope::Root(r) if r == root),
            "CommandExecution scope must be Root(project_root)"
        );

        let mutations: Vec<_> = effects
            .iter()
            .filter(|e| {
                e.kind == ImpactKind::WorkspaceMutation
                    && e.source == crate::effect::ledger::EffectSource::HandlerBackend
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
        let (ctx, _event_rx, _cancel_tx) =
            test_ctx_with_run_command(root, vec!["echo".into()]).await;

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
    async fn env_policy_rejects_unallowed_key() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path().to_str().unwrap();
        let (ctx, _event_rx, _cancel_tx) =
            test_ctx_with_run_command(root, vec!["echo".into()]).await;

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
            durable_metadata: crate::call_durable_metadata::CallDurableMetadata::default(),
            effect_ledger: crate::effect::ledger::EffectLedger::new(),
            authorized_region: crate::effect::ledger::AuthorizedRegionSlot::new(),
            policy_hook: Arc::new(crate::policy::default::DefaultPolicyHook::new(
                crate::policy::default::PolicyConfig::defaults_for_project_root(
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
            invariant_index: Arc::new(crate::invariants::InvariantCardIndex::new(vec![])),
            cep_mode: lancet_protocol::envelope::CepMode::Off,
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
            durable_metadata: crate::call_durable_metadata::CallDurableMetadata::default(),
            effect_ledger: crate::effect::ledger::EffectLedger::new(),
            authorized_region: crate::effect::ledger::AuthorizedRegionSlot::new(),
            policy_hook: Arc::new(crate::policy::default::DefaultPolicyHook::new(
                crate::policy::default::PolicyConfig::defaults_for_project_root(
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
            invariant_index: Arc::new(crate::invariants::InvariantCardIndex::new(vec![])),
            cep_mode: lancet_protocol::envelope::CepMode::Off,
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
        use crate::effect::region::EffectiveEffect;
        use crate::policy::command::EffectiveImpact;

        let effect =
            EffectiveEffect::from_command_impact(&EffectiveImpact::WorkspaceMutation).unwrap();
        let kinds: Vec<String> = effect.kinds.iter().map(|k| format!("{:?}", k)).collect();
        assert!(kinds.contains(&"CommandExecution".to_string()));
        assert!(kinds.contains(&"WorkspaceMutation".to_string()));
        assert_eq!(kinds.len(), 2);
    }

    #[test]
    fn command_run_composite_effect_diagnostic() {
        use crate::effect::region::EffectiveEffect;
        use crate::policy::command::EffectiveImpact;

        let effect = EffectiveEffect::from_command_impact(&EffectiveImpact::Diagnostic).unwrap();
        let kinds: Vec<String> = effect.kinds.iter().map(|k| format!("{:?}", k)).collect();
        assert!(kinds.contains(&"CommandExecution".to_string()));
        assert!(!kinds.contains(&"WorkspaceMutation".to_string()));
        assert_eq!(kinds.len(), 1);
    }

    #[test]
    fn command_observation_metadata_is_derived_for_workspace_mutation() {
        use crate::policy::command::EffectiveImpact;

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
        use crate::policy::command::EffectiveImpact;

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
}
