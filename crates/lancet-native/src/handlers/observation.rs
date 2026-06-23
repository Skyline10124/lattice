// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Native CEP workspace observation CEP handlers.

use std::path::{Path, PathBuf};

use crate::edit::content_hash;
use crate::enforcement::guard_l1::evaluate_and_enforce_policy;
use crate::execution_backend::{
    DirectoryEntry as BackendDirectoryEntry, ExecutionBackendError, ExecutionRouting, FindRequest,
    GrepRequest, PathFacts, PathIntent, WorkspacePath,
};
use crate::executor::render::render_file_slice;
use crate::server::registry::{HandlerContext, MethodRegistry};
use lancet_audit::{RejectionRecord, RejectionStage};
use lancet_authority::effect::ledger::{EffectSource, ObservedEffect, ObservedScope};
use lancet_authority::effect::region::AuthorizedEffectRegion;
use lancet_authority::guard::{DirectPathFacts, DirectPathKind, validate_direct_path_facts};
use lancet_authority::policy::CepPolicyConfig;
use lancet_authority::policy::budget::BudgetExhausted;
use lancet_catalog::CepMethod;
use lancet_catalog::method::RequiredCapability;
use lancet_catalog::methods::workspace::{
    DirectoryEntry, DirectoryEntryKind, FindFilesParams, FindFilesResult, GrepMatch, GrepParams,
    GrepResult, ListDirectoryParams, ListDirectoryResult, ObserveWorkspace, ObserveWorkspaceParams,
    ObserveWorkspaceResult, ReadFileParams, ReadFileResult,
};
use lancet_protocol::effect_types::ImpactKind;
use lancet_protocol::error::ValidationErrorData;
use lancet_protocol::{CepError, CepErrorCode, CepErrorData, Retriability};
use regex::Regex;
use serde::Serialize;
use serde_json::json;

const GREP_MAX_DEPTH: u32 = 32;
const DEFAULT_GREP_LIMIT: usize = 200;
const DEFAULT_LIST_LIMIT: usize = 200;

#[cfg(test)]
pub(crate) const BACKEND_SAFETY_NET_TEST_PATH: &str = "__cep_backend_safety_net__";

#[cfg(test)]
fn check_backend_safety_net(path: &str) -> Option<CepError> {
    if path == BACKEND_SAFETY_NET_TEST_PATH {
        Some(lancet_authority::policy::observation::backend_safety_net_denial(path))
    } else {
        None
    }
}

/// Register the native `observe.workspace` handler.
pub fn register_workspace_observation_methods(
    registry: &mut MethodRegistry,
) -> Result<(), lancet_protocol::CepError> {
    registry.register_method::<ObserveWorkspace, _, _>(handle_observe_workspace)?;
    Ok(())
}

async fn handle_observe_workspace(
    params: ObserveWorkspaceParams,
    ctx: HandlerContext,
) -> Result<ObserveWorkspaceResult, CepError> {
    match params {
        ObserveWorkspaceParams::ReadFile(params) => {
            let policy_params = ObserveWorkspaceParams::ReadFile(params.clone());
            handle_read_file_as(params, ctx, ObserveWorkspace::NAME, &policy_params)
                .await
                .map(ObserveWorkspaceResult::ReadFile)
        }
        ObserveWorkspaceParams::Grep(params) => {
            let policy_params = ObserveWorkspaceParams::Grep(params.clone());
            handle_grep_as(params, ctx, ObserveWorkspace::NAME, &policy_params)
                .await
                .map(ObserveWorkspaceResult::Grep)
        }
        ObserveWorkspaceParams::ListDirectory(params) => {
            let policy_params = ObserveWorkspaceParams::ListDirectory(params.clone());
            handle_list_directory_as(params, ctx, ObserveWorkspace::NAME, &policy_params)
                .await
                .map(ObserveWorkspaceResult::ListDirectory)
        }
        ObserveWorkspaceParams::FindFiles(params) => {
            let policy_params = ObserveWorkspaceParams::FindFiles(params.clone());
            handle_find_files_as(params, ctx, ObserveWorkspace::NAME, &policy_params)
                .await
                .map(ObserveWorkspaceResult::FindFiles)
        }
    }
}

async fn handle_read_file_as(
    params: ReadFileParams,
    ctx: HandlerContext,
    method: &'static str,
    policy_params: &impl Serialize,
) -> Result<ReadFileResult, CepError> {
    let _evaluation = evaluate_and_enforce_policy(&ctx, method, policy_params).await?;

    handle_direct_read_file(params, ctx, method).await
}

async fn handle_grep_as(
    params: GrepParams,
    ctx: HandlerContext,
    method: &'static str,
    policy_params: &impl Serialize,
) -> Result<GrepResult, CepError> {
    let _evaluation = evaluate_and_enforce_policy(&ctx, method, policy_params).await?;
    let path = params.path.clone().unwrap_or_else(|| ".".into());
    handle_direct_grep(params, ctx, &path, method).await
}

async fn handle_list_directory_as(
    params: ListDirectoryParams,
    ctx: HandlerContext,
    method: &'static str,
    policy_params: &impl Serialize,
) -> Result<ListDirectoryResult, CepError> {
    let _evaluation = evaluate_and_enforce_policy(&ctx, method, policy_params).await?;

    handle_direct_list_directory(params, ctx, method).await
}

async fn handle_find_files_as(
    params: FindFilesParams,
    ctx: HandlerContext,
    method: &'static str,
    policy_params: &impl Serialize,
) -> Result<FindFilesResult, CepError> {
    let path = params.path.clone().unwrap_or_else(|| ".".into());
    let _evaluation = evaluate_and_enforce_policy(&ctx, method, policy_params).await?;

    handle_direct_find_files(params, ctx, &path, method).await
}

fn charge_observation_bytes(ctx: &HandlerContext, bytes: usize) -> Result<(), CepError> {
    ctx.cep_context
        .budget
        .try_charge_observation(bytes)
        .map_err(|e: BudgetExhausted| e.to_cep_error())
}


fn single_read_budget_exceeded(actual: usize, policy: &CepPolicyConfig) -> CepError {
    BudgetExhausted {
        limit: "observation.max_single_read_bytes",
        requested: actual,
        remaining: policy
            .observation
            .max_single_read_bytes
            .saturating_sub(actual),
    }
    .to_cep_error()
}


fn path_not_found(path: &str) -> CepError {
    CepError::new(
        CepErrorCode::TypeError,
        format!("not_found: '{path}'"),
        CepErrorData::Validation(ValidationErrorData {
            field: "path".into(),
            reason: "not_found".into(),
        }),
        Retriability::Permanent,
    )
}


fn direct_execution(ctx: &HandlerContext) -> Result<&ExecutionRouting, CepError> {
    ctx.execution.as_ref().ok_or_else(|| {
        CepError::new(
            CepErrorCode::ExecutionFailed,
            "Direct observation requires an Upper execution backend",
            CepErrorData::Execution(lancet_protocol::error::ExecutionErrorData {
                command: "observe.workspace".into(),
                exit_code: None,
                stderr: Some("missing execution backend".into()),
            }),
            Retriability::Permanent,
        )
    })
}

fn parse_observation_path(raw: &str, field: &str) -> Result<WorkspacePath, CepError> {
    WorkspacePath::parse(raw).map_err(|err| {
        CepError::new(
            CepErrorCode::PathContainment,
            format!("observation path rejected: {err}"),
            CepErrorData::Validation(ValidationErrorData {
                field: field.into(),
                reason: err.to_string(),
            }),
            Retriability::Permanent,
        )
    })
}

fn path_facts_to_direct(facts: &PathFacts) -> DirectPathFacts {
    DirectPathFacts {
        normalized_path: facts.normalized_path.clone(),
        exists: facts.exists,
        kind: facts.kind.map(direct_path_kind),
        parent_exists: facts.parent_exists,
        parent_kind: facts.parent_kind.map(direct_path_kind),
        canonical_under_workspace: facts.canonical_under_workspace,
    }
}

fn direct_path_kind(kind: crate::execution_backend::PathKind) -> DirectPathKind {
    match kind {
        crate::execution_backend::PathKind::File => DirectPathKind::File,
        crate::execution_backend::PathKind::Directory => DirectPathKind::Directory,
        crate::execution_backend::PathKind::Symlink => DirectPathKind::Symlink,
    }
}

fn validate_direct_observation_facts(path: &str, facts: &PathFacts) -> Result<(), CepError> {
    validate_direct_path_facts(
        path,
        &path_facts_to_direct(facts),
        &AuthorizedEffectRegion::empty(),
    )
}

fn observation_backend_error(path: &str, err: ExecutionBackendError) -> CepError {
    CepError::new(
        CepErrorCode::ExecutionFailed,
        format!("observation backend failed for '{path}': {err}"),
        CepErrorData::Execution(lancet_protocol::error::ExecutionErrorData {
            command: "observe.workspace".into(),
            exit_code: None,
            stderr: Some(err.to_string()),
        }),
        Retriability::Permanent,
    )
}

fn reject_leaking_path(path: &str, requested: &str) -> CepError {
    CepError::new(
        CepErrorCode::ExecutionFailed,
        format!("backend returned leaking path '{path}' for request '{requested}'"),
        CepErrorData::Execution(lancet_protocol::error::ExecutionErrorData {
            command: "observe.workspace".into(),
            exit_code: None,
            stderr: Some("path leak".into()),
        }),
        Retriability::Permanent,
    )
}

fn normalize_backend_path(path: &str) -> Result<String, CepError> {
    let parsed = WorkspacePath::parse(path).map_err(|_| reject_leaking_path(path, path))?;
    Ok(parsed.as_str().to_string())
}

fn convert_directory_entry(entry: &BackendDirectoryEntry) -> DirectoryEntry {
    DirectoryEntry {
        name: entry.name.clone(),
        kind: match entry.kind {
            crate::execution_backend::PathKind::File => DirectoryEntryKind::File,
            crate::execution_backend::PathKind::Directory => DirectoryEntryKind::Directory,
            crate::execution_backend::PathKind::Symlink => DirectoryEntryKind::Symlink,
        },
    }
}

async fn record_direct_observation_denial(
    ctx: &HandlerContext,
    method: &str,
    stage: RejectionStage,
    denial_source: &str,
    backend_invoked: bool,
) {
    ctx.record_rejection(RejectionRecord {
        rejection_stage: stage,
        denial_source: denial_source.into(),
    });
    emit_denial_metadata(ctx, method, denial_source, backend_invoked).await;
}

async fn emit_direct_success_metadata(
    ctx: &HandlerContext,
    method: &str,
    request_path: &str,
    mut metadata: serde_json::Value,
) {
    if let Some(obj) = metadata.as_object_mut() {
        obj.insert("request_target".to_string(), json!(request_path));
    }
    ctx.record_durable_metadata(metadata.clone());
    ctx.events
        .emit(
            "cep.native_observation_metadata",
            json!({ "method": method, "metadata": metadata }),
        )
        .await;
}

async fn handle_direct_read_file(
    params: ReadFileParams,
    ctx: HandlerContext,
    method: &str,
) -> Result<ReadFileResult, CepError> {
    let policy = &ctx.cep_context.policy;
    let execution = direct_execution(&ctx)?;
    let ws_path = parse_observation_path(&params.path, "path")?;
    ws_path.require_file_path().map_err(|err| {
        CepError::new(
            CepErrorCode::PathContainment,
            format!("read path rejected: {err}"),
            CepErrorData::Validation(ValidationErrorData {
                field: "path".into(),
                reason: err.to_string(),
            }),
            Retriability::Permanent,
        )
    })?;

    ctx.mark_adapter_preflight_invoked();
    let facts = match execution
        .backend
        .resolve_path(&ws_path, PathIntent::Read)
        .await
    {
        Ok(facts) => facts,
        Err(err) => {
            record_direct_observation_denial(
                &ctx,
                method,
                RejectionStage::AdapterPreflight,
                "adapter_preflight",
                false,
            )
            .await;
            return Err(observation_backend_error(&params.path, err));
        }
    };
    if let Err(err) = validate_direct_observation_facts(ws_path.as_str(), &facts) {
        record_direct_observation_denial(
            &ctx,
            method,
            RejectionStage::AdapterPreflight,
            "guard_l1",
            false,
        )
        .await;
        return Err(err);
    }
    if !facts.exists || !matches!(facts.kind, Some(crate::execution_backend::PathKind::File)) {
        return Err(path_not_found(&params.path));
    }
    if let Some(size) = facts.byte_size
        && size as usize > policy.observation.max_single_read_bytes
    {
        return Err(single_read_budget_exceeded(size as usize, policy));
    }

    ctx.mark_backend_invoked();
    let bytes = execution
        .backend
        .read_file(&ws_path)
        .await
        .map_err(|err| observation_backend_error(&params.path, err))?;
    if bytes.len() > policy.observation.max_single_read_bytes {
        return Err(single_read_budget_exceeded(bytes.len(), policy));
    }
    let content = String::from_utf8(bytes).map_err(|e| {
        CepError::new(
            CepErrorCode::TypeError,
            format!("io error reading '{}': {e}", params.path),
            CepErrorData::Validation(ValidationErrorData {
                field: "path".into(),
                reason: e.to_string(),
            }),
            Retriability::Permanent,
        )
    })?;

    let offset = params
        .offset
        .and_then(|v| usize::try_from(v).ok())
        .unwrap_or(1)
        .max(1);
    let limit = params
        .limit
        .and_then(|v| usize::try_from(v).ok())
        .unwrap_or(2000)
        .max(1);
    let line_numbers = params.line_numbers.unwrap_or(true);
    let hash = content_hash(content.as_bytes());
    let rendered = render_file_slice(&content, offset, limit, line_numbers);
    let truncated = rendered.contains("[TRUNCATED:");
    let bytes_returned = rendered.len();
    charge_observation_bytes(&ctx, bytes_returned)?;

    ctx.record_effect(ObservedEffect {
        kind: ImpactKind::WorkspaceObservation,
        scope: ObservedScope::Root(params.path.clone()),
        required_capability: RequiredCapability::ReadPath,
        source: EffectSource::HandlerBackend,
    });

    emit_direct_success_metadata(
        &ctx,
        method,
        &params.path,
        json!({
            "observation_source": "workspace",
            "bytes_read": content.len(),
            "bytes_returned": bytes_returned,
            "content_hash": hash,
            "truncated": truncated,
            "policy_enforced": true,
            "backend_invoked": true,
        }),
    )
    .await;

    Ok(ReadFileResult {
        content: rendered,
        content_hash: Some(hash),
    })
}

async fn handle_direct_list_directory(
    params: ListDirectoryParams,
    ctx: HandlerContext,
    method: &str,
) -> Result<ListDirectoryResult, CepError> {
    let policy = &ctx.cep_context.policy;
    let execution = direct_execution(&ctx)?;
    let raw_path = if params.path.is_empty() {
        ".".to_string()
    } else {
        params.path.clone()
    };
    let ws_path = parse_observation_path(&raw_path, "path")?;

    ctx.mark_adapter_preflight_invoked();
    let facts = match execution
        .backend
        .resolve_path(&ws_path, PathIntent::ListDirectory)
        .await
    {
        Ok(facts) => facts,
        Err(err) => {
            record_direct_observation_denial(
                &ctx,
                method,
                RejectionStage::AdapterPreflight,
                "adapter_preflight",
                false,
            )
            .await;
            return Err(observation_backend_error(&raw_path, err));
        }
    };
    if let Err(err) = validate_direct_observation_facts(ws_path.as_str(), &facts) {
        record_direct_observation_denial(
            &ctx,
            method,
            RejectionStage::AdapterPreflight,
            "guard_l1",
            false,
        )
        .await;
        return Err(err);
    }
    if !facts.exists
        || !matches!(
            facts.kind,
            Some(crate::execution_backend::PathKind::Directory)
        )
    {
        return Err(path_not_found(&raw_path));
    }

    let limit = params
        .limit
        .and_then(|v| usize::try_from(v).ok())
        .unwrap_or(DEFAULT_LIST_LIMIT)
        .clamp(1, policy.observation.max_list_entries);

    ctx.mark_backend_invoked();
    let backend_entries = execution
        .backend
        .list_dir(&ws_path)
        .await
        .map_err(|err| observation_backend_error(&raw_path, err))?;
    let mut entries = backend_entries
        .iter()
        .map(convert_directory_entry)
        .collect::<Vec<_>>();
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    let total = entries.len();
    let truncated = total > limit;
    if truncated {
        entries.truncate(limit);
    }

    let bytes_returned = serde_json::to_string(&entries)
        .map(|s| s.len())
        .unwrap_or(0);
    charge_observation_bytes(&ctx, bytes_returned)?;

    ctx.record_effect(ObservedEffect {
        kind: ImpactKind::WorkspaceObservation,
        scope: ObservedScope::Root(raw_path.clone()),
        required_capability: RequiredCapability::Inspect,
        source: EffectSource::HandlerBackend,
    });

    emit_direct_success_metadata(
        &ctx,
        method,
        &raw_path,
        json!({
            "observation_source": "workspace",
            "entry_count": total,
            "bytes_returned": bytes_returned,
            "truncated": truncated,
            "policy_enforced": true,
            "backend_invoked": true,
        }),
    )
    .await;

    Ok(ListDirectoryResult {
        entries,
        truncated: truncated.then_some(true),
    })
}

async fn handle_direct_grep(
    params: GrepParams,
    ctx: HandlerContext,
    path: &str,
    method: &str,
) -> Result<GrepResult, CepError> {
    let policy = &ctx.cep_context.policy;
    let execution = direct_execution(&ctx)?;
    let ws_path = parse_observation_path(path, "path")?;
    let _re = Regex::new(&params.pattern).map_err(|e| {
        CepError::new(
            CepErrorCode::TypeError,
            format!("invalid regex: {e}"),
            CepErrorData::Validation(ValidationErrorData {
                field: "pattern".into(),
                reason: e.to_string(),
            }),
            Retriability::Permanent,
        )
    })?;

    ctx.mark_adapter_preflight_invoked();
    let facts = match execution
        .backend
        .resolve_path(&ws_path, PathIntent::ListDirectory)
        .await
    {
        Ok(facts) => facts,
        Err(err) => {
            record_direct_observation_denial(
                &ctx,
                method,
                RejectionStage::AdapterPreflight,
                "adapter_preflight",
                false,
            )
            .await;
            return Err(observation_backend_error(path, err));
        }
    };
    if let Err(err) = validate_direct_observation_facts(ws_path.as_str(), &facts) {
        record_direct_observation_denial(
            &ctx,
            method,
            RejectionStage::AdapterPreflight,
            "guard_l1",
            false,
        )
        .await;
        return Err(err);
    }
    if !facts.exists
        || !matches!(
            facts.kind,
            Some(crate::execution_backend::PathKind::Directory)
        )
    {
        return Err(path_not_found(path));
    }

    let max_results = params
        .max_results
        .and_then(|v| usize::try_from(v).ok())
        .unwrap_or(DEFAULT_GREP_LIMIT)
        .clamp(1, policy.observation.max_grep_matches);

    ctx.mark_backend_invoked();
    let request = GrepRequest {
        pattern: params.pattern.clone(),
        path: ws_path,
        glob: None,
    };
    let backend_matches = execution
        .backend
        .grep(&request)
        .await
        .map_err(|err| observation_backend_error(path, err))?;

    let mut matches = Vec::new();
    let mut truncated = backend_matches.len() >= max_results;
    for m in backend_matches {
        let rel = normalize_backend_path(m.path.as_str())?;
        matches.push(GrepMatch {
            path: rel,
            line: m.line_number,
            text: m.line,
        });
        if matches.len() >= max_results {
            truncated = true;
            break;
        }
    }

    let bytes_returned = matches
        .iter()
        .map(|m| m.path.len() + m.text.len() + 16)
        .sum::<usize>();
    charge_observation_bytes(&ctx, bytes_returned)?;

    ctx.record_effect(ObservedEffect {
        kind: ImpactKind::WorkspaceObservation,
        scope: ObservedScope::Root(path.to_string()),
        required_capability: RequiredCapability::Inspect,
        source: EffectSource::HandlerBackend,
    });

    emit_direct_success_metadata(
        &ctx,
        method,
        path,
        json!({
            "observation_source": "workspace",
            "match_count": matches.len(),
            "bytes_returned": bytes_returned,
            "truncated": truncated,
            "policy_enforced": true,
            "backend_invoked": true,
        }),
    )
    .await;

    Ok(GrepResult {
        matches,
        truncated: truncated.then_some(true),
    })
}

async fn handle_direct_find_files(
    params: FindFilesParams,
    ctx: HandlerContext,
    path: &str,
    method: &str,
) -> Result<FindFilesResult, CepError> {
    let policy = &ctx.cep_context.policy;
    let execution = direct_execution(&ctx)?;
    let ws_path = parse_observation_path(path, "path")?;

    ctx.mark_adapter_preflight_invoked();
    let facts = match execution
        .backend
        .resolve_path(&ws_path, PathIntent::ListDirectory)
        .await
    {
        Ok(facts) => facts,
        Err(err) => {
            record_direct_observation_denial(
                &ctx,
                method,
                RejectionStage::AdapterPreflight,
                "adapter_preflight",
                false,
            )
            .await;
            return Err(observation_backend_error(path, err));
        }
    };
    if let Err(err) = validate_direct_observation_facts(ws_path.as_str(), &facts) {
        record_direct_observation_denial(
            &ctx,
            method,
            RejectionStage::AdapterPreflight,
            "guard_l1",
            false,
        )
        .await;
        return Err(err);
    }
    if !facts.exists
        || !matches!(
            facts.kind,
            Some(crate::execution_backend::PathKind::Directory)
        )
    {
        return Err(path_not_found(path));
    }

    let limit = params
        .limit
        .and_then(|v| usize::try_from(v).ok())
        .unwrap_or(DEFAULT_LIST_LIMIT)
        .clamp(1, policy.observation.max_find_paths);

    ctx.mark_backend_invoked();
    let request = FindRequest {
        pattern: params.pattern.clone(),
        path: ws_path,
    };
    let backend_paths = execution
        .backend
        .find_files(&request)
        .await
        .map_err(|err| observation_backend_error(path, err))?;

    let mut paths = Vec::new();
    let mut truncated = backend_paths.len() >= limit;
    for p in backend_paths {
        paths.push(normalize_backend_path(p.as_str())?);
        if paths.len() >= limit {
            truncated = true;
            break;
        }
    }

    let bytes_returned = paths.iter().map(|p| p.len()).sum::<usize>();
    charge_observation_bytes(&ctx, bytes_returned)?;

    ctx.record_effect(ObservedEffect {
        kind: ImpactKind::WorkspaceObservation,
        scope: ObservedScope::Root(path.to_string()),
        required_capability: RequiredCapability::Inspect,
        source: EffectSource::HandlerBackend,
    });

    emit_direct_success_metadata(
        &ctx,
        method,
        path,
        json!({
            "observation_source": "workspace",
            "path_count": paths.len(),
            "bytes_returned": bytes_returned,
            "truncated": truncated,
            "policy_enforced": true,
            "backend_invoked": true,
        }),
    )
    .await;

    Ok(FindFilesResult {
        paths,
        truncated: truncated.then_some(true),
    })
}


async fn emit_denial_metadata(
    ctx: &HandlerContext,
    method: &str,
    denial_source: &str,
    backend_invoked: bool,
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
            "cep.native_observation_metadata",
            json!({ "method": method, "metadata": metadata }),
        )
        .await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::registry::{Cancellation, EventSink};
    use lancet_authority::policy::CepTurnContext;
    use lancet_authority::policy::observation::reject_parent_traversal;
    use lancet_protocol::policy::BaseExecutionEnvelope;
    use std::fs;
    use std::sync::Arc;
    use tempfile::TempDir;
    use tokio::sync::{mpsc, watch};

    fn native_registry(root: &str) -> MethodRegistry {
        let mut registry = MethodRegistry::new();
        register_workspace_observation_methods(&mut registry).unwrap();
        let _ = root;
        registry
    }

    fn ctx_for(root: &str) -> (HandlerContext, mpsc::Receiver<lancet_protocol::ToolEvent>) {
        ctx_for_policy(CepPolicyConfig::defaults_for_project_root(root), root)
    }

    fn ctx_for_policy(
        policy: CepPolicyConfig,
        envelope_root: &str,
    ) -> (HandlerContext, mpsc::Receiver<lancet_protocol::ToolEvent>) {
        let (event_tx, event_rx) = mpsc::channel(32);
        let (_cancel_tx, cancel_rx) = watch::channel(false);
        let cep_context = Arc::new(CepTurnContext::from_policy(policy));
        let policy_cfg = lancet_authority::policy::default::PolicyConfig {
            capability_sources: Default::default(),
            methods: Default::default(),
            command: cep_context.policy.command.clone(),
            network: cep_context.policy.network.clone(),
            budget: cep_context.policy.budget.clone(),
            observation: cep_context.policy.observation.clone(),
            workspace: Default::default(),
            project_root: cep_context.policy.project_root.clone(),
            denied_effect_kinds: Default::default(),
        };
        let default_hook = Arc::new(lancet_authority::policy::default::DefaultPolicyHook::new(
            policy_cfg,
        ));
        let mut base = BaseExecutionEnvelope::for_workspace_root(envelope_root, "native obs test");
        base.inner.capabilities = base
            .inner
            .capabilities
            .into_iter()
            .map(|cap| match cap {
                lancet_protocol::policy::Capability::ReadPath { .. } => {
                    lancet_protocol::policy::Capability::ReadPath {
                        path_pattern: ".".into(),
                    }
                }
                other => other,
            })
            .collect();
        (
            HandlerContext {
                call_id: 1,
                events: EventSink::new(event_tx, 1),
                cancellation: Cancellation::new(cancel_rx),
                base_envelope: base,
                cep_context,
                active_envelope: Arc::new(
                    lancet_authority::envelope_store::ActiveEnvelopeStore::new(),
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
                authority_mode: lancet_protocol::policy::AuthorityMode::default(),
                execution: None,
                direct_verify: None,
                routing_invocation: crate::execution_backend::RoutingInvocationState::default(),
            },
            event_rx,
        )
    }

    fn policy_with_limits(root: &Path, max_single: usize, max_turn: usize) -> CepTurnContext {
        let mut policy = CepPolicyConfig::defaults_for_project_root(root);
        policy.observation.max_single_read_bytes = max_single;
        policy.observation.max_turn_observation_bytes = max_turn;
        policy.budget.max_single_read_bytes = max_single;
        policy.budget.max_turn_observation_bytes = max_turn;
        CepTurnContext::from_policy(policy)
    }

    async fn dispatch_read_file(
        registry: &MethodRegistry,
        params: ReadFileParams,
        ctx: HandlerContext,
    ) -> Result<ReadFileResult, CepError> {
        let value = registry
            .dispatch(
                ObserveWorkspace::NAME,
                serde_json::to_value(ObserveWorkspaceParams::ReadFile(params)).unwrap(),
                ctx,
            )
            .await?;
        match serde_json::from_value(value).unwrap() {
            ObserveWorkspaceResult::ReadFile(result) => Ok(result),
            other => panic!("unexpected observe.workspace result: {other:?}"),
        }
    }

    async fn dispatch_grep(
        registry: &MethodRegistry,
        params: GrepParams,
        ctx: HandlerContext,
    ) -> Result<GrepResult, CepError> {
        let value = registry
            .dispatch(
                ObserveWorkspace::NAME,
                serde_json::to_value(ObserveWorkspaceParams::Grep(params)).unwrap(),
                ctx,
            )
            .await?;
        match serde_json::from_value(value).unwrap() {
            ObserveWorkspaceResult::Grep(result) => Ok(result),
            other => panic!("unexpected observe.workspace result: {other:?}"),
        }
    }

    async fn dispatch_list_directory(
        registry: &MethodRegistry,
        params: ListDirectoryParams,
        ctx: HandlerContext,
    ) -> Result<ListDirectoryResult, CepError> {
        let value = registry
            .dispatch(
                ObserveWorkspace::NAME,
                serde_json::to_value(ObserveWorkspaceParams::ListDirectory(params)).unwrap(),
                ctx,
            )
            .await?;
        match serde_json::from_value(value).unwrap() {
            ObserveWorkspaceResult::ListDirectory(result) => Ok(result),
            other => panic!("unexpected observe.workspace result: {other:?}"),
        }
    }

    async fn dispatch_find_files(
        registry: &MethodRegistry,
        params: FindFilesParams,
        ctx: HandlerContext,
    ) -> Result<FindFilesResult, CepError> {
        let value = registry
            .dispatch(
                ObserveWorkspace::NAME,
                serde_json::to_value(ObserveWorkspaceParams::FindFiles(params)).unwrap(),
                ctx,
            )
            .await?;
        match serde_json::from_value(value).unwrap() {
            ObserveWorkspaceResult::FindFiles(result) => Ok(result),
            other => panic!("unexpected observe.workspace result: {other:?}"),
        }
    }

    #[tokio::test]
    async fn native_read_file_returns_typed_content_and_hash() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("hello.txt"), "hello\nworld\n").unwrap();
        let root = dir.path().to_str().unwrap();
        let registry = native_registry(root);
        let (ctx, _events) = ctx_for(root);

        let params = ReadFileParams {
            path: "hello.txt".into(),
            offset: Some(1),
            limit: Some(10),
            line_numbers: Some(false),
        };
        let parsed = dispatch_read_file(&registry, params, ctx).await.unwrap();
        assert!(parsed.content.contains("hello"));
        assert!(parsed.content_hash.as_ref().unwrap().starts_with("blake3:"));
    }

    #[tokio::test]
    async fn native_read_file_rejects_parent_traversal_without_backend() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().to_str().unwrap();
        let registry = native_registry(root);
        let (ctx, mut events) = ctx_for(root);

        let params = ReadFileParams {
            path: "../outside".into(),
            offset: None,
            limit: None,
            line_numbers: None,
        };
        let err = dispatch_read_file(&registry, params, ctx)
            .await
            .unwrap_err();
        assert_eq!(err.code, CepErrorCode::GuardDenied);

        let event = events.recv().await.unwrap();
        assert_eq!(event.method, "cep.guard_verdict");
    }

    #[tokio::test]
    async fn native_read_file_single_read_budget_error() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("big.txt"), "x".repeat(200)).unwrap();
        let root = dir.path().to_str().unwrap();
        let mut registry = MethodRegistry::new();
        register_workspace_observation_methods(&mut registry).unwrap();
        let (event_tx, _rx) = mpsc::channel(8);
        let (_cancel_tx, cancel_rx) = watch::channel(false);
        let cep_context = Arc::new(policy_with_limits(dir.path(), 50, 10_000));
        let ctx = HandlerContext {
            call_id: 2,
            events: EventSink::new(event_tx, 2),
            cancellation: Cancellation::new(cancel_rx),
            base_envelope: BaseExecutionEnvelope::for_workspace_root(root, "budget test"),
            cep_context,
            active_envelope: Arc::new(lancet_authority::envelope_store::ActiveEnvelopeStore::new()),
            turn_id: "test-turn".into(),
            durable_metadata: crate::server::call_durable_metadata::CallDurableMetadata::default(),
            effect_ledger: lancet_authority::effect::ledger::EffectLedger::new(),
            authorized_region: lancet_authority::effect::ledger::AuthorizedRegionSlot::new(),
            policy_hook: Arc::new(lancet_authority::policy::default::DefaultPolicyHook::new(
                lancet_authority::policy::default::PolicyConfig::defaults_for_project_root(
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
            cep_mode: lancet_protocol::policy::CepMode::Off,
            command_isolation:
                crate::enforcement::isolation::CommandIsolationRuntimeConfig::default(),
            authority_mode: lancet_protocol::policy::AuthorityMode::default(),
            execution: None,
            direct_verify: None,
            routing_invocation: crate::execution_backend::RoutingInvocationState::default(),
        };
        let params = ReadFileParams {
            path: "big.txt".into(),
            offset: None,
            limit: None,
            line_numbers: None,
        };
        let err = dispatch_read_file(&registry, params, ctx)
            .await
            .unwrap_err();
        assert_eq!(err.code, CepErrorCode::Reserved);
    }

    #[tokio::test]
    async fn native_read_file_turn_budget_error() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("a.txt"), "abcdefghij").unwrap();
        let root = dir.path().to_str().unwrap();
        let mut registry = MethodRegistry::new();
        register_workspace_observation_methods(&mut registry).unwrap();
        let (event_tx, _rx) = mpsc::channel(8);
        let (_cancel_tx, cancel_rx) = watch::channel(false);
        let cep_context = Arc::new(policy_with_limits(dir.path(), 10_000, 5));
        let ctx = HandlerContext {
            call_id: 3,
            events: EventSink::new(event_tx, 3),
            cancellation: Cancellation::new(cancel_rx),
            base_envelope: BaseExecutionEnvelope::for_workspace_root(root, "turn budget"),
            cep_context,
            active_envelope: Arc::new(lancet_authority::envelope_store::ActiveEnvelopeStore::new()),
            turn_id: "test-turn".into(),
            durable_metadata: crate::server::call_durable_metadata::CallDurableMetadata::default(),
            effect_ledger: lancet_authority::effect::ledger::EffectLedger::new(),
            authorized_region: lancet_authority::effect::ledger::AuthorizedRegionSlot::new(),
            policy_hook: Arc::new(lancet_authority::policy::default::DefaultPolicyHook::new(
                lancet_authority::policy::default::PolicyConfig::defaults_for_project_root(
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
            cep_mode: lancet_protocol::policy::CepMode::Off,
            command_isolation:
                crate::enforcement::isolation::CommandIsolationRuntimeConfig::default(),
            authority_mode: lancet_protocol::policy::AuthorityMode::default(),
            execution: None,
            direct_verify: None,
            routing_invocation: crate::execution_backend::RoutingInvocationState::default(),
        };
        let params = ReadFileParams {
            path: "a.txt".into(),
            offset: None,
            limit: None,
            line_numbers: None,
        };
        let err = dispatch_read_file(&registry, params, ctx)
            .await
            .unwrap_err();
        assert_eq!(err.code, CepErrorCode::Reserved);
    }

    #[tokio::test]
    async fn backend_safety_net_denial_records_backend_invoked() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join(BACKEND_SAFETY_NET_TEST_PATH), "probe").unwrap();
        let root = dir.path().to_str().unwrap();
        let registry = native_registry(root);
        let (ctx, mut events) = ctx_for(root);

        let params = ReadFileParams {
            path: BACKEND_SAFETY_NET_TEST_PATH.into(),
            offset: None,
            limit: None,
            line_numbers: None,
        };
        let err = dispatch_read_file(&registry, params, ctx)
            .await
            .unwrap_err();
        assert_eq!(err.code, CepErrorCode::ProjectRootViolation);

        let guard_event = events.recv().await.unwrap();
        assert_eq!(guard_event.method, "cep.guard_verdict");
        let event = events.recv().await.unwrap();
        assert_eq!(
            event.params["metadata"]["denial_source"],
            "backend_safety_net"
        );
        assert_eq!(event.params["metadata"]["backend_invoked"], true);
    }

    #[tokio::test]
    async fn native_workspace_observation_grep_rejects_parent_traversal() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().to_str().unwrap();
        let registry = native_registry(root);
        let (ctx, mut events) = ctx_for(root);

        let params = GrepParams {
            pattern: "x".into(),
            path: Some("../outside".into()),
            max_results: None,
        };
        let err = dispatch_grep(&registry, params, ctx).await.unwrap_err();
        assert_eq!(err.code, CepErrorCode::GuardDenied);
        let event = events.recv().await.unwrap();
        assert_eq!(event.method, "cep.guard_verdict");
    }

    #[tokio::test]
    async fn native_workspace_observation_grep_invalid_regex() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().to_str().unwrap();
        let registry = native_registry(root);
        let (ctx, _events) = ctx_for(root);

        let params = GrepParams {
            pattern: "[unclosed".into(),
            path: Some(".".into()),
            max_results: None,
        };
        let err = dispatch_grep(&registry, params, ctx).await.unwrap_err();
        assert_eq!(err.code, CepErrorCode::TypeError);
    }

    #[tokio::test]
    async fn native_workspace_observation_grep_and_list_and_find() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("needle.txt"), "find me\n").unwrap();
        fs::create_dir(dir.path().join("sub")).unwrap();
        fs::write(dir.path().join("sub/inner.txt"), "nested\n").unwrap();
        let root = dir.path().to_str().unwrap();
        let registry = native_registry(root);
        let (ctx, _events) = ctx_for(root);

        let grep_params = GrepParams {
            pattern: "find".into(),
            path: Some(".".into()),
            max_results: Some(10),
        };
        let grep = dispatch_grep(&registry, grep_params, ctx.clone())
            .await
            .unwrap();
        assert!(!grep.matches.is_empty());

        let list_params = ListDirectoryParams {
            path: ".".into(),
            limit: Some(50),
        };
        let list = dispatch_list_directory(&registry, list_params, ctx.clone())
            .await
            .unwrap();
        assert!(!list.entries.is_empty());

        let find_params = FindFilesParams {
            pattern: "*.txt".into(),
            path: Some(".".into()),
            limit: Some(10),
        };
        let find = dispatch_find_files(&registry, find_params, ctx)
            .await
            .unwrap();
        assert!(!find.paths.is_empty());
    }

    #[tokio::test]
    async fn native_observation_records_requested_root_effect_scope_under_allowed_root() {
        let dir = TempDir::new().unwrap();
        let src = dir.path().join("src");
        fs::create_dir(&src).unwrap();
        fs::write(src.join("allowed.txt"), "needle\n").unwrap();

        let root = dir.path().to_str().unwrap();
        let registry = native_registry(root);
        let mut policy = CepPolicyConfig::defaults_for_project_root(dir.path());
        policy.observation.allowed_roots = vec![src.canonicalize().unwrap()];
        let (ctx, _events) = ctx_for_policy(policy, root);

        let grep_params = GrepParams {
            pattern: "needle".into(),
            path: Some("src".into()),
            max_results: Some(10),
        };
        let grep = dispatch_grep(&registry, grep_params, ctx.clone())
            .await
            .unwrap();
        assert_eq!(grep.matches.len(), 1);
        assert_eq!(grep.matches[0].path, "allowed.txt");

        let src_scope = src.canonicalize().unwrap().to_string_lossy().to_string();
        let effects = ctx.effect_ledger.snapshot();
        assert!(
            effects.iter().any(|effect| matches!(
                &effect.scope,
                ObservedScope::Root(root) if root == &src_scope
            )),
            "expected observation effect scoped to {src_scope}, got {effects:?}"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn native_recursive_observation_does_not_follow_symlink_outside_allowed_root() {
        let dir = TempDir::new().unwrap();
        let src = dir.path().join("src");
        fs::create_dir(&src).unwrap();
        fs::write(src.join("allowed.txt"), "SECRET_ALLOWED=yes\n").unwrap();
        fs::write(dir.path().join("secret.txt"), "SECRET_FORBIDDEN=yes\n").unwrap();
        std::os::unix::fs::symlink("../secret.txt", src.join("link_to_secret.txt")).unwrap();

        let root = dir.path().to_str().unwrap();
        let registry = native_registry(root);
        let mut policy = CepPolicyConfig::defaults_for_project_root(dir.path());
        policy.observation.allowed_roots = vec![src.canonicalize().unwrap()];
        let (ctx, _events) = ctx_for_policy(policy, root);

        let read_params = ReadFileParams {
            path: "secret.txt".into(),
            offset: None,
            limit: None,
            line_numbers: None,
        };
        let read_err = dispatch_read_file(&registry, read_params, ctx.clone())
            .await
            .unwrap_err();
        assert_eq!(read_err.code, CepErrorCode::ProjectRootViolation);

        let grep_params = GrepParams {
            pattern: "SECRET".into(),
            path: Some("src".into()),
            max_results: Some(10),
        };
        let grep = dispatch_grep(&registry, grep_params, ctx.clone())
            .await
            .unwrap();
        assert_eq!(grep.matches.len(), 1);
        assert_eq!(grep.matches[0].path, "allowed.txt");
        assert_eq!(grep.matches[0].text, "SECRET_ALLOWED=yes");

        let find_params = FindFilesParams {
            pattern: "*secret*.txt".into(),
            path: Some("src".into()),
            limit: Some(10),
        };
        let find = dispatch_find_files(&registry, find_params, ctx)
            .await
            .unwrap();
        assert!(find.paths.is_empty(), "unexpected paths: {:?}", find.paths);
    }

    #[tokio::test]
    async fn native_workspace_observation_list_directory_rejects_parent_traversal() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().to_str().unwrap();
        let registry = native_registry(root);
        let (ctx, _events) = ctx_for(root);

        let params = ListDirectoryParams {
            path: "../outside".into(),
            limit: None,
        };
        let err = dispatch_list_directory(&registry, params, ctx)
            .await
            .unwrap_err();
        assert_eq!(err.code, CepErrorCode::GuardDenied);
    }

    #[tokio::test]
    async fn native_workspace_observation_truncates_grep_results() {
        let dir = TempDir::new().unwrap();
        for i in 0..5 {
            fs::write(dir.path().join(format!("f{i}.txt")), "match\n").unwrap();
        }
        let root = dir.path().to_str().unwrap();
        let mut registry = MethodRegistry::new();
        register_workspace_observation_methods(&mut registry).unwrap();
        let (event_tx, _rx) = mpsc::channel(8);
        let (_cancel_tx, cancel_rx) = watch::channel(false);
        let mut policy = CepPolicyConfig::defaults_for_project_root(dir.path());
        policy.observation.max_grep_matches = 2;
        policy.budget.max_grep_matches = 2;
        let ctx = HandlerContext {
            call_id: 4,
            events: EventSink::new(event_tx, 4),
            cancellation: Cancellation::new(cancel_rx),
            base_envelope: BaseExecutionEnvelope::for_workspace_root(root, "grep cap"),
            cep_context: Arc::new(CepTurnContext::from_policy(policy)),
            active_envelope: Arc::new(lancet_authority::envelope_store::ActiveEnvelopeStore::new()),
            turn_id: "test-turn".into(),
            durable_metadata: crate::server::call_durable_metadata::CallDurableMetadata::default(),
            effect_ledger: lancet_authority::effect::ledger::EffectLedger::new(),
            authorized_region: lancet_authority::effect::ledger::AuthorizedRegionSlot::new(),
            policy_hook: Arc::new(lancet_authority::policy::default::DefaultPolicyHook::new(
                lancet_authority::policy::default::PolicyConfig::defaults_for_project_root(
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
            cep_mode: lancet_protocol::policy::CepMode::Off,
            command_isolation:
                crate::enforcement::isolation::CommandIsolationRuntimeConfig::default(),
            authority_mode: lancet_protocol::policy::AuthorityMode::default(),
            execution: None,
            direct_verify: None,
            routing_invocation: crate::execution_backend::RoutingInvocationState::default(),
        };
        let params = GrepParams {
            pattern: "match".into(),
            path: Some(".".into()),
            max_results: None,
        };
        let grep = dispatch_grep(&registry, params, ctx).await.unwrap();
        assert_eq!(grep.matches.len(), 2);
        assert_eq!(grep.truncated, Some(true));
    }

    #[test]
    fn observation_policy_rejects_dotdot_in_handler_path() {
        let err = reject_parent_traversal("foo/../bar").unwrap_err();
        assert_eq!(err.code, CepErrorCode::PathContainment);
    }

    mod phase34_direct_observation {
        use super::*;
        use crate::execution_backend::{
            DirectoryEntry, ExecutionBackend, ExecutionBackendError, ExecutionRouting, FindRequest,
            GrepMatch, GrepRequest, PathFacts, PathIntent, UpperContext, WorkspacePath,
        };
        use lancet_protocol::policy::AuthorityMode;
        use std::future::Future;
        use std::pin::Pin;
        use std::sync::{Arc, Mutex};

        #[derive(Default)]
        struct ObsSpyBackend {
            read_content: Mutex<Vec<u8>>,
        }

        impl ExecutionBackend for ObsSpyBackend {
            fn backend_name(&self) -> &str {
                "obs-spy"
            }

            fn resolve_path(
                &self,
                path: &WorkspacePath,
                _intent: PathIntent,
            ) -> Pin<Box<dyn Future<Output = Result<PathFacts, ExecutionBackendError>> + Send + '_>>
            {
                let path_str = path.as_str().to_string();
                Box::pin(async move {
                    Ok(PathFacts {
                        normalized_path: path_str.clone(),
                        exists: true,
                        kind: Some(crate::execution_backend::PathKind::File),
                        parent_exists: true,
                        parent_kind: Some(crate::execution_backend::PathKind::Directory),
                        byte_size: Some(5),
                        canonical_under_workspace: true,
                    })
                })
            }

            fn read_file(
                &self,
                _path: &WorkspacePath,
            ) -> Pin<Box<dyn Future<Output = Result<Vec<u8>, ExecutionBackendError>> + Send + '_>>
            {
                let bytes = self.read_content.lock().unwrap().clone();
                Box::pin(async move { Ok(bytes) })
            }

            fn write_file(
                &self,
                _path: &WorkspacePath,
                _content: &[u8],
            ) -> Pin<Box<dyn Future<Output = Result<(), ExecutionBackendError>> + Send + '_>>
            {
                Box::pin(async {
                    Err(ExecutionBackendError::Unavailable {
                        message: "not used".into(),
                    })
                })
            }

            fn remove_file(
                &self,
                _path: &WorkspacePath,
            ) -> Pin<Box<dyn Future<Output = Result<(), ExecutionBackendError>> + Send + '_>>
            {
                Box::pin(async {
                    Err(ExecutionBackendError::Unavailable {
                        message: "not used".into(),
                    })
                })
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
                Box::pin(async {
                    Err(ExecutionBackendError::Unavailable {
                        message: "not used".into(),
                    })
                })
            }

            fn grep(
                &self,
                _request: &GrepRequest,
            ) -> Pin<
                Box<dyn Future<Output = Result<Vec<GrepMatch>, ExecutionBackendError>> + Send + '_>,
            > {
                Box::pin(async {
                    Err(ExecutionBackendError::Unavailable {
                        message: "not used".into(),
                    })
                })
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
                Box::pin(async {
                    Err(ExecutionBackendError::Unavailable {
                        message: "not used".into(),
                    })
                })
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
                    Err(ExecutionBackendError::Unavailable {
                        message: "not used".into(),
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

        fn direct_obs_ctx(root: &str, backend: Arc<ObsSpyBackend>) -> HandlerContext {
            let (event_tx, _rx) = mpsc::channel(8);
            let (_cancel_tx, cancel_rx) = watch::channel(false);
            let policy = CepPolicyConfig::defaults_for_project_root(root);
            let cep_context = Arc::new(CepTurnContext::from_policy(policy));
            let policy_cfg =
                lancet_authority::policy::default::PolicyConfig::defaults_for_project_root(
                    PathBuf::from(root),
                );
            let default_hook = Arc::new(lancet_authority::policy::default::DefaultPolicyHook::new(
                policy_cfg,
            ));
            HandlerContext {
                call_id: 1,
                events: EventSink::new(event_tx, 1),
                cancellation: Cancellation::new(cancel_rx),
                base_envelope: BaseExecutionEnvelope::for_workspace_root(root, "phase34 obs"),
                cep_context,
                active_envelope: Arc::new(
                    lancet_authority::envelope_store::ActiveEnvelopeStore::new(),
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
                audit: Arc::new(crate::audit::CepAuditStore::in_memory().expect("audit")),
                cep_mode: lancet_protocol::policy::CepMode::Off,
                command_isolation:
                    crate::enforcement::isolation::CommandIsolationRuntimeConfig::default(),
                authority_mode: AuthorityMode::Direct,
                execution: Some(ExecutionRouting {
                    backend,
                    upper: UpperContext {
                        execution_backend: "obs-spy".into(),
                        upper_workspace_id: "upper-1".into(),
                    },
                }),
                direct_verify: None,
                routing_invocation: crate::execution_backend::RoutingInvocationState::default(),
            }
        }

        #[tokio::test]
        async fn phase34_direct_observation_returns_upper_not_lower() {
            let dir = TempDir::new().unwrap();
            fs::write(dir.path().join("conflict.txt"), "LOWER_CONTENT").unwrap();
            let root = dir.path().to_str().unwrap();
            let registry = native_registry(root);
            let backend = Arc::new(ObsSpyBackend::default());
            *backend.read_content.lock().unwrap() = b"UPPER_CONTENT".to_vec();
            let ctx = direct_obs_ctx(root, backend);
            let result = dispatch_read_file(
                &registry,
                ReadFileParams {
                    path: "conflict.txt".into(),
                    offset: Some(1),
                    limit: Some(10),
                    line_numbers: Some(false),
                },
                ctx,
            )
            .await
            .unwrap();
            assert!(result.content.contains("UPPER_CONTENT"));
            assert!(!result.content.contains("LOWER_CONTENT"));
            let lower = fs::read_to_string(dir.path().join("conflict.txt")).unwrap();
            assert_eq!(lower, "LOWER_CONTENT");
        }

        #[tokio::test]
        async fn phase34_direct_observation_policy_denial_before_preflight() {
            let dir = TempDir::new().unwrap();
            let root = dir.path().to_str().unwrap();
            let registry = native_registry(root);
            let backend = Arc::new(ObsSpyBackend::default());
            let ctx = direct_obs_ctx(root, backend);
            let routing = ctx.routing_invocation.clone();
            let err = dispatch_read_file(
                &registry,
                ReadFileParams {
                    path: "../outside".into(),
                    offset: None,
                    limit: None,
                    line_numbers: None,
                },
                ctx,
            )
            .await
            .unwrap_err();
            assert_eq!(err.code, CepErrorCode::GuardDenied);
            assert!(!routing.adapter_preflight_invoked());
        }
    }
}
