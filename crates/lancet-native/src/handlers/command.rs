// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

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
    command_boundary_metadata, effective_boundary_metadata_json_with_observation,
    evaluate_command_boundary_attempt, evaluate_command_boundary_preflight,
    unavailable_required_boundaries, BoundaryAttemptOutcome, CommandBoundaryPreflight,
    CommandIsolationAttempt, CommandIsolationMode, CommandIsolationUnsupportedHostBehavior,
    MinimalMountViewStatus, SupervisorSetupStatus,
};
use crate::enforcement::verify::{
    extract_verify_command, run_verify, run_verify_on_candidate_view, validate_verify_command,
    verify_failed_error,
};
use crate::executor::limits::ToolLimits;
use crate::executor::shell::{
    execute_argv, execute_argv_supervised, BoundedStreamOutput, ExecuteConfig,
    SupervisedCommandError, SupervisorExecution,
};
use crate::executor::supervisor::artifact::{
    adjudicate_artifact_manifest, commit_artifact_manifest, prepare_artifact_commit_set,
    scan_overlay_upper, ArtifactAdjudicationReport, ArtifactCommitReport, CommitOutcome,
    RejectedArtifactKind,
};
use crate::executor::supervisor::env::build_isolated_env;
use crate::executor::supervisor::overlay::{
    cleanup_overlay, prepare_candidate_artifact_view, CleanupOutcome, OverlayCleanupReport,
    OverlayDisposition, OverlayMode, OverlayPlan, OverlayStatus, VerifyDiscardReport,
    VerifyLayerStatus,
};
use crate::executor::supervisor::snapshot::{
    capture_lower_baseline, capture_lower_fingerprint, capture_workspace_snapshot,
    compute_changed_paths, LowerBaseline, LowerConsistencyStatus, LowerFingerprint,
    WorkspaceSnapshot,
};
use crate::executor::supervisor::syscall::LiveSupervisorSyscalls;
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
use std::collections::BTreeMap;
use std::path::Path;

#[cfg(test)]
static TEST_ARTIFACT_VERIFY_WRITE: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

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

    let policy = ctx.cep_context.policy.command.clone();

    // 3. Classify command
    let effective_impact = classify_command(&params.argv, params.declared_impact.clone(), &policy)?;

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
    let effective_timeout = validate_timeout(params.timeout_ms, &policy)?;

    let project_root = ctx
        .cep_context
        .policy
        .project_root
        .to_string_lossy()
        .to_string();

    // 8. Build effective env and supervisor attempt. Isolation `off`
    // preserves compatibility host env inheritance; isolated modes validate
    // caller env before planning and use supervisor-generated allowlist env.
    let execution_plan =
        prepare_command_execution_plan(&ctx, &policy, &params, &project_root, &effective_impact)?;

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
            if effective_impact == EffectiveImpact::Diagnostic {
                record_overlay_command_metadata(
                    &ctx,
                    method,
                    OverlayCommandMetadata {
                        mode: None,
                        status: overlay_status_for_unapplied_diagnostic(&ctx),
                        disposition: None,
                        lower_consistency: None,
                        cleanup: None,
                    },
                )
                .await;
            }
            return Err(command_boundary_fail_closed_error(
                method,
                &boundary_preflight,
            ));
        }
    }

    if effective_impact == EffectiveImpact::Diagnostic {
        if overlay_requested_for_command(&ctx) {
            return handle_diagnostic_overlay_run(
                params,
                ctx,
                method,
                effective_timeout,
                execution_plan,
                project_root,
                &policy,
            )
            .await;
        }
        record_overlay_command_metadata(
            &ctx,
            method,
            OverlayCommandMetadata {
                mode: None,
                status: OverlayStatus::NotRequested,
                disposition: None,
                lower_consistency: None,
                cleanup: None,
            },
        )
        .await;
    }

    if effective_impact == EffectiveImpact::WorkspaceMutation && overlay_requested_for_command(&ctx)
    {
        return handle_artifact_overlay_run(
            params,
            ctx,
            method,
            effective_timeout,
            execution_plan,
            active_record
                .clone()
                .expect("workspace mutation active envelope checked above"),
            project_root,
            &policy,
        )
        .await;
    }

    if effective_impact == EffectiveImpact::WorkspaceMutation {
        let pre_snapshot = capture_workspace_snapshot(&project_root)?;
        return artifact_fallback_without_overlay(
            params,
            ctx,
            method,
            effective_timeout,
            active_record
                .clone()
                .expect("workspace mutation active envelope checked above"),
            project_root,
            &policy,
            pre_snapshot,
        )
        .await;
    }

    // 9. Execute via rewritten execute_argv with bounded tail reader
    let mut cancellation = ctx.cancellation.clone();

    // 9a. Pre-execution baseline for WorkspaceMutation commands.
    let pre_snapshot = if effective_impact == EffectiveImpact::WorkspaceMutation {
        capture_workspace_snapshot(&project_root)?
    } else {
        WorkspaceSnapshot::new()
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
                    let fallback_env = build_unbounded_fallback_env(&params.env, &policy)?;
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
                        validate_verify_command(&verify_cmd, &policy, &project_root)?;
                        let mut verify_cancel = ctx.cancellation.clone();
                        match run_verify(&verify_cmd, &project_root, &policy, &mut verify_cancel)
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

struct OverlayCommandMetadata<'a> {
    mode: Option<OverlayMode>,
    status: OverlayStatus,
    disposition: Option<OverlayDisposition>,
    lower_consistency: Option<LowerConsistencyStatus>,
    cleanup: Option<&'a OverlayCleanupReport>,
}

struct ArtifactOverlayMetadata<'a> {
    status: OverlayStatus,
    disposition: Option<OverlayDisposition>,
    lower_consistency: Option<LowerConsistencyStatus>,
    artifact_manifest_status: &'a str,
    artifact_adjudication: &'a str,
    verify_layer: VerifyLayerStatus,
    cleanup: Option<&'a OverlayCleanupReport>,
    commit_report: Option<&'a ArtifactCommitReport>,
    rejected_kinds: &'a [RejectedArtifactKind],
    boundary_preflight: Option<&'a CommandBoundaryPreflight>,
    artifact_manifest_generated: bool,
}

async fn handle_diagnostic_overlay_run(
    params: RunParams,
    ctx: HandlerContext,
    method: &'static str,
    effective_timeout: u64,
    execution_plan: PreparedCommandExecution,
    project_root: String,
    policy: &lancet_authority::policy::command::CommandPolicy,
) -> Result<RunResult, CepError> {
    let lower_fingerprint = capture_lower_fingerprint(Path::new(&project_root))?;

    let Some(plan) = execution_plan.supervisor_plan else {
        if let Some(attempt) = execution_plan.boundary_attempt.as_ref() {
            record_command_boundary_attempt(&ctx, method, attempt).await;
        }
        record_overlay_command_metadata(
            &ctx,
            method,
            OverlayCommandMetadata {
                mode: Some(OverlayMode::Diagnostic),
                status: OverlayStatus::NotApplied,
                disposition: None,
                lower_consistency: Some(diagnostic_lower_consistency(
                    Path::new(&project_root),
                    &lower_fingerprint,
                )?),
                cleanup: None,
            },
        )
        .await;
        return run_diagnostic_unbounded_fallback(
            params,
            ctx,
            method,
            effective_timeout,
            project_root,
            policy,
        )
        .await;
    };

    run_diagnostic_overlay_supervised(
        execution_plan.argv,
        execution_plan.effective_env,
        params,
        ctx,
        method,
        effective_timeout,
        project_root,
        policy,
        lower_fingerprint,
        SupervisorExecution::Live { plan },
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn run_diagnostic_overlay_supervised(
    argv: Vec<String>,
    effective_env: BTreeMap<String, String>,
    params: RunParams,
    ctx: HandlerContext,
    method: &'static str,
    effective_timeout: u64,
    project_root: String,
    policy: &lancet_authority::policy::command::CommandPolicy,
    lower_fingerprint: LowerFingerprint,
    supervisor: SupervisorExecution,
) -> Result<RunResult, CepError> {
    let mut cancellation = ctx.cancellation.clone();
    let execute_config = ExecuteConfig {
        project_root: &project_root,
        limits: &ToolLimits::default(),
        argv: &argv,
        cwd: params.cwd.as_deref(),
        env: &effective_env,
        timeout_ms: Some(effective_timeout),
        cancellation: &mut cancellation,
        max_output_bytes: policy.max_output_bytes,
    };

    match execute_argv_supervised(execute_config, supervisor).await {
        Ok(output) => {
            record_command_boundary_attempt(&ctx, method, &output.setup_attempt).await;
            let lower_consistency =
                diagnostic_lower_consistency(Path::new(&project_root), &lower_fingerprint)?;
            record_overlay_command_metadata(
                &ctx,
                method,
                OverlayCommandMetadata {
                    mode: Some(OverlayMode::Diagnostic),
                    status: OverlayStatus::Applied,
                    disposition: Some(OverlayDisposition::Discarded),
                    lower_consistency: Some(lower_consistency),
                    cleanup: output.overlay_cleanup.as_ref(),
                },
            )
            .await;
            finish_diagnostic_success(
                &ctx,
                method,
                &project_root,
                output.output,
                output.stdout_meta,
                output.stderr_meta,
            )
            .await
        }
        Err(err) if should_run_unbounded_fallback(&ctx.command_isolation, &err) => {
            if let Some(attempt) = err.setup_attempt.as_ref() {
                record_command_boundary_attempt(&ctx, method, attempt).await;
            }
            let lower_consistency =
                diagnostic_lower_consistency(Path::new(&project_root), &lower_fingerprint)?;
            record_overlay_command_metadata(
                &ctx,
                method,
                OverlayCommandMetadata {
                    mode: Some(OverlayMode::Diagnostic),
                    status: OverlayStatus::NotApplied,
                    disposition: None,
                    lower_consistency: Some(lower_consistency),
                    cleanup: err.overlay_cleanup.as_ref(),
                },
            )
            .await;
            run_diagnostic_unbounded_fallback(
                params,
                ctx,
                method,
                effective_timeout,
                project_root,
                policy,
            )
            .await
        }
        Err(err) => {
            if let Some(attempt) = err.setup_attempt.as_ref() {
                record_command_boundary_attempt(&ctx, method, attempt).await;
            }
            let lower_consistency =
                diagnostic_lower_consistency(Path::new(&project_root), &lower_fingerprint)?;
            let status = if err
                .setup_attempt
                .as_ref()
                .is_some_and(|attempt| attempt.outcome.failed_closed)
            {
                OverlayStatus::SetupFailed
            } else {
                OverlayStatus::Applied
            };
            record_overlay_command_metadata(
                &ctx,
                method,
                OverlayCommandMetadata {
                    mode: Some(OverlayMode::Diagnostic),
                    status,
                    disposition: Some(OverlayDisposition::Discarded),
                    lower_consistency: Some(lower_consistency),
                    cleanup: err.overlay_cleanup.as_ref(),
                },
            )
            .await;
            let backend_invoked = supervised_error_backend_invoked(&err);
            finish_diagnostic_error(&ctx, method, err.error, backend_invoked).await
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn handle_artifact_overlay_run(
    params: RunParams,
    ctx: HandlerContext,
    method: &'static str,
    effective_timeout: u64,
    execution_plan: PreparedCommandExecution,
    active_record: EnvelopeRecord,
    project_root: String,
    policy: &lancet_authority::policy::command::CommandPolicy,
) -> Result<RunResult, CepError> {
    let lower_baseline = capture_lower_baseline(Path::new(&project_root))?;

    let Some(plan) = execution_plan.supervisor_plan else {
        if let Some(attempt) = execution_plan.boundary_attempt.as_ref() {
            record_command_boundary_attempt(&ctx, method, attempt).await;
        }
        return artifact_fallback_without_overlay(
            params,
            ctx,
            method,
            effective_timeout,
            active_record,
            project_root,
            policy,
            lower_baseline.entries,
        )
        .await;
    };

    run_artifact_overlay_supervised(
        params,
        ctx,
        method,
        effective_timeout,
        execution_plan.argv,
        execution_plan.effective_env,
        active_record,
        project_root,
        policy,
        lower_baseline,
        SupervisorExecution::Live { plan },
        None,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn run_artifact_overlay_supervised(
    params: RunParams,
    ctx: HandlerContext,
    method: &'static str,
    effective_timeout: u64,
    argv: Vec<String>,
    effective_env: BTreeMap<String, String>,
    active_record: EnvelopeRecord,
    project_root: String,
    policy: &lancet_authority::policy::command::CommandPolicy,
    lower_baseline: LowerBaseline,
    supervisor: SupervisorExecution,
    cleanup_override: Option<OverlayCleanupReport>,
) -> Result<RunResult, CepError> {
    let artifact_plan = supervisor.overlay_plan().cloned().ok_or_else(|| {
        CepError::internal_error("artifact overlay execution requested without overlay plan")
    })?;
    let execute_project_root =
        artifact_execute_project_root_for_supervisor(&project_root, &artifact_plan, &supervisor);
    let mut cancellation = ctx.cancellation.clone();
    let execute_config = ExecuteConfig {
        project_root: &execute_project_root,
        limits: &ToolLimits::default(),
        argv: &argv,
        cwd: params.cwd.as_deref(),
        env: &effective_env,
        timeout_ms: Some(effective_timeout),
        cancellation: &mut cancellation,
        max_output_bytes: policy.max_output_bytes,
    };

    let output = match execute_argv_supervised(execute_config, supervisor).await {
        Ok(output) => {
            record_command_boundary_attempt(&ctx, method, &output.setup_attempt).await;
            output
        }
        Err(err) if should_run_unbounded_fallback(&ctx.command_isolation, &err) => {
            if let Some(attempt) = err.setup_attempt.as_ref() {
                record_command_boundary_attempt(&ctx, method, attempt).await;
            }
            record_artifact_overlay_metadata(
                &ctx,
                method,
                ArtifactOverlayMetadata {
                    status: OverlayStatus::NotApplied,
                    disposition: None,
                    lower_consistency: Some(LowerConsistencyStatus::Unchanged),
                    artifact_manifest_status: "not_scanned",
                    artifact_adjudication: "skipped_no_overlay",
                    verify_layer: VerifyLayerStatus::NotRun,
                    cleanup: err.overlay_cleanup.as_ref(),
                    commit_report: None,
                    rejected_kinds: &[],
                    boundary_preflight: None,
                    artifact_manifest_generated: false,
                },
            )
            .await;
            return artifact_fallback_without_overlay(
                params,
                ctx,
                method,
                effective_timeout,
                active_record,
                project_root,
                policy,
                lower_baseline.entries,
            )
            .await;
        }
        Err(err) => {
            if let Some(attempt) = err.setup_attempt.as_ref() {
                record_command_boundary_attempt(&ctx, method, attempt).await;
            }
            let cleanup = cleanup_override.unwrap_or_else(|| {
                cleanup_artifact_overlay(&artifact_plan, OverlayDisposition::Denied, true)
            });
            record_artifact_overlay_metadata(
                &ctx,
                method,
                ArtifactOverlayMetadata {
                    status: artifact_status_from_error(&err),
                    disposition: Some(OverlayDisposition::Denied),
                    lower_consistency: Some(LowerConsistencyStatus::Unchanged),
                    artifact_manifest_status: "not_scanned",
                    artifact_adjudication: "denied_execution_failed",
                    verify_layer: VerifyLayerStatus::NotRun,
                    cleanup: Some(&cleanup),
                    commit_report: None,
                    rejected_kinds: &[],
                    boundary_preflight: None,
                    artifact_manifest_generated: false,
                },
            )
            .await;
            emit_denial_metadata(
                &ctx,
                "execution_error",
                supervised_error_backend_invoked(&err),
                method,
            )
            .await;
            return Err(err.error);
        }
    };
    let boundary_preflight =
        evaluate_command_boundary_attempt(&ctx.command_isolation, Some(&output.setup_attempt));

    let exit_code = output.output.status.code().unwrap_or(-1);
    record_command_execution_for_artifact(
        &ctx,
        method,
        &project_root,
        &active_record.envelope_id,
        exit_code,
        &output.stdout_meta,
        &output.stderr_meta,
    )
    .await;

    let scan = scan_overlay_upper(&artifact_plan.paths.upper, &lower_baseline)?;
    let write_patterns = extract_write_path_patterns(&active_record.envelope.capabilities);
    let authorized_region = ctx
        .authorized_region
        .get()
        .unwrap_or_else(lancet_authority::effect::region::AuthorizedEffectRegion::empty);
    let adjudication = match adjudicate_artifact_manifest(
        &scan.manifest,
        &scan.rejected_kinds,
        &project_root,
        &write_patterns,
        &authorized_region,
    ) {
        Ok(report) => report,
        Err(failure) => {
            record_manifest_effects(&ctx, &failure.report);
            if !failure.report.denied_paths.is_empty() {
                emit_artifact_replan_required(
                    &ctx,
                    method,
                    &active_record.envelope_id,
                    &failure.report.denied_paths,
                    &scan.manifest.paths(),
                )
                .await;
            }
            let cleanup = cleanup_override.unwrap_or_else(|| {
                cleanup_artifact_overlay(&artifact_plan, failure.report.overlay_disposition, true)
            });
            record_artifact_overlay_metadata(
                &ctx,
                method,
                ArtifactOverlayMetadata {
                    status: OverlayStatus::Applied,
                    disposition: Some(failure.report.overlay_disposition),
                    lower_consistency: Some(LowerConsistencyStatus::Unchanged),
                    artifact_manifest_status: failure.report.artifact_manifest_status.label(),
                    artifact_adjudication: &failure.report.artifact_adjudication,
                    verify_layer: VerifyLayerStatus::NotRun,
                    cleanup: Some(&cleanup),
                    commit_report: None,
                    rejected_kinds: &failure.report.rejected_kinds,
                    boundary_preflight: Some(&boundary_preflight),
                    artifact_manifest_generated: true,
                },
            )
            .await;
            return Err(failure.into_error());
        }
    };
    record_manifest_effects(&ctx, &adjudication);

    let verify_report = match run_artifact_verify_if_configured(
        &ctx,
        method,
        &active_record,
        &artifact_plan,
        &scan.manifest,
        policy,
    )
    .await
    {
        Ok(report) => report,
        Err((err, report)) => {
            let cleanup = cleanup_override.unwrap_or_else(|| {
                cleanup_artifact_overlay(&artifact_plan, OverlayDisposition::Denied, true)
            });
            record_artifact_overlay_metadata(
                &ctx,
                method,
                ArtifactOverlayMetadata {
                    status: OverlayStatus::Applied,
                    disposition: Some(OverlayDisposition::Denied),
                    lower_consistency: Some(LowerConsistencyStatus::Unchanged),
                    artifact_manifest_status: adjudication.artifact_manifest_status.label(),
                    artifact_adjudication: "denied_verify_failed",
                    verify_layer: report.verify_layer,
                    cleanup: Some(&cleanup),
                    commit_report: None,
                    rejected_kinds: &[],
                    boundary_preflight: Some(&boundary_preflight),
                    artifact_manifest_generated: true,
                },
            )
            .await;
            ctx.events
                .emit(
                    "cep.verify_failed",
                    json!({
                        "method": method,
                        "envelope_id": &active_record.envelope_id,
                        "verify_exit_code": -1,
                        "diff_retained": false,
                    }),
                )
                .await;
            return Err(err);
        }
    };

    let prepared = prepare_artifact_commit_set(
        scan.manifest.clone(),
        &artifact_plan.paths.upper,
        &lower_baseline,
    )?;
    let artifact_cleanup = cleanup_override.unwrap_or_else(|| {
        cleanup_artifact_overlay(&artifact_plan, OverlayDisposition::Committed, true)
    });
    let commit_report = match commit_artifact_manifest(
        Path::new(&project_root),
        prepared,
        &artifact_cleanup,
        &verify_report,
    ) {
        Ok(report) => report,
        Err(failure) => {
            record_artifact_overlay_metadata(
                &ctx,
                method,
                ArtifactOverlayMetadata {
                    status: OverlayStatus::Applied,
                    disposition: Some(failure.report.overlay_disposition),
                    lower_consistency: lower_consistency_from_commit_report(&failure.report),
                    artifact_manifest_status: adjudication.artifact_manifest_status.label(),
                    artifact_adjudication: &failure.report.artifact_adjudication,
                    verify_layer: verify_report.verify_layer,
                    cleanup: Some(&artifact_cleanup),
                    commit_report: Some(&failure.report),
                    rejected_kinds: &[],
                    boundary_preflight: Some(&boundary_preflight),
                    artifact_manifest_generated: true,
                },
            )
            .await;
            return Err(failure.into_error());
        }
    };

    record_artifact_overlay_metadata(
        &ctx,
        method,
        ArtifactOverlayMetadata {
            status: OverlayStatus::Applied,
            disposition: Some(commit_report.overlay_disposition),
            lower_consistency: lower_consistency_from_commit_report(&commit_report),
            artifact_manifest_status: adjudication.artifact_manifest_status.label(),
            artifact_adjudication: &commit_report.artifact_adjudication,
            verify_layer: verify_report.verify_layer,
            cleanup: Some(&artifact_cleanup),
            commit_report: Some(&commit_report),
            rejected_kinds: &[],
            boundary_preflight: Some(&boundary_preflight),
            artifact_manifest_generated: true,
        },
    )
    .await;

    emit_success_metadata(
        &ctx,
        CommandSuccessAudit {
            classification: &EffectiveImpact::WorkspaceMutation,
            envelope_id: Some(&active_record.envelope_id),
            exit_code,
            stdout_meta: &output.stdout_meta,
            stderr_meta: &output.stderr_meta,
            method,
            observation_metadata: &command_observation_metadata(
                &EffectiveImpact::WorkspaceMutation,
            ),
        },
    )
    .await;

    Ok(RunResult {
        exit_code,
        stdout: String::from_utf8_lossy(&output.output.stdout).to_string(),
        stderr: String::from_utf8_lossy(&output.output.stderr).to_string(),
        stdout_truncated: output.stdout_meta.truncated.then_some(true),
        stderr_truncated: output.stderr_meta.truncated.then_some(true),
        stdout_bytes: Some(output.stdout_meta.total_bytes),
        stderr_bytes: Some(output.stderr_meta.total_bytes),
    })
}

fn artifact_execute_project_root_for_supervisor(
    project_root: &str,
    artifact_plan: &OverlayPlan,
    supervisor: &SupervisorExecution,
) -> String {
    #[cfg(not(test))]
    let _ = supervisor;
    #[cfg(test)]
    if matches!(supervisor, SupervisorExecution::Fixed { .. }) {
        let _ = std::fs::create_dir_all(&artifact_plan.paths.upper);
        return artifact_plan.paths.upper.to_string_lossy().into_owned();
    }
    let _ = artifact_plan;
    project_root.to_string()
}

#[allow(clippy::too_many_arguments)]
async fn artifact_fallback_without_overlay(
    params: RunParams,
    ctx: HandlerContext,
    method: &'static str,
    effective_timeout: u64,
    active_record: EnvelopeRecord,
    project_root: String,
    policy: &lancet_authority::policy::command::CommandPolicy,
    pre_snapshot: WorkspaceSnapshot,
) -> Result<RunResult, CepError> {
    record_artifact_overlay_metadata(
        &ctx,
        method,
        ArtifactOverlayMetadata {
            status: match ctx.command_isolation.mode {
                CommandIsolationMode::Off => OverlayStatus::NotRequested,
                CommandIsolationMode::Audit | CommandIsolationMode::Enforce => {
                    OverlayStatus::NotApplied
                }
            },
            disposition: None,
            lower_consistency: None,
            artifact_manifest_status: "not_scanned",
            artifact_adjudication: "skipped_no_overlay",
            verify_layer: VerifyLayerStatus::NotRun,
            cleanup: None,
            commit_report: None,
            rejected_kinds: &[],
            boundary_preflight: None,
            artifact_manifest_generated: false,
        },
    )
    .await;

    let fallback_env = if ctx.command_isolation.mode == CommandIsolationMode::Off {
        build_effective_env(&params.env, policy)?
    } else {
        build_unbounded_fallback_env(&params.env, policy)?
    };
    let mut cancellation = ctx.cancellation.clone();
    let execute_config = ExecuteConfig {
        project_root: &project_root,
        limits: &ToolLimits::default(),
        argv: &params.argv,
        cwd: params.cwd.as_deref(),
        env: &fallback_env,
        timeout_ms: Some(effective_timeout),
        cancellation: &mut cancellation,
        max_output_bytes: policy.max_output_bytes,
    };
    match execute_argv(execute_config).await {
        Ok((output, stdout_meta, stderr_meta)) => {
            let exit_code = output.status.code().unwrap_or(-1);
            record_command_execution_for_artifact(
                &ctx,
                method,
                &project_root,
                &active_record.envelope_id,
                exit_code,
                &stdout_meta,
                &stderr_meta,
            )
            .await;
            let diff = record_workspace_mutation_diff(
                &ctx,
                &active_record,
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
            if let Some(verify_cmd) = extract_verify_command(&active_record.envelope) {
                validate_verify_command(&verify_cmd, policy, &project_root)?;
                let mut verify_cancel = ctx.cancellation.clone();
                match run_verify(&verify_cmd, &project_root, policy, &mut verify_cancel).await {
                    Ok(outcome) if !outcome.success => {
                        ctx.events
                            .emit(
                                "cep.verify_failed",
                                json!({
                                    "method": method,
                                    "envelope_id": &active_record.envelope_id,
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
                    Err(err) => return Err(err),
                }
            }
            let observation_metadata =
                command_observation_metadata(&EffectiveImpact::WorkspaceMutation);
            emit_success_metadata(
                &ctx,
                CommandSuccessAudit {
                    classification: &EffectiveImpact::WorkspaceMutation,
                    envelope_id: Some(&active_record.envelope_id),
                    exit_code,
                    stdout_meta: &stdout_meta,
                    stderr_meta: &stderr_meta,
                    method,
                    observation_metadata: &observation_metadata,
                },
            )
            .await;
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
        Err(error) => {
            let diff = record_workspace_mutation_diff(
                &ctx,
                &active_record,
                &project_root,
                &pre_snapshot,
                method,
            )
            .await?;
            ctx.record_durable_metadata(json!({
                "classification": command_classification_label(&EffectiveImpact::WorkspaceMutation),
                "envelope_id": &active_record.envelope_id,
                "error_code": error.code.as_i32(),
                "backend_invoked": true,
                "policy_enforced": true,
            }));
            ctx.record_durable_metadata(command_observation_metadata(
                &EffectiveImpact::WorkspaceMutation,
            ));
            emit_denial_metadata(&ctx, "execution_error", true, method).await;
            if !diff.out_of_scope_paths.is_empty() {
                return Err(out_of_scope_changed_paths_error(
                    diff.out_of_scope_paths,
                    method,
                ));
            }
            Err(error)
        }
    }
}

async fn record_command_execution_for_artifact(
    ctx: &HandlerContext,
    method: &'static str,
    project_root: &str,
    envelope_id: &str,
    exit_code: i32,
    stdout_meta: &BoundedStreamOutput,
    stderr_meta: &BoundedStreamOutput,
) {
    let observation_metadata = command_observation_metadata(&EffectiveImpact::WorkspaceMutation);
    let execution_metadata = command_execution_metadata(CommandExecutionMetadata {
        classification: &EffectiveImpact::WorkspaceMutation,
        envelope_id: Some(envelope_id),
        exit_code,
        stdout_meta,
        stderr_meta,
        method,
        observation_metadata: &observation_metadata,
    });
    ctx.record_effect(ObservedEffect {
        kind: ImpactKind::CommandExecution,
        scope: ObservedScope::Root(project_root.to_string()),
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
    ctx.record_durable_metadata(observation_metadata);
}

async fn run_artifact_verify_if_configured(
    ctx: &HandlerContext,
    method: &'static str,
    active_record: &EnvelopeRecord,
    artifact_plan: &OverlayPlan,
    manifest: &crate::executor::supervisor::artifact::ArtifactManifest,
    policy: &lancet_authority::policy::command::CommandPolicy,
) -> Result<VerifyDiscardReport, (CepError, VerifyDiscardReport)> {
    let Some(verify_cmd) = extract_verify_command(&active_record.envelope) else {
        return Ok(VerifyDiscardReport::not_run());
    };
    let candidate = prepare_candidate_artifact_view(artifact_plan, manifest);
    #[cfg(test)]
    if let Some(relative) = take_test_artifact_verify_write() {
        validate_verify_command(&verify_cmd, policy, &candidate.root.to_string_lossy())
            .map_err(|err| (err, VerifyDiscardReport::not_run()))?;
        if !relative.is_empty() && (relative.contains("..") || Path::new(&relative).is_absolute()) {
            return Err((
                CepError::internal_error("test artifact verify write path must be relative"),
                VerifyDiscardReport::not_run(),
            ));
        }
        if !relative.is_empty() {
            std::fs::write(artifact_plan.paths.upper.join(&relative), "verify-only").map_err(
                |err| {
                    (
                        CepError::internal_error(format!(
                            "failed to write test verify artifact: {err}"
                        )),
                        VerifyDiscardReport::not_run(),
                    )
                },
            )?;
        }
        ctx.record_effect(ObservedEffect {
            kind: ImpactKind::CommandExecution,
            scope: ObservedScope::Root(candidate.root.to_string_lossy().to_string()),
            required_capability: RequiredCapability::RunCommand,
            source: EffectSource::Verify,
        });
        return Ok(VerifyDiscardReport {
            verify_layer: VerifyLayerStatus::Discarded,
            cleanup_outcome: Some(CleanupOutcome::Succeeded),
            failure_kind: None,
            path_label: None,
            reason: None,
            child_cleanup_succeeded: true,
            contaminated_paths: if relative.is_empty() {
                Vec::new()
            } else {
                vec![relative]
            },
        });
    }
    let mut verify_cancel = ctx.cancellation.clone();
    match run_verify_on_candidate_view(
        &verify_cmd,
        &candidate,
        policy,
        &mut verify_cancel,
        &format!("{}-{}-verify", ctx.turn_id, ctx.call_id),
    )
    .await
    {
        Ok(outcome) => {
            ctx.record_effect(ObservedEffect {
                kind: ImpactKind::CommandExecution,
                scope: ObservedScope::Root(candidate.root.to_string_lossy().to_string()),
                required_capability: RequiredCapability::RunCommand,
                source: EffectSource::Verify,
            });
            Ok(outcome.discard)
        }
        Err(err) => {
            let report = if err.code == CepErrorCode::VerifyFailed {
                VerifyDiscardReport {
                    verify_layer: VerifyLayerStatus::Discarded,
                    cleanup_outcome: Some(CleanupOutcome::Succeeded),
                    failure_kind: None,
                    path_label: None,
                    reason: None,
                    child_cleanup_succeeded: true,
                    contaminated_paths: Vec::new(),
                }
            } else {
                VerifyDiscardReport {
                    verify_layer: VerifyLayerStatus::CleanupFailed,
                    cleanup_outcome: Some(CleanupOutcome::Failed),
                    failure_kind: None,
                    path_label: None,
                    reason: Some(err.message.clone()),
                    child_cleanup_succeeded: true,
                    contaminated_paths: Vec::new(),
                }
            };
            let _ = method;
            Err((err, report))
        }
    }
}

fn cleanup_artifact_overlay(
    artifact_plan: &OverlayPlan,
    disposition: OverlayDisposition,
    child_cleanup_succeeded: bool,
) -> OverlayCleanupReport {
    let mut syscalls = LiveSupervisorSyscalls::default();
    cleanup_overlay(
        artifact_plan,
        disposition,
        child_cleanup_succeeded,
        &mut syscalls,
    )
}

fn artifact_status_from_error(err: &SupervisedCommandError) -> OverlayStatus {
    if err
        .setup_attempt
        .as_ref()
        .is_some_and(|attempt| attempt.outcome.failed_closed)
    {
        OverlayStatus::SetupFailed
    } else {
        OverlayStatus::Applied
    }
}

fn record_manifest_effects(ctx: &HandlerContext, report: &ArtifactAdjudicationReport) {
    for effect in &report.observed_effects {
        ctx.record_effect(effect.clone());
    }
}

async fn emit_artifact_replan_required(
    ctx: &HandlerContext,
    method: &'static str,
    envelope_id: &str,
    denied_paths: &[String],
    manifest_paths: &[String],
) {
    ctx.events
        .emit(
            "cep.replan_required",
            json!({
                "method": method,
                "active_envelope_id": envelope_id,
                "out_of_scope_paths": denied_paths,
                "changed_paths": manifest_paths,
            }),
        )
        .await;
}

fn lower_consistency_from_commit_report(
    report: &ArtifactCommitReport,
) -> Option<LowerConsistencyStatus> {
    match report.lower_consistency.as_str() {
        "unchanged" => Some(LowerConsistencyStatus::Unchanged),
        "drift_detected" => Some(LowerConsistencyStatus::DriftDetected),
        _ => None,
    }
}

#[cfg(test)]
fn set_test_artifact_verify_write(path: Option<&str>) {
    *TEST_ARTIFACT_VERIFY_WRITE.lock().unwrap() = path.map(str::to_string);
}

#[cfg(test)]
fn take_test_artifact_verify_write() -> Option<String> {
    TEST_ARTIFACT_VERIFY_WRITE.lock().unwrap().take()
}

async fn run_diagnostic_unbounded_fallback(
    params: RunParams,
    ctx: HandlerContext,
    method: &'static str,
    effective_timeout: u64,
    project_root: String,
    policy: &lancet_authority::policy::command::CommandPolicy,
) -> Result<RunResult, CepError> {
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
    match execute_argv(fallback_config).await {
        Ok((output, stdout_meta, stderr_meta)) => {
            finish_diagnostic_success(
                &ctx,
                method,
                &project_root,
                output,
                stdout_meta,
                stderr_meta,
            )
            .await
        }
        Err(error) => finish_diagnostic_error(&ctx, method, error, true).await,
    }
}

async fn finish_diagnostic_success(
    ctx: &HandlerContext,
    method: &'static str,
    project_root: &str,
    output: std::process::Output,
    stdout_meta: BoundedStreamOutput,
    stderr_meta: BoundedStreamOutput,
) -> Result<RunResult, CepError> {
    let exit_code = output.status.code().unwrap_or(-1);
    let effective_impact = EffectiveImpact::Diagnostic;
    let observation_metadata = command_observation_metadata(&effective_impact);
    let execution_metadata = command_execution_metadata(CommandExecutionMetadata {
        classification: &effective_impact,
        envelope_id: None,
        exit_code,
        stdout_meta: &stdout_meta,
        stderr_meta: &stderr_meta,
        method,
        observation_metadata: &observation_metadata,
    });

    ctx.record_effect(ObservedEffect {
        kind: ImpactKind::CommandExecution,
        scope: ObservedScope::Root(project_root.to_string()),
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
    ctx.record_durable_metadata(observation_metadata.clone());
    emit_success_metadata(
        ctx,
        CommandSuccessAudit {
            classification: &effective_impact,
            envelope_id: None,
            exit_code,
            stdout_meta: &stdout_meta,
            stderr_meta: &stderr_meta,
            method,
            observation_metadata: &observation_metadata,
        },
    )
    .await;

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

async fn finish_diagnostic_error(
    ctx: &HandlerContext,
    method: &'static str,
    error: CepError,
    backend_invoked: bool,
) -> Result<RunResult, CepError> {
    let effective_impact = EffectiveImpact::Diagnostic;
    ctx.record_durable_metadata(json!({
        "classification": command_classification_label(&effective_impact),
        "envelope_id": serde_json::Value::Null,
        "error_code": error.code.as_i32(),
        "backend_invoked": backend_invoked,
        "policy_enforced": true,
    }));
    ctx.record_durable_metadata(command_observation_metadata(&effective_impact));
    emit_denial_metadata(ctx, "execution_error", backend_invoked, method).await;
    Err(error)
}

fn overlay_requested_for_command(ctx: &HandlerContext) -> bool {
    matches!(
        ctx.command_isolation.mode,
        CommandIsolationMode::Enforce | CommandIsolationMode::Audit
    )
}

fn overlay_status_for_unapplied_diagnostic(ctx: &HandlerContext) -> OverlayStatus {
    match ctx.command_isolation.mode {
        CommandIsolationMode::Off => OverlayStatus::NotRequested,
        CommandIsolationMode::Audit => OverlayStatus::NotApplied,
        CommandIsolationMode::Enforce => OverlayStatus::SetupFailed,
    }
}

fn overlay_mode_for_effect(effective_impact: &EffectiveImpact) -> Option<OverlayMode> {
    match effective_impact {
        EffectiveImpact::Diagnostic => Some(OverlayMode::Diagnostic),
        EffectiveImpact::WorkspaceMutation => Some(OverlayMode::Artifact),
        EffectiveImpact::HardDenied => None,
    }
}

fn diagnostic_lower_consistency(
    project_root: &Path,
    baseline: &LowerFingerprint,
) -> Result<LowerConsistencyStatus, CepError> {
    let current = capture_lower_fingerprint(project_root)?;
    if current == *baseline {
        Ok(LowerConsistencyStatus::Unchanged)
    } else {
        Ok(LowerConsistencyStatus::DriftDetected)
    }
}

async fn record_overlay_command_metadata(
    ctx: &HandlerContext,
    method: &'static str,
    metadata: OverlayCommandMetadata<'_>,
) {
    let json = overlay_command_metadata_json(method, metadata);
    ctx.record_durable_metadata(json.clone());
    ctx.events
        .emit(
            "cep.command_metadata",
            json!({ "method": method, "metadata": json }),
        )
        .await;
}

async fn record_artifact_overlay_metadata(
    ctx: &HandlerContext,
    method: &'static str,
    metadata: ArtifactOverlayMetadata<'_>,
) {
    let json = artifact_overlay_metadata_json(method, metadata);
    ctx.record_durable_metadata(json.clone());
    ctx.events
        .emit(
            "cep.command_metadata",
            json!({ "method": method, "metadata": json }),
        )
        .await;
}

fn artifact_overlay_metadata_json(
    method: &'static str,
    metadata: ArtifactOverlayMetadata<'_>,
) -> serde_json::Value {
    let cleanup_outcome = metadata
        .cleanup
        .map(|cleanup| cleanup.outcome.label())
        .unwrap_or_else(|| match metadata.status {
            OverlayStatus::NotRequested => "not_requested",
            OverlayStatus::NotApplied => "not_applied",
            OverlayStatus::SetupFailed | OverlayStatus::Applied => "not_reported",
        });
    let commit_outcome = metadata
        .commit_report
        .map(|report| report.commit_outcome.label())
        .unwrap_or(CommitOutcome::NotStarted.label());
    let applied_entries = metadata
        .commit_report
        .map(|report| {
            report
                .applied_entries
                .iter()
                .map(|entry| {
                    json!({
                        "path": entry.path,
                        "kind": entry.kind.label(),
                        "operation": entry.operation.label(),
                    })
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let failed_entry = metadata.commit_report.and_then(|report| {
        report.failed_entry.as_ref().map(|entry| {
            json!({
                "path": entry.path,
                "kind": entry.kind.label(),
                "error": entry.error,
            })
        })
    });
    let mut value = json!({
        "method": method,
        "overlay_mode": overlay_mode_label(OverlayMode::Artifact),
        "overlay_status": metadata.status.label(),
        "overlay_disposition": metadata
            .disposition
            .map(|disposition| disposition.label())
            .unwrap_or_else(|| metadata.status.label()),
        "lower_consistency": metadata
            .lower_consistency
            .map(lower_consistency_label)
            .or_else(|| metadata.commit_report.map(|report| report.lower_consistency.as_str()))
            .unwrap_or("not_checked"),
        "artifact_manifest_status": metadata.artifact_manifest_status,
        "artifact_adjudication": metadata.artifact_adjudication,
        "verify_layer": metadata.verify_layer.label(),
        "cleanup_outcome": cleanup_outcome,
        "cleanup_failure_kind": metadata
            .cleanup
            .and_then(|cleanup| cleanup.failure_kind.map(|kind| kind.label())),
        "child_cleanup_succeeded": metadata
            .cleanup
            .map(|cleanup| cleanup.child_cleanup_succeeded),
        "commit_outcome": commit_outcome,
        "rejected_kinds": rejected_kinds_json(metadata.rejected_kinds),
        "applied_entries": applied_entries,
        "failed_entry": failed_entry,
        "lower_changed_paths": metadata
            .commit_report
            .map(|report| report.changed_paths.clone())
            .unwrap_or_default(),
        "commit_baseline_validation": metadata
            .commit_report
            .map(|report| report.commit_baseline_validation.as_str()),
        "commit_baseline_failure_reason": metadata
            .commit_report
            .and_then(|report| report.commit_baseline_failure_reason.as_deref()),
        "commit_baseline_details": metadata
            .commit_report
            .map(|report| report.commit_baseline_details.clone())
            .unwrap_or_else(|| json!({})),
    });
    if let Some(preflight) = metadata.boundary_preflight {
        value["effective_boundary"] = effective_boundary_metadata_json_with_observation(
            &preflight.effective_boundary,
            metadata.status == OverlayStatus::Applied,
            metadata.artifact_manifest_generated,
        );
    }
    value
}

fn rejected_kinds_json(rejected: &[RejectedArtifactKind]) -> Vec<serde_json::Value> {
    rejected
        .iter()
        .map(|kind| {
            json!({
                "path": kind.path,
                "kind": kind.kind.label(),
                "reason": kind.reason,
            })
        })
        .collect()
}

fn overlay_command_metadata_json(
    method: &'static str,
    metadata: OverlayCommandMetadata<'_>,
) -> serde_json::Value {
    let cleanup_outcome = metadata
        .cleanup
        .map(|cleanup| cleanup.outcome.label())
        .unwrap_or_else(|| match metadata.status {
            OverlayStatus::NotRequested => "not_requested",
            OverlayStatus::NotApplied => "not_applied",
            OverlayStatus::SetupFailed | OverlayStatus::Applied => "not_reported",
        });
    let cleanup_failure_kind = metadata
        .cleanup
        .and_then(|cleanup| cleanup.failure_kind.map(|kind| kind.label()));
    let child_cleanup_succeeded = metadata
        .cleanup
        .map(|cleanup| cleanup.child_cleanup_succeeded);
    json!({
        "method": method,
        "overlay_mode": metadata.mode.map(overlay_mode_label).unwrap_or("none"),
        "overlay_status": metadata.status.label(),
        "overlay_disposition": metadata
            .disposition
            .map(|disposition| disposition.label())
            .unwrap_or_else(|| metadata.status.label()),
        "lower_consistency": metadata
            .lower_consistency
            .map(lower_consistency_label)
            .unwrap_or("not_checked"),
        "cleanup_outcome": cleanup_outcome,
        "cleanup_failure_kind": cleanup_failure_kind,
        "child_cleanup_succeeded": child_cleanup_succeeded,
    })
}

fn overlay_mode_label(mode: OverlayMode) -> &'static str {
    match mode {
        OverlayMode::Diagnostic => "diagnostic_discard",
        OverlayMode::Artifact => "artifact",
        OverlayMode::CandidateVerify => "candidate_verify",
    }
}

fn lower_consistency_label(status: LowerConsistencyStatus) -> &'static str {
    match status {
        LowerConsistencyStatus::Unchanged => "unchanged",
        LowerConsistencyStatus::DriftDetected => "drift_detected",
    }
}

fn prepare_command_execution_plan(
    ctx: &HandlerContext,
    policy: &lancet_authority::policy::command::CommandPolicy,
    params: &RunParams,
    project_root: &str,
    effective_impact: &EffectiveImpact,
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
                Ok(mut plan) => {
                    if let Some(overlay_mode) = overlay_mode_for_effect(effective_impact) {
                        let overlay_plan = OverlayPlan::deterministic(
                            Path::new(project_root),
                            overlay_mode,
                            &format!("{}-{}", ctx.turn_id, ctx.call_id),
                        )
                        .map_err(|err| {
                            CepError::internal_error(format!(
                                "failed to plan {} overlay {}: {}",
                                overlay_mode.label(),
                                err.field,
                                err.reason
                            ))
                        })?;
                        plan = plan.with_overlay_plan(overlay_plan);
                    }
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
    pre_snapshot: &WorkspaceSnapshot,
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
        }),
        EffectiveImpact::Diagnostic => json!({
            "effect_observation": "partial",
            "observed_effects": ["command_execution"],
        }),
        EffectiveImpact::HardDenied => json!({
            "effect_observation": "none",
            "observed_effects": [],
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
