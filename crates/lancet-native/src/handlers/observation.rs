// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Native CEP workspace observation CEP handlers.
//!
//! Observation reads the one concrete [`crate::workspace::LocalWorkspace`]
//! directly: secure per-segment resolution, Guard L1 validation of the
//! resolved facts, then bounded read/list/grep/find. There is no backend
//! routing layer.

use crate::edit::content_hash;
use crate::enforcement::guard_l1::evaluate_and_enforce_policy;
use crate::executor::render::render_file_slice;
use crate::server::registry::{HandlerContext, MethodRegistry};
use crate::workspace::{
    FindRequest, GrepRequest, LocalWorkspaceError, PathFacts, PathIntent, VisibilityEngine,
    WorkspacePath, join_rel,
};
use lancet_audit::{RejectionRecord, RejectionStage};
use lancet_authority::effect::ledger::{EffectSource, ObservedEffect, ObservedScope};
use lancet_authority::effect::region::AuthorizedEffectRegion;
use lancet_authority::guard::validate_path_facts;
use lancet_authority::policy::CepPolicyConfig;
use lancet_authority::policy::budget::BudgetExhausted;
use lancet_authority::{HiddenReason, ReadVisibility, ReadVisibilityPolicy};
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

#[allow(dead_code)]
const GREP_MAX_DEPTH: u32 = 32;
const DEFAULT_GREP_LIMIT: usize = 200;
const DEFAULT_LIST_LIMIT: usize = 200;

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

    handle_read_file(params, ctx, method).await
}

async fn handle_grep_as(
    params: GrepParams,
    ctx: HandlerContext,
    method: &'static str,
    policy_params: &impl Serialize,
) -> Result<GrepResult, CepError> {
    let _evaluation = evaluate_and_enforce_policy(&ctx, method, policy_params).await?;
    let path = params.path.clone().unwrap_or_else(|| ".".into());
    handle_grep(params, ctx, &path, method).await
}

async fn handle_list_directory_as(
    params: ListDirectoryParams,
    ctx: HandlerContext,
    method: &'static str,
    policy_params: &impl Serialize,
) -> Result<ListDirectoryResult, CepError> {
    let _evaluation = evaluate_and_enforce_policy(&ctx, method, policy_params).await?;

    handle_list_directory(params, ctx, method).await
}

async fn handle_find_files_as(
    params: FindFilesParams,
    ctx: HandlerContext,
    method: &'static str,
    policy_params: &impl Serialize,
) -> Result<FindFilesResult, CepError> {
    let path = params.path.clone().unwrap_or_else(|| ".".into());
    let _evaluation = evaluate_and_enforce_policy(&ctx, method, policy_params).await?;

    handle_find_files(params, ctx, &path, method).await
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

fn validate_observation_facts(path: &str, facts: &PathFacts) -> Result<(), CepError> {
    validate_path_facts(path, &facts.to_guard(), &AuthorizedEffectRegion::empty())
}

fn observation_workspace_error(path: &str, err: LocalWorkspaceError) -> CepError {
    CepError::new(
        CepErrorCode::ExecutionFailed,
        format!("observation failed for '{path}': {err}"),
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
        format!("workspace returned leaking path '{path}' for request '{requested}'"),
        CepErrorData::Execution(lancet_protocol::error::ExecutionErrorData {
            command: "observe.workspace".into(),
            exit_code: None,
            stderr: Some("path leak".into()),
        }),
        Retriability::Permanent,
    )
}

fn normalize_workspace_path(path: &str) -> Result<String, CepError> {
    let parsed = WorkspacePath::parse(path).map_err(|_| reject_leaking_path(path, path))?;
    Ok(parsed.as_str().to_string())
}

fn convert_directory_entry(entry: &crate::workspace::DirectoryEntry) -> DirectoryEntry {
    DirectoryEntry {
        name: entry.name.clone(),
        kind: match entry.kind {
            crate::workspace::PathKind::File => DirectoryEntryKind::File,
            crate::workspace::PathKind::Directory => DirectoryEntryKind::Directory,
            crate::workspace::PathKind::Symlink => DirectoryEntryKind::Symlink,
        },
    }
}

async fn record_observation_denial(
    ctx: &HandlerContext,
    method: &str,
    stage: RejectionStage,
    denial_source: &str,
) {
    ctx.record_rejection(RejectionRecord {
        rejection_stage: stage,
        denial_source: denial_source.into(),
    });
    emit_denial_metadata(ctx, method, denial_source).await;
}

async fn emit_success_metadata(
    ctx: &HandlerContext,
    method: &str,
    request_path: &str,
    guarded_path: &str,
    mut metadata: serde_json::Value,
) {
    if let Some(obj) = metadata.as_object_mut() {
        obj.insert("request_target".to_string(), json!(request_path));
        obj.insert("guarded_path".to_string(), json!(guarded_path));
    }
    ctx.record_durable_metadata(metadata.clone());
    ctx.events
        .emit(
            "cep.native_observation_metadata",
            json!({ "method": method, "metadata": metadata }),
        )
        .await;
}

fn read_visibility_policy(ctx: &HandlerContext) -> ReadVisibilityPolicy {
    ctx.default_policy_hook
        .as_ref()
        .and_then(|hook| hook.config().workspace.read_visibility_policy().ok())
        .unwrap_or_default()
}

fn load_visibility(ctx: &HandlerContext) -> VisibilityEngine {
    VisibilityEngine::load(ctx.workspace.root(), read_visibility_policy(ctx))
}

async fn deny_hidden(
    ctx: &HandlerContext,
    method: &str,
    request_path: &str,
    reason: HiddenReason,
) -> CepError {
    record_observation_denial(
        ctx,
        method,
        RejectionStage::PathGuard,
        reason.as_audit_label(),
    )
    .await;
    // Agent-facing shape matches a missing path; reason is audit-only.
    let _ = request_path;
    path_not_found(request_path)
}

/// Resolve `ws_path` through the concrete Local Workspace and validate the
/// observed facts against Guard L1, recording denial evidence on failure.
async fn resolve_and_validate(
    ctx: &HandlerContext,
    ws_path: &WorkspacePath,
    intent: PathIntent,
    request_path: &str,
    method: &str,
) -> Result<PathFacts, CepError> {
    let facts = match ctx.workspace.resolve_facts(ws_path, intent).await {
        Ok(facts) => facts,
        Err(err) => {
            record_observation_denial(ctx, method, RejectionStage::PathGuard, "local_workspace")
                .await;
            return Err(observation_workspace_error(request_path, err));
        }
    };
    if let Err(err) = validate_observation_facts(ws_path.as_str(), &facts) {
        record_observation_denial(ctx, method, RejectionStage::PathGuard, "guard_l1").await;
        return Err(err);
    }
    Ok(facts)
}

async fn handle_read_file(
    params: ReadFileParams,
    ctx: HandlerContext,
    method: &str,
) -> Result<ReadFileResult, CepError> {
    let policy = &ctx.cep_context.policy;
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

    let facts =
        resolve_and_validate(&ctx, &ws_path, PathIntent::Read, &params.path, method).await?;
    if !facts.exists || !matches!(facts.kind, Some(crate::workspace::PathKind::File)) {
        return Err(path_not_found(&params.path));
    }
    let mut visibility = load_visibility(&ctx);
    if let ReadVisibility::Hidden(reason) = visibility.decide(ws_path.as_str(), false) {
        return Err(deny_hidden(&ctx, method, &params.path, reason).await);
    }
    if let Some(size) = facts.byte_size
        && size as usize > policy.observation.max_single_read_bytes
    {
        return Err(single_read_budget_exceeded(size as usize, policy));
    }

    let bytes = ctx
        .workspace
        .read_file(&ws_path)
        .await
        .map_err(|err| observation_workspace_error(&params.path, err))?;
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

    emit_success_metadata(
        &ctx,
        method,
        &params.path,
        ws_path.as_str(),
        json!({
            "observation_source": "workspace",
            "bytes_read": content.len(),
            "bytes_returned": bytes_returned,
            "content_hash": hash,
            "truncated": truncated,
            "policy_enforced": true,
        }),
    )
    .await;

    Ok(ReadFileResult {
        content: rendered,
        content_hash: Some(hash),
    })
}

async fn handle_list_directory(
    params: ListDirectoryParams,
    ctx: HandlerContext,
    method: &str,
) -> Result<ListDirectoryResult, CepError> {
    let policy = &ctx.cep_context.policy;
    let raw_path = if params.path.is_empty() {
        ".".to_string()
    } else {
        params.path.clone()
    };
    let ws_path = parse_observation_path(&raw_path, "path")?;

    let facts =
        resolve_and_validate(&ctx, &ws_path, PathIntent::ListDirectory, &raw_path, method).await?;
    if !facts.exists || !matches!(facts.kind, Some(crate::workspace::PathKind::Directory)) {
        return Err(path_not_found(&raw_path));
    }
    {
        let mut visibility = load_visibility(&ctx);
        if let ReadVisibility::Hidden(reason) = visibility.decide(ws_path.as_str(), true) {
            // Navigable-only hidden dirs (include reachability) may still be listed.
            if !visibility.is_listable(ws_path.as_str(), true) {
                return Err(deny_hidden(&ctx, method, &raw_path, reason).await);
            }
        }
    }

    let limit = params
        .limit
        .and_then(|v| usize::try_from(v).ok())
        .unwrap_or(DEFAULT_LIST_LIMIT)
        .clamp(1, policy.observation.max_list_entries);

    let workspace_entries = ctx
        .workspace
        .list_dir(&ws_path)
        .await
        .map_err(|err| observation_workspace_error(&raw_path, err))?;
    let mut visibility = load_visibility(&ctx);
    let parent_rel = ws_path.as_str();
    let mut entries = Vec::new();
    for entry in &workspace_entries {
        let child_rel = join_rel(parent_rel, &entry.name);
        let is_dir = matches!(entry.kind, crate::workspace::PathKind::Directory);
        if visibility.is_listable(&child_rel, is_dir) {
            entries.push(convert_directory_entry(entry));
        }
    }
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

    emit_success_metadata(
        &ctx,
        method,
        &raw_path,
        ws_path.as_str(),
        json!({
            "observation_source": "workspace",
            "entry_count": total,
            "bytes_returned": bytes_returned,
            "truncated": truncated,
            "policy_enforced": true,
        }),
    )
    .await;

    Ok(ListDirectoryResult {
        entries,
        truncated: truncated.then_some(true),
    })
}

async fn handle_grep(
    params: GrepParams,
    ctx: HandlerContext,
    path: &str,
    method: &str,
) -> Result<GrepResult, CepError> {
    let policy = &ctx.cep_context.policy;
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

    let facts =
        resolve_and_validate(&ctx, &ws_path, PathIntent::ListDirectory, path, method).await?;
    if !facts.exists || !matches!(facts.kind, Some(crate::workspace::PathKind::Directory)) {
        return Err(path_not_found(path));
    }
    {
        let mut visibility = load_visibility(&ctx);
        if let ReadVisibility::Hidden(reason) = visibility.decide(ws_path.as_str(), true) {
            if !visibility.is_listable(ws_path.as_str(), true) {
                return Err(deny_hidden(&ctx, method, path, reason).await);
            }
        }
    }

    let max_results = params
        .max_results
        .and_then(|v| usize::try_from(v).ok())
        .unwrap_or(DEFAULT_GREP_LIMIT)
        .clamp(1, policy.observation.max_grep_matches);

    let request = GrepRequest {
        pattern: params.pattern.clone(),
        path: ws_path.clone(),
        glob: None,
    };
    let workspace_matches = ctx
        .workspace
        .grep(&request, max_results, read_visibility_policy(&ctx))
        .await
        .map_err(|err| observation_workspace_error(path, err))?;

    let mut matches = Vec::new();
    let mut truncated = workspace_matches.len() >= max_results;
    for m in workspace_matches {
        let rel = normalize_workspace_path(m.path.as_str())?;
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

    emit_success_metadata(
        &ctx,
        method,
        path,
        ws_path.as_str(),
        json!({
            "observation_source": "workspace",
            "match_count": matches.len(),
            "bytes_returned": bytes_returned,
            "truncated": truncated,
            "policy_enforced": true,
        }),
    )
    .await;

    Ok(GrepResult {
        matches,
        truncated: truncated.then_some(true),
    })
}

async fn handle_find_files(
    params: FindFilesParams,
    ctx: HandlerContext,
    path: &str,
    method: &str,
) -> Result<FindFilesResult, CepError> {
    let policy = &ctx.cep_context.policy;
    let ws_path = parse_observation_path(path, "path")?;

    let facts =
        resolve_and_validate(&ctx, &ws_path, PathIntent::ListDirectory, path, method).await?;
    if !facts.exists || !matches!(facts.kind, Some(crate::workspace::PathKind::Directory)) {
        return Err(path_not_found(path));
    }
    {
        let mut visibility = load_visibility(&ctx);
        if let ReadVisibility::Hidden(reason) = visibility.decide(ws_path.as_str(), true) {
            if !visibility.is_listable(ws_path.as_str(), true) {
                return Err(deny_hidden(&ctx, method, path, reason).await);
            }
        }
    }

    let limit = params
        .limit
        .and_then(|v| usize::try_from(v).ok())
        .unwrap_or(DEFAULT_LIST_LIMIT)
        .clamp(1, policy.observation.max_find_paths);

    let request = FindRequest {
        pattern: params.pattern.clone(),
        path: ws_path.clone(),
    };
    let workspace_paths = ctx
        .workspace
        .find_files(&request, limit, read_visibility_policy(&ctx))
        .await
        .map_err(|err| observation_workspace_error(path, err))?;

    let mut paths = Vec::new();
    let mut truncated = workspace_paths.len() >= limit;
    for p in workspace_paths {
        paths.push(normalize_workspace_path(p.as_str())?);
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

    emit_success_metadata(
        &ctx,
        method,
        path,
        ws_path.as_str(),
        json!({
            "observation_source": "workspace",
            "path_count": paths.len(),
            "bytes_returned": bytes_returned,
            "truncated": truncated,
            "policy_enforced": true,
        }),
    )
    .await;

    Ok(FindFilesResult {
        paths,
        truncated: truncated.then_some(true),
    })
}

async fn emit_denial_metadata(ctx: &HandlerContext, method: &str, denial_source: &str) {
    let metadata = json!({
        "method": method,
        "denial_source": denial_source,
        "policy_enforced": true,
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
    use crate::workspace::LocalWorkspace;
    use lancet_authority::policy::CepTurnContext;
    use lancet_authority::policy::observation::reject_parent_traversal;
    use std::fs;
    use std::path::Path;
    use std::sync::Arc;
    use tempfile::TempDir;
    use tokio::sync::{mpsc, watch};

    fn native_registry() -> MethodRegistry {
        let mut registry = MethodRegistry::new();
        register_workspace_observation_methods(&mut registry).unwrap();
        registry
    }

    fn local_workspace(root: &str) -> Arc<LocalWorkspace> {
        Arc::new(LocalWorkspace::new(Path::new(root)).expect("local workspace"))
    }

    fn ctx_for(root: &str) -> (HandlerContext, mpsc::Receiver<lancet_protocol::ToolEvent>) {
        ctx_for_policy(CepPolicyConfig::defaults_for_project_root(root), root)
    }

    fn ctx_for_workspace(
        root: &str,
        workspace: lancet_authority::policy::default::WorkspacePolicy,
    ) -> (HandlerContext, mpsc::Receiver<lancet_protocol::ToolEvent>) {
        ctx_for_policy_workspace(
            CepPolicyConfig::defaults_for_project_root(root),
            root,
            workspace,
        )
    }

    fn ctx_for_policy(
        policy: CepPolicyConfig,
        root: &str,
    ) -> (HandlerContext, mpsc::Receiver<lancet_protocol::ToolEvent>) {
        ctx_for_policy_workspace(policy, root, Default::default())
    }

    fn ctx_for_policy_workspace(
        policy: CepPolicyConfig,
        root: &str,
        workspace: lancet_authority::policy::default::WorkspacePolicy,
    ) -> (HandlerContext, mpsc::Receiver<lancet_protocol::ToolEvent>) {
        let (event_tx, event_rx) = mpsc::channel(32);
        let (_cancel_tx, cancel_rx) = watch::channel(false);
        let cep_context = Arc::new(CepTurnContext::from_policy(policy));
        let policy_cfg = lancet_authority::policy::default::PolicyConfig {
            capability_sources: Default::default(),
            methods: Default::default(),
            network: cep_context.policy.network.clone(),
            budget: cep_context.policy.budget.clone(),
            observation: cep_context.policy.observation.clone(),
            workspace,
            project_root: cep_context.policy.project_root.clone(),
            denied_effect_kinds: Default::default(),
        };
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
        let registry = native_registry();
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
    async fn native_read_file_reads_current_disk_state() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("watch.txt"), "v1").unwrap();
        let root = dir.path().to_str().unwrap();
        let registry = native_registry();
        let (ctx, _events) = ctx_for(root);

        let params = ReadFileParams {
            path: "watch.txt".into(),
            offset: Some(1),
            limit: Some(10),
            line_numbers: Some(false),
        };
        let first = dispatch_read_file(&registry, params.clone(), ctx.clone())
            .await
            .unwrap();
        assert!(first.content.contains("v1"));

        // An external writer changes the file between Tool calls; the next
        // observation reads the current Local state.
        fs::write(dir.path().join("watch.txt"), "v2-external").unwrap();
        let second = dispatch_read_file(&registry, params, ctx).await.unwrap();
        assert!(second.content.contains("v2-external"));
        assert!(!second.content.contains("v1"));
    }

    #[tokio::test]
    async fn native_read_file_rejects_parent_traversal_before_workspace_io() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().to_str().unwrap();
        let registry = native_registry();
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
        let registry = native_registry();
        let (event_tx, _rx) = mpsc::channel(8);
        let (_cancel_tx, cancel_rx) = watch::channel(false);
        let cep_context = Arc::new(policy_with_limits(dir.path(), 50, 10_000));
        let ctx = HandlerContext {
            call_id: 2,
            events: EventSink::new(event_tx, 2),
            cancellation: Cancellation::new(cancel_rx),
            cep_context,
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
            workspace: local_workspace(root),
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
        let registry = native_registry();
        let (event_tx, _rx) = mpsc::channel(8);
        let (_cancel_tx, cancel_rx) = watch::channel(false);
        let cep_context = Arc::new(policy_with_limits(dir.path(), 10_000, 5));
        let ctx = HandlerContext {
            call_id: 3,
            events: EventSink::new(event_tx, 3),
            cancellation: Cancellation::new(cancel_rx),
            cep_context,
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
            workspace: local_workspace(root),
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
    async fn native_workspace_observation_grep_rejects_parent_traversal() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().to_str().unwrap();
        let registry = native_registry();
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
        let registry = native_registry();
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
        let registry = native_registry();
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
        let registry = native_registry();
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
        // Match paths are workspace-root-relative (the concrete Local
        // Workspace reports paths beneath the canonical root).
        assert_eq!(grep.matches[0].path, "src/allowed.txt");

        let effects = ctx.effect_ledger.snapshot();
        assert!(
            effects.iter().any(|effect| matches!(
                &effect.scope,
                ObservedScope::Root(root) if root == "src"
            )),
            "expected observation effect scoped to src, got {effects:?}"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn native_recursive_observation_does_not_follow_symlink_outside_allowed_root() {
        let dir = TempDir::new().unwrap();
        let src = dir.path().join("src");
        fs::create_dir(&src).unwrap();
        fs::write(src.join("allowed.txt"), "SECRET_ALLOWED=yes\n").unwrap();
        // Create a symlink inside src that points outside the workspace root.
        let outside = TempDir::new().unwrap();
        fs::write(outside.path().join("secret.txt"), "SECRET_FORBIDDEN=yes\n").unwrap();
        std::os::unix::fs::symlink(
            outside.path().join("secret.txt"),
            src.join("link_to_secret.txt"),
        )
        .unwrap();

        let root = dir.path().to_str().unwrap();
        let registry = native_registry();
        let (ctx, _events) = ctx_for(root);

        // Reading through the symlink fails: the concrete Local Workspace
        // rejects the symlink leaf during secure resolution, and the denial
        // is recorded with the path-guard stage.
        let read_params = ReadFileParams {
            path: "src/link_to_secret.txt".into(),
            offset: None,
            limit: None,
            line_numbers: None,
        };
        let durable = ctx.durable_metadata.clone();
        let read_err = dispatch_read_file(&registry, read_params, ctx.clone())
            .await
            .unwrap_err();
        assert_eq!(
            read_err.code,
            CepErrorCode::ExecutionFailed,
            "symlink leaf rejection surfaces as a workspace resolution failure, got {read_err:?}"
        );
        assert!(
            read_err.message.contains("symlink"),
            "expected symlink rejection, got: {}",
            read_err.message
        );
        let metadata: serde_json::Value =
            serde_json::from_str(&durable.take_json().expect("durable metadata")).unwrap();
        assert_eq!(metadata["rejection_stage"], "path_guard");
        assert_eq!(metadata["denial_source"], "local_workspace");

        // Grep in src only finds allowed.txt: the symlink is never followed.
        let grep_params = GrepParams {
            pattern: "SECRET".into(),
            path: Some("src".into()),
            max_results: Some(10),
        };
        let grep = dispatch_grep(&registry, grep_params, ctx.clone())
            .await
            .unwrap();
        assert_eq!(grep.matches.len(), 1);
        assert_eq!(grep.matches[0].path, "src/allowed.txt");
        assert_eq!(grep.matches[0].text, "SECRET_ALLOWED=yes");

        // Find lists names only; the symlink target's content is never read.
        let find_params = FindFilesParams {
            pattern: "*allowed.txt".into(),
            path: Some("src".into()),
            limit: Some(10),
        };
        let find = dispatch_find_files(&registry, find_params, ctx)
            .await
            .unwrap();
        assert_eq!(find.paths, vec!["src/allowed.txt".to_string()]);
    }

    #[tokio::test]
    async fn native_workspace_observation_list_directory_rejects_parent_traversal() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().to_str().unwrap();
        let registry = native_registry();
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
        let registry = native_registry();
        let (event_tx, _rx) = mpsc::channel(8);
        let (_cancel_tx, cancel_rx) = watch::channel(false);
        let mut policy = CepPolicyConfig::defaults_for_project_root(dir.path());
        policy.observation.max_grep_matches = 2;
        policy.budget.max_grep_matches = 2;
        let ctx = HandlerContext {
            call_id: 4,
            events: EventSink::new(event_tx, 4),
            cancellation: Cancellation::new(cancel_rx),
            cep_context: Arc::new(CepTurnContext::from_policy(policy)),
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
            workspace: local_workspace(root),
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

    #[tokio::test]
    async fn observation_policy_denial_records_no_observation_metadata() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().to_str().unwrap();
        let registry = native_registry();
        let (ctx, _events) = ctx_for(root);
        let durable = ctx.durable_metadata.clone();
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
        // Policy denial precedes any workspace I/O: no observation success or
        // denial metadata is emitted beyond the policy evidence itself.
        let metadata: serde_json::Value =
            serde_json::from_str(&durable.take_json().expect("durable metadata")).unwrap();
        assert!(metadata.get("observation_source").is_none());
        assert_eq!(metadata["denial_source"], "cep_policy");
    }

    fn init_git_ignore_fixture() -> TempDir {
        use std::process::Command;
        let dir = TempDir::new().unwrap();
        let root = dir.path();
        assert!(
            Command::new("git")
                .args(["init"])
                .current_dir(root)
                .status()
                .unwrap()
                .success()
        );
        fs::write(root.join(".gitignore"), ".env\nsecret/\n").unwrap();
        fs::write(root.join("visible.txt"), "hello visible\n").unwrap();
        fs::write(root.join(".env"), "SECRET=1\n").unwrap();
        fs::create_dir(root.join("secret")).unwrap();
        fs::write(root.join("secret/key"), "hidden-key\n").unwrap();
        fs::write(root.join("secret/other"), "other\n").unwrap();
        assert!(
            Command::new("git")
                .args(["add", "visible.txt", ".gitignore"])
                .current_dir(root)
                .status()
                .unwrap()
                .success()
        );
        assert!(
            Command::new("git")
                .args([
                    "-c",
                    "user.email=t@t",
                    "-c",
                    "user.name=t",
                    "commit",
                    "-m",
                    "init"
                ])
                .current_dir(root)
                .status()
                .unwrap()
                .success()
        );
        dir
    }

    #[tokio::test]
    async fn policy_visible_secret_sentinel_hidden_on_all_four_ops() {
        let dir = init_git_ignore_fixture();
        let root = dir.path().to_str().unwrap();
        let registry = native_registry();
        let (ctx, _) = ctx_for(root);

        let read_err = dispatch_read_file(
            &registry,
            ReadFileParams {
                path: ".env".into(),
                offset: None,
                limit: None,
                line_numbers: None,
            },
            ctx.clone(),
        )
        .await
        .unwrap_err();
        assert!(
            read_err.message.contains("not_found"),
            "hidden read must look like missing: {}",
            read_err.message
        );

        let list = dispatch_list_directory(
            &registry,
            ListDirectoryParams {
                path: ".".into(),
                limit: Some(50),
            },
            ctx.clone(),
        )
        .await
        .unwrap();
        let names: Vec<_> = list.entries.iter().map(|e| e.name.as_str()).collect();
        assert!(names.contains(&"visible.txt"));
        assert!(!names.contains(&".env"));
        assert!(!names.contains(&"secret"));

        let grep = dispatch_grep(
            &registry,
            GrepParams {
                pattern: "SECRET|hidden".into(),
                path: Some(".".into()),
                max_results: Some(20),
            },
            ctx.clone(),
        )
        .await
        .unwrap();
        assert!(
            grep.matches.is_empty(),
            "hidden files must not produce matches: {:?}",
            grep.matches
        );

        let find = dispatch_find_files(
            &registry,
            FindFilesParams {
                pattern: "*".into(),
                path: Some(".".into()),
                limit: Some(50),
            },
            ctx,
        )
        .await
        .unwrap();
        assert!(
            find.paths
                .iter()
                .all(|p| p != ".env" && !p.starts_with("secret")),
            "find leaked hidden paths: {:?}",
            find.paths
        );
    }

    #[tokio::test]
    async fn policy_visible_include_and_denial_precedence() {
        let dir = init_git_ignore_fixture();
        let root = dir.path().to_str().unwrap();
        let registry = native_registry();
        let (ctx, _) = ctx_for_workspace(
            root,
            lancet_authority::policy::default::WorkspacePolicy {
                included_ignored_read_patterns: vec![".env".into(), "secret/**".into()],
                denied_read_patterns: vec!["secret/other".into()],
                ..Default::default()
            },
        );

        let env = dispatch_read_file(
            &registry,
            ReadFileParams {
                path: ".env".into(),
                offset: None,
                limit: None,
                line_numbers: None,
            },
            ctx.clone(),
        )
        .await
        .unwrap();
        assert!(env.content.contains("SECRET=1"));

        let key = dispatch_read_file(
            &registry,
            ReadFileParams {
                path: "secret/key".into(),
                offset: None,
                limit: None,
                line_numbers: None,
            },
            ctx.clone(),
        )
        .await
        .unwrap();
        assert!(key.content.contains("hidden-key"));

        let denied = dispatch_read_file(
            &registry,
            ReadFileParams {
                path: "secret/other".into(),
                offset: None,
                limit: None,
                line_numbers: None,
            },
            ctx.clone(),
        )
        .await
        .unwrap_err();
        assert!(denied.message.contains("not_found"), "{}", denied.message);

        let list = dispatch_list_directory(
            &registry,
            ListDirectoryParams {
                path: "secret".into(),
                limit: Some(20),
            },
            ctx,
        )
        .await
        .unwrap();
        let names: Vec<_> = list.entries.iter().map(|e| e.name.as_str()).collect();
        assert!(names.contains(&"key"));
        assert!(!names.contains(&"other"));
    }

    #[tokio::test]
    async fn policy_visible_system_exclusion_beats_include() {
        let dir = init_git_ignore_fixture();
        let root = dir.path().to_str().unwrap();
        let registry = native_registry();
        let (ctx, _) = ctx_for_workspace(
            root,
            lancet_authority::policy::default::WorkspacePolicy {
                included_ignored_read_patterns: vec![".git/**".into(), ".git/config".into()],
                ..Default::default()
            },
        );

        let err = dispatch_read_file(
            &registry,
            ReadFileParams {
                path: ".git/config".into(),
                offset: None,
                limit: None,
                line_numbers: None,
            },
            ctx.clone(),
        )
        .await
        .unwrap_err();
        assert!(err.message.contains("not_found"), "{}", err.message);

        let list = dispatch_list_directory(
            &registry,
            ListDirectoryParams {
                path: ".".into(),
                limit: Some(50),
            },
            ctx,
        )
        .await
        .unwrap();
        assert!(!list.entries.iter().any(|e| e.name == ".git"));
    }

    #[tokio::test]
    async fn policy_visible_four_ops_agree_on_same_path() {
        let dir = init_git_ignore_fixture();
        let root = dir.path().to_str().unwrap();
        let registry = native_registry();
        let (ctx, _) = ctx_for(root);

        let read = dispatch_read_file(
            &registry,
            ReadFileParams {
                path: ".env".into(),
                offset: None,
                limit: None,
                line_numbers: None,
            },
            ctx.clone(),
        )
        .await;
        assert!(read.is_err());

        let list = dispatch_list_directory(
            &registry,
            ListDirectoryParams {
                path: ".".into(),
                limit: Some(50),
            },
            ctx.clone(),
        )
        .await
        .unwrap();
        assert!(!list.entries.iter().any(|e| e.name == ".env"));

        let grep = dispatch_grep(
            &registry,
            GrepParams {
                pattern: "SECRET".into(),
                path: Some(".".into()),
                max_results: Some(10),
            },
            ctx.clone(),
        )
        .await
        .unwrap();
        assert!(grep.matches.iter().all(|m| m.path != ".env"));

        let find = dispatch_find_files(
            &registry,
            FindFilesParams {
                pattern: ".env".into(),
                path: Some(".".into()),
                limit: Some(10),
            },
            ctx,
        )
        .await
        .unwrap();
        assert!(!find.paths.iter().any(|p| p == ".env"));
    }
}
