//! Native exec.command_run CEP handler with policy enforcement.
//!
//! This is the only shell execution path from the agent.
//! WorkspaceMutation commands capture a pre-execution workspace snapshot, compute
//! changed paths after execution, and check them against WritePath scope before
//! running any configured verify command.

use crate::enforcement::guard_l1::{
    check_write_scope, evaluate_and_enforce_policy, extract_write_path_patterns,
    has_run_command_capability,
};
use crate::enforcement::isolation::{
    command_boundary_metadata, evaluate_command_boundary_attempt,
    evaluate_command_boundary_preflight, unavailable_required_boundaries, BoundaryAttemptOutcome,
    CommandIsolationAttempt, CommandIsolationMode, CommandIsolationUnsupportedHostBehavior,
    MinimalMountViewStatus, SupervisorSetupStatus,
};
use crate::enforcement::verify::{
    extract_verify_command, run_verify, validate_verify_command, verify_failed_error,
};
use crate::executor::limits::ToolLimits;
use crate::executor::shell::{
    execute_argv, execute_argv_supervised, BoundedStreamOutput, ExecuteConfig,
    SupervisedCommandError, SupervisorExecution,
};
use crate::executor::supervisor::env::build_isolated_env;
use crate::executor::supervisor::{default_executable_view_paths, SupervisorPlan};
use crate::server::registry::{HandlerContext, MethodRegistry};
use lancet_authority::effect::ledger::{EffectSource, ObservedEffect, ObservedScope};
use lancet_authority::envelope_store::EnvelopeRecord;
use lancet_authority::policy::command::{
    build_allowed_params_env, build_effective_env, classify_command, validate_timeout,
    EffectiveImpact,
};
use lancet_catalog::method::RequiredCapability;
use lancet_catalog::methods::command::{Run, RunParams, RunResult};
use lancet_catalog::CepMethod;
use lancet_protocol::envelope::Capability;
use lancet_protocol::envelope::ImpactKind;
use lancet_protocol::error::{CepErrorData, GuardErrorData, PermissionErrorData};
use lancet_protocol::{CepError, CepErrorCode, Retriability};
use serde_json::json;
use std::collections::{BTreeMap, HashMap};
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
    let effective_impact = classify_command(&params.argv, params.declared_impact.clone(), policy)?;

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
                if let Some((path, is_prefix)) =
                    lancet_authority::effect::region::normalize_write_region_path(
                        path_pattern,
                        &ctx.cep_context.policy.project_root,
                    )
                {
                    if is_prefix {
                        write_path_prefixes.push(path);
                    } else {
                        write_paths.push(path);
                    }
                }
            }
        }
        if !write_paths.is_empty() || !write_path_prefixes.is_empty() {
            let mut write_region =
                lancet_authority::effect::region::AuthorizedEffectRegion::empty();
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
    let effective_timeout = validate_timeout(params.timeout_ms, policy)?;

    let project_root = ctx
        .cep_context
        .policy
        .project_root
        .to_string_lossy()
        .to_string();

    // 8. Build effective env and supervisor attempt. Isolation `off`
    // preserves compatibility host env inheritance; isolated modes validate
    // caller env before planning and use supervisor-generated allowlist env.
    let execution_plan = prepare_command_execution_plan(&ctx, policy, &params, &project_root)?;

    // 8b. Pre-known command physical-boundary decisions. Live supervisor
    // attempts are recorded after the child setup report is received, because
    // host-probe availability alone is not applied evidence.
    if execution_plan.supervisor_plan.is_none() {
        let boundary_preflight = match execution_plan.boundary_attempt.as_ref() {
            Some(attempt) => {
                evaluate_command_boundary_attempt(&ctx.command_isolation, Some(attempt))
            }
            None => evaluate_command_boundary_preflight(&ctx.command_isolation),
        };
        record_command_boundary_preflight(&ctx, method, &boundary_preflight).await;
        if boundary_preflight.should_fail_closed() {
            return Err(command_boundary_fail_closed_error(
                method,
                &boundary_preflight,
            ));
        }
    }

    // 9. Execute via rewritten execute_argv with bounded tail reader
    let mut cancellation = ctx.cancellation.clone();

    // 9a. Pre-execution baseline for WorkspaceMutation commands.
    let pre_snapshot = if effective_impact == EffectiveImpact::WorkspaceMutation {
        capture_workspace_snapshot(&project_root)?
    } else {
        HashMap::new()
    };

    let execute_config = ExecuteConfig {
        project_root: &project_root,
        limits: &ToolLimits::default(),
        argv: &execution_plan.argv,
        cwd: params.cwd.as_deref(),
        env: &execution_plan.effective_env,
        timeout_ms: Some(effective_timeout),
        cancellation: &mut cancellation,
        max_output_bytes: policy.max_output_bytes,
    };
    let result = match execution_plan.supervisor_plan {
        Some(plan) => {
            match execute_argv_supervised(execute_config, SupervisorExecution::Live { plan }).await
            {
                Ok(output) => {
                    record_command_boundary_attempt(&ctx, method, &output.setup_attempt).await;
                    Ok((output.output, output.stdout_meta, output.stderr_meta))
                }
                Err(err) if should_run_unbounded_fallback(&ctx.command_isolation, &err) => {
                    if let Some(attempt) = err.setup_attempt.as_ref() {
                        record_command_boundary_attempt(&ctx, method, attempt).await;
                    }
                    let fallback_env = build_unbounded_fallback_env(&params.env, policy)?;
                    let mut fallback_cancellation = ctx.cancellation.clone();
                    let fallback_config = ExecuteConfig {
                        project_root: &project_root,
                        limits: &ToolLimits::default(),
                        argv: &params.argv,
                        cwd: params.cwd.as_deref(),
                        env: &fallback_env,
                        timeout_ms: Some(effective_timeout),
                        cancellation: &mut fallback_cancellation,
                        max_output_bytes: policy.max_output_bytes,
                    };
                    execute_argv(fallback_config)
                        .await
                        .map_err(|error| CommandExecutionFailure {
                            error,
                            backend_invoked: true,
                        })
                }
                Err(err) => {
                    if let Some(attempt) = err.setup_attempt.as_ref() {
                        record_command_boundary_attempt(&ctx, method, attempt).await;
                    }
                    Err(CommandExecutionFailure {
                        backend_invoked: supervised_error_backend_invoked(&err),
                        error: err.error,
                    })
                }
            }
        }
        None => execute_argv(execute_config)
            .await
            .map_err(|error| CommandExecutionFailure {
                error,
                backend_invoked: true,
            }),
    };

    // 10. Handle result
    match result {
        Ok((output, stdout_meta, stderr_meta)) => {
            // 11. Build RunResult with truncation metadata
            let exit_code = output.status.code().unwrap_or(-1);
            let observation_metadata = command_observation_metadata(&effective_impact);
            let execution_metadata = command_execution_metadata(CommandExecutionMetadata {
                classification: &effective_impact,
                envelope_id: envelope_id.as_deref(),
                exit_code,
                stdout_meta: &stdout_meta,
                stderr_meta: &stderr_meta,
                method,
                observation_metadata: &observation_metadata,
            });

            ctx.record_effect(ObservedEffect {
                kind: ImpactKind::CommandExecution,
                scope: ObservedScope::Root(project_root.clone()),
                required_capability: RequiredCapability::RunCommand,
                source: EffectSource::HandlerBackend,
            });
            ctx.record_durable_metadata(execution_metadata.clone());
            ctx.events
                .emit(
                    "cep.command_metadata",
                    json!({ "method": method, "metadata": execution_metadata }),
                )
                .await;

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

            // Keep observation metadata available as top-level durable fields for
            // provenance readers that do not inspect command_metadata events.
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
        Err(failure) => {
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
                "error_code": failure.error.code.as_i32(),
                "backend_invoked": failure.backend_invoked,
                "policy_enforced": true,
            }));
            ctx.record_durable_metadata(command_observation_metadata(&effective_impact));

            emit_denial_metadata(&ctx, "execution_error", failure.backend_invoked, method).await;

            if let Some(diff) = diff {
                if !diff.out_of_scope_paths.is_empty() {
                    return Err(out_of_scope_changed_paths_error(
                        diff.out_of_scope_paths,
                        method,
                    ));
                }
            }

            Err(failure.error)
        }
    }
}

fn command_boundary_fail_closed_error(
    method: &str,
    preflight: &crate::enforcement::isolation::CommandBoundaryPreflight,
) -> CepError {
    let message = preflight.failure_message();
    CepError::new(
        CepErrorCode::GuardDenied,
        message.clone(),
        CepErrorData::Guard(GuardErrorData {
            method: method.into(),
            reason: message,
        }),
        Retriability::Permanent,
    )
}

struct PreparedCommandExecution {
    argv: Vec<String>,
    effective_env: BTreeMap<String, String>,
    boundary_attempt: Option<CommandIsolationAttempt>,
    supervisor_plan: Option<SupervisorPlan>,
}

struct CommandExecutionFailure {
    error: CepError,
    backend_invoked: bool,
}

fn prepare_command_execution_plan(
    ctx: &HandlerContext,
    policy: &lancet_authority::policy::command::CommandPolicy,
    params: &RunParams,
    project_root: &str,
) -> Result<PreparedCommandExecution, CepError> {
    match ctx.command_isolation.mode {
        CommandIsolationMode::Off => Ok(PreparedCommandExecution {
            argv: params.argv.clone(),
            effective_env: build_effective_env(&params.env, policy)?,
            boundary_attempt: None,
            supervisor_plan: None,
        }),
        CommandIsolationMode::Enforce | CommandIsolationMode::Audit => {
            build_allowed_params_env(&params.env, policy)?;
            if ctx.command_isolation.mode == CommandIsolationMode::Enforce
                && required_host_boundaries_unavailable(&ctx.command_isolation)
            {
                return Ok(PreparedCommandExecution {
                    argv: params.argv.clone(),
                    effective_env: BTreeMap::new(),
                    boundary_attempt: None,
                    supervisor_plan: None,
                });
            }
            match SupervisorPlan::build(
                Path::new(project_root),
                params.cwd.as_deref(),
                &params.argv,
                &ctx.command_isolation,
            ) {
                Ok(plan) => {
                    let isolated_env =
                        build_isolated_env(&params.env, policy, &plan, &BTreeMap::new())?;
                    let argv = supervised_argv(&params.argv, &plan);
                    Ok(PreparedCommandExecution {
                        argv,
                        effective_env: isolated_env.values,
                        boundary_attempt: None,
                        supervisor_plan: Some(plan),
                    })
                }
                Err(err) => {
                    let attempt = boundary_attempt_from_plan_error(&err);
                    if ctx.command_isolation.mode == CommandIsolationMode::Enforce {
                        Ok(PreparedCommandExecution {
                            argv: params.argv.clone(),
                            effective_env: BTreeMap::new(),
                            boundary_attempt: Some(attempt),
                            supervisor_plan: None,
                        })
                    } else {
                        let fallback_plan = SupervisorPlan::env_fd_hygiene(
                            Path::new("/project").to_path_buf(),
                            Path::new("/project").to_path_buf(),
                            default_executable_view_paths(),
                            vec![Path::new("/tmp").to_path_buf()],
                        );
                        let isolated_env = build_isolated_env(
                            &params.env,
                            policy,
                            &fallback_plan,
                            &BTreeMap::new(),
                        )?;
                        Ok(PreparedCommandExecution {
                            argv: params.argv.clone(),
                            effective_env: isolated_env.values,
                            boundary_attempt: Some(attempt),
                            supervisor_plan: None,
                        })
                    }
                }
            }
        }
    }
}

fn supervised_argv(params_argv: &[String], plan: &SupervisorPlan) -> Vec<String> {
    let mut argv = params_argv.to_vec();
    if let Some(first) = argv.first_mut() {
        *first = plan.executable.view_path.to_string_lossy().into_owned();
    }
    argv
}

fn required_host_boundaries_unavailable(
    config: &crate::enforcement::isolation::CommandIsolationRuntimeConfig,
) -> bool {
    let preflight = evaluate_command_boundary_preflight(config);
    !unavailable_required_boundaries(&preflight.host_probe, &config.required_boundaries).is_empty()
}

async fn record_command_boundary_preflight(
    ctx: &HandlerContext,
    method: &str,
    preflight: &crate::enforcement::isolation::CommandBoundaryPreflight,
) {
    let metadata = command_boundary_metadata(&ctx.command_isolation, preflight);
    ctx.record_durable_metadata(metadata.clone());
    ctx.events
        .emit(
            "cep.command_metadata",
            json!({ "method": method, "metadata": metadata }),
        )
        .await;
}

async fn record_command_boundary_attempt(
    ctx: &HandlerContext,
    method: &str,
    attempt: &CommandIsolationAttempt,
) {
    let preflight = evaluate_command_boundary_attempt(&ctx.command_isolation, Some(attempt));
    record_command_boundary_preflight(ctx, method, &preflight).await;
}

fn should_run_unbounded_fallback(
    config: &crate::enforcement::isolation::CommandIsolationRuntimeConfig,
    error: &SupervisedCommandError,
) -> bool {
    config.mode == CommandIsolationMode::Audit
        && config.unsupported_host == CommandIsolationUnsupportedHostBehavior::AllowUnbounded
        && error
            .setup_attempt
            .as_ref()
            .is_some_and(|attempt| attempt.outcome.failed_closed)
}

fn supervised_error_backend_invoked(error: &SupervisedCommandError) -> bool {
    !error
        .setup_attempt
        .as_ref()
        .is_some_and(|attempt| attempt.outcome.failed_closed)
}

fn build_unbounded_fallback_env(
    params_env: &BTreeMap<String, String>,
    policy: &lancet_authority::policy::command::CommandPolicy,
) -> Result<BTreeMap<String, String>, CepError> {
    let fallback_plan = SupervisorPlan::env_fd_hygiene(
        Path::new("/project").to_path_buf(),
        Path::new("/project").to_path_buf(),
        default_executable_view_paths(),
        vec![Path::new("/tmp").to_path_buf()],
    );
    Ok(build_isolated_env(params_env, policy, &fallback_plan, &BTreeMap::new())?.values)
}

fn boundary_attempt_from_plan_error(err: &CepError) -> CommandIsolationAttempt {
    CommandIsolationAttempt::new(BoundaryAttemptOutcome {
        supervisor_backend: Some("supervisor_plan".to_string()),
        setup_failure_stage: Some("plan".to_string()),
        setup_failure_reason: Some(err.message.clone()),
        failed_closed: true,
        network_namespace_status: SupervisorSetupStatus::FailedClosed,
        mount_namespace_status: SupervisorSetupStatus::FailedClosed,
        minimal_mount_view_status: MinimalMountViewStatus::FailedClosed,
        fd_cleanup_status: Some("not_applied".to_string()),
        env_policy: Some("not_applied".to_string()),
        ..BoundaryAttemptOutcome::default()
    })
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

struct CommandExecutionMetadata<'a> {
    classification: &'a EffectiveImpact,
    envelope_id: Option<&'a str>,
    exit_code: i32,
    stdout_meta: &'a BoundedStreamOutput,
    stderr_meta: &'a BoundedStreamOutput,
    method: &'static str,
    observation_metadata: &'a serde_json::Value,
}

fn command_execution_metadata(input: CommandExecutionMetadata<'_>) -> serde_json::Value {
    let mut metadata = json!({
        "method": input.method,
        "classification": command_classification_label(input.classification),
        "effective_effect": command_effective_effect_labels(input.classification),
        "envelope_id": input.envelope_id,
        "exit_code": input.exit_code,
        "stdout_bytes": input.stdout_meta.total_bytes,
        "stderr_bytes": input.stderr_meta.total_bytes,
        "stdout_truncated": input.stdout_meta.truncated,
        "stderr_truncated": input.stderr_meta.truncated,
        "backend_invoked": true,
        "policy_enforced": true,
    });
    merge_json_object(&mut metadata, input.observation_metadata);
    metadata
}

fn command_effective_effect_labels(effective_impact: &EffectiveImpact) -> Vec<String> {
    lancet_authority::effect::region::EffectiveEffect::from_command_impact(effective_impact)
        .ok()
        .map(|ee| {
            ee.kinds
                .iter()
                .map(|kind| format!("{:?}", kind))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
}

fn command_classification_label(effective_impact: &EffectiveImpact) -> &'static str {
    match effective_impact {
        EffectiveImpact::Diagnostic => "diagnostic",
        EffectiveImpact::WorkspaceMutation => "workspace_mutation",
        EffectiveImpact::HardDenied => "hard_denied",
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
    let mut metadata = json!({
        "method": audit.method,
        "classification": command_classification_label(audit.classification),
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
#[path = "command_tests.rs"]
mod tests;
