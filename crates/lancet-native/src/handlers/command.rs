// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Native exec.command_run CEP handler with policy enforcement.
//!
//! This is the only shell execution path from the agent.
//! WorkspaceMutation commands capture a pre-execution workspace snapshot, compute
//! changed paths after execution, and check them against WritePath scope before
//! running any configured verify command.

use crate::enforcement::guard_l1::evaluate_and_enforce_policy;
use crate::enforcement::preflight::{CommandPreflight, command_preflight_metadata};
use crate::enforcement::verify::{
    VerifyCommandSpec, run_verify_with_backend, verify_failed_error,
};
use crate::execution_backend::{
    ExecRequest, ExecutionBackendError, PathFacts, PathIntent, WorkspacePath,
};
use crate::executor::process::BoundedStreamOutput;
use crate::server::registry::{HandlerContext, MethodRegistry};
use lancet_audit::{RejectionRecord, RejectionStage};
use lancet_authority::effect::ledger::{EffectSource, ObservedEffect, ObservedScope};
use lancet_authority::guard::{DirectPathFacts, DirectPathKind, validate_direct_path_facts};
use lancet_authority::policy::command::{
    EffectiveImpact, build_allowed_params_env, classify_command,
};
use lancet_catalog::CepMethod;
use lancet_catalog::method::RequiredCapability;
use lancet_catalog::methods::command::{Run, RunParams, RunResult};
use lancet_protocol::effect_types::ImpactKind;
use lancet_protocol::error::{CepErrorData, GuardErrorData};
use lancet_protocol::{CepError, CepErrorCode, Retriability};
use serde_json::json;

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

    execute_direct_command(params, ctx, method, &policy).await
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

async fn execute_direct_command(
    params: RunParams,
    ctx: HandlerContext,
    method: &'static str,
    policy: &lancet_authority::policy::command::CommandPolicy,
) -> Result<RunResult, CepError> {
    let outcome = execute_direct_command_inner(params, &ctx, method, policy).await;
    ctx.record_routing_metadata();
    outcome
}

async fn execute_direct_command_inner(
    params: RunParams,
    ctx: &HandlerContext,
    method: &'static str,
    policy: &lancet_authority::policy::command::CommandPolicy,
) -> Result<RunResult, CepError> {
    let effective_impact = classify_command(&params.argv, params.declared_impact.clone(), policy)?;

    if effective_impact == EffectiveImpact::HardDenied {
        let cmd = params.argv.first().map(|s| s.as_str()).unwrap_or("<empty>");
        record_command_preflight(
            ctx,
            method,
            &params.argv,
            params.declared_impact.as_ref(),
            &effective_impact,
            None,
            false,
            false,
            false,
        );
        emit_denial_metadata(ctx, "hard_denied", false, method).await;
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

    record_command_preflight(
        ctx,
        method,
        &params.argv,
        params.declared_impact.as_ref(),
        &effective_impact,
        None,
        false,
        false,
        false,
    );

    if ctx.cancellation.is_cancelled() {
        return Err(direct_command_cancelled_error(method, &params.argv));
    }

    let requested_timeout = params.timeout_ms.unwrap_or(policy.default_timeout_ms);
    let effective_timeout = std::cmp::min(requested_timeout, policy.max_timeout_ms);
    let effective_env = build_allowed_params_env(&params.env, policy)?;
    let cwd = resolve_direct_command_cwd(params.cwd.as_deref())?;

    let execution = ctx
        .execution
        .as_ref()
        .ok_or_else(|| direct_backend_unavailable_error(method))?;

    ctx.mark_adapter_preflight_invoked();
    let path_facts = execution
        .backend
        .resolve_path(&cwd, PathIntent::ExecCwd)
        .await
        .map_err(|err| {
            record_direct_backend_rejection(ctx, execution);
            direct_command_backend_error(method, &params.argv, err)
        })?;

    let authorized_region = ctx
        .authorized_region
        .get()
        .unwrap_or_else(lancet_authority::effect::region::AuthorizedEffectRegion::empty);
    if let Err(err) = validate_direct_command_cwd(cwd.as_str(), &path_facts, &authorized_region) {
        ctx.record_rejection(RejectionRecord {
            rejection_stage: RejectionStage::AdapterPreflight,
            denial_source: "guard_l1".into(),
        });
        emit_denial_metadata(ctx, "guard_l1", false, method).await;
        return Err(err);
    }

    if ctx.cancellation.is_cancelled() {
        return Err(direct_command_cancelled_error(method, &params.argv));
    }

    let request = ExecRequest::new(params.argv.clone(), effective_env, effective_timeout, cwd)
        .map_err(|_| {
            CepError::new(
                CepErrorCode::SchemaMismatch,
                "exec.command_run requires a non-empty argv",
                CepErrorData::Validation(lancet_protocol::error::ValidationErrorData {
                    field: "argv".into(),
                    reason: "must not be empty".into(),
                }),
                Retriability::Permanent,
            )
        })?;

    ctx.mark_backend_invoked();
    let output = execution.backend.exec(&request).await.map_err(|err| {
        record_direct_backend_rejection(ctx, execution);
        direct_command_backend_error(method, &params.argv, err)
    })?;

    record_direct_command_effects(ctx, &effective_impact, &authorized_region);

    if effective_impact == EffectiveImpact::WorkspaceMutation {
        let changed = execution.backend.changed_paths().await.map_err(|err| {
            record_direct_backend_rejection(ctx, execution);
            direct_command_backend_error(method, &params.argv, err)
        })?;

        let has_write_region = !authorized_region.write_paths.is_empty()
            || !authorized_region.write_path_prefixes.is_empty();
        let mut out_of_scope_paths = Vec::new();
        for path in &changed {
            let effect = ObservedEffect {
                kind: ImpactKind::WorkspaceMutation,
                scope: ObservedScope::Path(std::path::PathBuf::from(path.as_str())),
                required_capability: RequiredCapability::RunCommand,
                source: EffectSource::HandlerBackend,
            };
            ctx.record_effect(effect.clone());
            if has_write_region && !authorized_region.contains(&effect) {
                out_of_scope_paths.push(path.as_str().to_string());
            }
        }

        if !out_of_scope_paths.is_empty() {
            let changed_path_strings: Vec<String> =
                changed.iter().map(|p| p.as_str().to_string()).collect();
            ctx.record_durable_metadata(json!({
                "classification": "workspace_mutation",
                "out_of_scope_paths": &out_of_scope_paths,
                "changed_paths": &changed_path_strings,
                "backend_invoked": true,
                "policy_enforced": true,
            }));
            return Err(out_of_scope_changed_paths_error(out_of_scope_paths, method));
        }
    }

    if effective_impact == EffectiveImpact::WorkspaceMutation
        && let Some(verify_config) = ctx.direct_verify.as_ref()
    {
        let mut verify_cancel = ctx.cancellation.clone();
        match run_verify_with_backend(
            verify_config,
            execution,
            policy,
            &mut verify_cancel,
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
                let spec = verify_spec_from_direct(verify_config);
                return Err(verify_failed_error(&spec, &outcome));
            }
            Ok(_) => {
                ctx.record_effect(ObservedEffect {
                    kind: ImpactKind::CommandExecution,
                    scope: direct_command_scope(&authorized_region),
                    required_capability: RequiredCapability::RunCommand,
                    source: EffectSource::Verify,
                });
            }
            Err(err) => {
                record_direct_backend_rejection(ctx, execution);
                return Err(err);
            }
        }
    }

    let stdout_meta = bounded_stream_meta(&output.stdout, policy.max_output_bytes);
    let stderr_meta = bounded_stream_meta(&output.stderr, policy.max_output_bytes);
    let exit_code = output.exit_code;
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
    ctx.record_durable_metadata(execution_metadata.clone());
    ctx.events
        .emit(
            "cep.command_metadata",
            json!({ "method": method, "metadata": execution_metadata }),
        )
        .await;

    Ok(RunResult {
        exit_code,
        stdout: output.stdout,
        stderr: output.stderr,
        stdout_truncated: stdout_meta.truncated.then_some(true),
        stderr_truncated: stderr_meta.truncated.then_some(true),
        stdout_bytes: Some(stdout_meta.total_bytes),
        stderr_bytes: Some(stderr_meta.total_bytes),
    })
}

fn resolve_direct_command_cwd(raw: Option<&str>) -> Result<WorkspacePath, CepError> {
    match raw {
        Some(path) => WorkspacePath::parse(path).map_err(|err| {
            CepError::new(
                CepErrorCode::PathContainment,
                format!("command cwd rejected: {err}"),
                CepErrorData::Validation(lancet_protocol::error::ValidationErrorData {
                    field: "cwd".into(),
                    reason: err.to_string(),
                }),
                Retriability::Permanent,
            )
        }),
        None => Ok(WorkspacePath::project_root()),
    }
}

fn validate_direct_command_cwd(
    path: &str,
    facts: &PathFacts,
    authorized_region: &lancet_authority::effect::region::AuthorizedEffectRegion,
) -> Result<(), CepError> {
    let direct_facts = DirectPathFacts {
        normalized_path: facts.normalized_path.clone(),
        exists: facts.exists,
        kind: facts.kind.map(direct_path_kind_from_backend),
        parent_exists: facts.parent_exists,
        parent_kind: facts.parent_kind.map(direct_path_kind_from_backend),
        canonical_under_workspace: facts.canonical_under_workspace,
    };
    // Workspace root as cwd is always permitted — skip write-region check.
    let empty_region = lancet_authority::effect::region::AuthorizedEffectRegion::empty();
    let region = if path == "." {
        &empty_region
    } else {
        authorized_region
    };
    validate_direct_path_facts(path, &direct_facts, region)
}

fn direct_path_kind_from_backend(kind: crate::execution_backend::PathKind) -> DirectPathKind {
    match kind {
        crate::execution_backend::PathKind::File => DirectPathKind::File,
        crate::execution_backend::PathKind::Directory => DirectPathKind::Directory,
        crate::execution_backend::PathKind::Symlink => DirectPathKind::Symlink,
    }
}

fn record_direct_command_effects(
    ctx: &HandlerContext,
    _effective_impact: &EffectiveImpact,
    authorized_region: &lancet_authority::effect::region::AuthorizedEffectRegion,
) {
    let scope = direct_command_scope(authorized_region);
    ctx.record_effect(ObservedEffect {
        kind: ImpactKind::CommandExecution,
        scope,
        required_capability: RequiredCapability::RunCommand,
        source: EffectSource::HandlerBackend,
    });
}

fn direct_command_scope(
    authorized_region: &lancet_authority::effect::region::AuthorizedEffectRegion,
) -> ObservedScope {
    if let Some(prefix) = authorized_region.write_path_prefixes.first() {
        return ObservedScope::Root(prefix.to_string_lossy().to_string());
    }
    if let Some(path) = authorized_region.write_paths.first() {
        return ObservedScope::Root(path.to_string_lossy().to_string());
    }
    ObservedScope::Root(".".into())
}

fn verify_spec_from_direct(
    config: &crate::execution_backend::DirectVerifyConfig,
) -> VerifyCommandSpec {
    VerifyCommandSpec {
        argv: config.argv.clone(),
        cwd: Some(config.cwd.as_str().to_string()),
        timeout_ms: config.timeout_ms,
        expected_exit_code: Some(config.expected_exit_code),
    }
}

fn record_direct_backend_rejection(
    ctx: &HandlerContext,
    execution: &crate::execution_backend::ExecutionRouting,
) {
    ctx.record_rejection(RejectionRecord {
        rejection_stage: RejectionStage::BackendExecution,
        denial_source: execution.backend.backend_name().into(),
    });
}

fn direct_backend_unavailable_error(method: &str) -> CepError {
    CepError::new(
        CepErrorCode::ExecutionFailed,
        "Direct command execution requires an Upper execution backend",
        CepErrorData::Execution(lancet_protocol::error::ExecutionErrorData {
            command: method.into(),
            exit_code: None,
            stderr: Some("execution backend unavailable".into()),
        }),
        Retriability::Permanent,
    )
}

fn direct_command_cancelled_error(method: &str, argv: &[String]) -> CepError {
    CepError::new(
        CepErrorCode::ExecutionFailed,
        "command cancelled before backend execution",
        CepErrorData::Execution(lancet_protocol::error::ExecutionErrorData {
            command: argv.join(" "),
            exit_code: None,
            stderr: Some(format!("cancelled during {method}")),
        }),
        Retriability::Permanent,
    )
}

fn direct_command_backend_error(
    _method: &str,
    argv: &[String],
    err: ExecutionBackendError,
) -> CepError {
    let command = argv.join(" ");
    match err {
        ExecutionBackendError::Path(path_err) => CepError::new(
            CepErrorCode::PathContainment,
            format!("command cwd rejected: {path_err}"),
            CepErrorData::Validation(lancet_protocol::error::ValidationErrorData {
                field: "cwd".into(),
                reason: path_err.to_string(),
            }),
            Retriability::Permanent,
        ),
        ExecutionBackendError::Unavailable { message } => CepError::new(
            CepErrorCode::ExecutionFailed,
            format!("Upper backend unavailable: {message}"),
            CepErrorData::Execution(lancet_protocol::error::ExecutionErrorData {
                command: command.clone(),
                exit_code: None,
                stderr: Some(message),
            }),
            Retriability::Transient,
        ),
        ExecutionBackendError::ExecutionFailed { message } => CepError::new(
            CepErrorCode::ExecutionFailed,
            format!("Upper backend execution failed: {message}"),
            CepErrorData::Execution(lancet_protocol::error::ExecutionErrorData {
                command,
                exit_code: None,
                stderr: Some(message),
            }),
            Retriability::Transient,
        ),
        ExecutionBackendError::ContentTooLarge => CepError::new(
            CepErrorCode::ExecutionFailed,
            "command output exceeded configured limits",
            CepErrorData::Execution(lancet_protocol::error::ExecutionErrorData {
                command,
                exit_code: None,
                stderr: Some("content too large".into()),
            }),
            Retriability::Permanent,
        ),
        ExecutionBackendError::Timeout => CepError::new(
            CepErrorCode::ExecutionFailed,
            "command timed out in Upper backend",
            CepErrorData::Execution(lancet_protocol::error::ExecutionErrorData {
                command,
                exit_code: None,
                stderr: Some("timeout".into()),
            }),
            Retriability::Transient,
        ),
    }
}

fn bounded_stream_meta(content: &str, max_bytes: usize) -> BoundedStreamOutput {
    let total_bytes = content.len();
    let truncated = total_bytes > max_bytes;
    let tail_bytes = if truncated {
        content
            .chars()
            .take(max_bytes)
            .collect::<String>()
            .into_bytes()
    } else {
        content.as_bytes().to_vec()
    };
    BoundedStreamOutput {
        tail_bytes,
        total_bytes,
        truncated,
    }
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

#[allow(clippy::too_many_arguments)]
fn record_command_preflight(
    ctx: &HandlerContext,
    method: &'static str,
    argv: &[String],
    declared_impact: Option<&ImpactKind>,
    effective_impact: &EffectiveImpact,
    envelope_id: Option<&str>,
    run_command_capability_present: bool,
    argv_allowlist_matched: bool,
    backend_invoked: bool,
) {
    let authorized_region = ctx.authorized_region.get();
    ctx.record_durable_metadata(command_preflight_metadata(CommandPreflight {
        method,
        argv,
        declared_impact,
        effective_impact,
        envelope_id,
        run_command_capability_present,
        argv_allowlist_matched,
        authorized_region: authorized_region.as_ref(),
        backend_invoked,
    }));
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
