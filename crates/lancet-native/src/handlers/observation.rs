//! Native CEP workspace observation CEP handlers.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::edit::content_hash;
use crate::enforcement::guard_l1::evaluate_and_enforce_policy;
use crate::executor::render::render_file_slice;
use crate::server::registry::{HandlerContext, MethodRegistry};
use lancet_authority::effect::ledger::{EffectSource, ObservedEffect, ObservedScope};
use lancet_authority::policy::budget::BudgetExhausted;
use lancet_authority::policy::observation::{
    resolve_observation_target, ResolvedObservationTarget,
};
use lancet_authority::policy::CepPolicyConfig;
use lancet_catalog::method::RequiredCapability;
use lancet_catalog::methods::workspace::{
    DirectoryEntry, DirectoryEntryKind, FindFilesParams, FindFilesResult, GrepMatch, GrepParams,
    GrepResult, ListDirectoryParams, ListDirectoryResult, ObserveWorkspace, ObserveWorkspaceParams,
    ObserveWorkspaceResult, ReadFileParams, ReadFileResult,
};
use lancet_catalog::CepMethod;
use lancet_protocol::envelope::ImpactKind;
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

    let policy = &ctx.cep_context.policy;
    let target = match resolve_observation_target(policy, &params.path).await {
        Ok(t) => t,
        Err(err) => {
            emit_denial_metadata(&ctx, method, "cep_policy", false).await;
            return Err(err);
        }
    };
    #[cfg(test)]
    if let Some(err) = check_backend_safety_net(&params.path) {
        emit_denial_metadata(&ctx, method, "backend_safety_net", true).await;
        return Err(err);
    }

    let meta = tokio::fs::metadata(&target.canonical_target)
        .await
        .map_err(|_| path_not_found(&params.path))?;
    if !meta.is_file() {
        return Err(path_not_found(&params.path));
    }
    if meta.len() as usize > policy.observation.max_single_read_bytes {
        return Err(single_read_budget_exceeded(meta.len() as usize, policy));
    }

    let content = tokio::fs::read_to_string(&target.canonical_target)
        .await
        .map_err(|e| io_validation_error(&params.path, e))?;
    if content.len() > policy.observation.max_single_read_bytes {
        return Err(single_read_budget_exceeded(content.len(), policy));
    }

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
        scope: ObservedScope::Path(target.canonical_target.clone()),
        required_capability: RequiredCapability::ReadPath,
        source: EffectSource::HandlerBackend,
    });

    emit_success_metadata(
        &ctx,
        method,
        &target,
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

async fn handle_grep_as(
    params: GrepParams,
    ctx: HandlerContext,
    method: &'static str,
    policy_params: &impl Serialize,
) -> Result<GrepResult, CepError> {
    let _evaluation = evaluate_and_enforce_policy(&ctx, method, policy_params).await?;
    let path = params.path.clone().unwrap_or_else(|| ".".into());
    run_grep(params, ctx, &path, method).await
}

async fn handle_list_directory_as(
    params: ListDirectoryParams,
    ctx: HandlerContext,
    method: &'static str,
    policy_params: &impl Serialize,
) -> Result<ListDirectoryResult, CepError> {
    let _evaluation = evaluate_and_enforce_policy(&ctx, method, policy_params).await?;

    let policy = &ctx.cep_context.policy;
    let target = resolve_target_or_deny(policy, &params.path, method, &ctx).await?;
    backend_or_continue(&params.path, method, &ctx).await?;

    let limit = params
        .limit
        .and_then(|v| usize::try_from(v).ok())
        .unwrap_or(DEFAULT_LIST_LIMIT)
        .clamp(1, policy.observation.max_list_entries);

    let mut entries = list_directory_entries(&target.canonical_target).await?;
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
        scope: ObservedScope::Root(observation_root_scope(&target)),
        required_capability: RequiredCapability::Inspect,
        source: EffectSource::HandlerBackend,
    });

    emit_success_metadata(
        &ctx,
        method,
        &target,
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

async fn handle_find_files_as(
    params: FindFilesParams,
    ctx: HandlerContext,
    method: &'static str,
    policy_params: &impl Serialize,
) -> Result<FindFilesResult, CepError> {
    let path = params.path.as_deref().unwrap_or(".");
    let _evaluation = evaluate_and_enforce_policy(&ctx, method, policy_params).await?;

    let policy = &ctx.cep_context.policy;
    let target = resolve_target_or_deny(policy, path, method, &ctx).await?;
    backend_or_continue(path, method, &ctx).await?;

    let limit = params
        .limit
        .and_then(|v| usize::try_from(v).ok())
        .unwrap_or(DEFAULT_LIST_LIMIT)
        .clamp(1, policy.observation.max_find_paths);

    let (paths, paths_visited, truncated) = find_files_at(
        &target.canonical_target,
        &target.allowed_root,
        &params.pattern,
        limit,
    )
    .await;

    let bytes_returned = paths.iter().map(|p| p.len()).sum::<usize>();
    charge_observation_bytes(&ctx, bytes_returned)?;

    ctx.record_effect(ObservedEffect {
        kind: ImpactKind::WorkspaceObservation,
        scope: ObservedScope::Root(observation_root_scope(&target)),
        required_capability: RequiredCapability::Inspect,
        source: EffectSource::HandlerBackend,
    });

    emit_success_metadata(
        &ctx,
        method,
        &target,
        json!({
            "observation_source": "workspace",
            "paths_visited": paths_visited,
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

async fn run_grep(
    params: GrepParams,
    ctx: HandlerContext,
    path: &str,
    method: &'static str,
) -> Result<GrepResult, CepError> {
    let policy = &ctx.cep_context.policy;
    let target = resolve_target_or_deny(policy, path, method, &ctx).await?;
    backend_or_continue(path, method, &ctx).await?;

    let re = Regex::new(&params.pattern).map_err(|e| {
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

    let max_results = params
        .max_results
        .and_then(|v| usize::try_from(v).ok())
        .unwrap_or(DEFAULT_GREP_LIMIT)
        .clamp(1, policy.observation.max_grep_matches);

    let (matches, bytes_scanned, truncated) = grep_at(
        &target.canonical_target,
        &target.allowed_root,
        &re,
        max_results,
        policy.observation.max_single_read_bytes,
    )
    .await;

    let bytes_returned = matches
        .iter()
        .map(|m| m.path.len() + m.text.len() + 16)
        .sum::<usize>();
    charge_observation_bytes(&ctx, bytes_returned)?;

    ctx.record_effect(ObservedEffect {
        kind: ImpactKind::WorkspaceObservation,
        scope: ObservedScope::Root(observation_root_scope(&target)),
        required_capability: RequiredCapability::Inspect,
        source: EffectSource::HandlerBackend,
    });

    emit_success_metadata(
        &ctx,
        method,
        &target,
        json!({
            "observation_source": "workspace",
            "bytes_scanned": bytes_scanned,
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

async fn resolve_target_or_deny(
    policy: &CepPolicyConfig,
    path: &str,
    method: &str,
    ctx: &HandlerContext,
) -> Result<ResolvedObservationTarget, CepError> {
    match resolve_observation_target(policy, path).await {
        Ok(t) => Ok(t),
        Err(err) => {
            emit_denial_metadata(ctx, method, "cep_policy", false).await;
            Err(err)
        }
    }
}

#[cfg(test)]
async fn backend_or_continue(
    path: &str,
    method: &str,
    ctx: &HandlerContext,
) -> Result<(), CepError> {
    if let Some(err) = check_backend_safety_net(path) {
        emit_denial_metadata(ctx, method, "backend_safety_net", true).await;
        return Err(err);
    }
    Ok(())
}

#[cfg(not(test))]
async fn backend_or_continue(
    _path: &str,
    _method: &str,
    _ctx: &HandlerContext,
) -> Result<(), CepError> {
    Ok(())
}

fn charge_observation_bytes(ctx: &HandlerContext, bytes: usize) -> Result<(), CepError> {
    ctx.cep_context
        .budget
        .try_charge_observation(bytes)
        .map_err(|e: BudgetExhausted| e.to_cep_error())
}

fn observation_root_scope(target: &ResolvedObservationTarget) -> String {
    target.canonical_target.to_string_lossy().to_string()
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

fn io_validation_error(path: &str, err: std::io::Error) -> CepError {
    CepError::new(
        CepErrorCode::TypeError,
        format!("io error reading '{path}': {err}"),
        CepErrorData::Validation(ValidationErrorData {
            field: "path".into(),
            reason: err.to_string(),
        }),
        Retriability::Permanent,
    )
}

async fn list_directory_entries(dir: &Path) -> Result<Vec<DirectoryEntry>, CepError> {
    let mut out = Vec::new();
    let mut entries = tokio::fs::read_dir(dir)
        .await
        .map_err(|e| io_validation_error(&dir.display().to_string(), e))?;
    while let Some(entry) = entries
        .next_entry()
        .await
        .map_err(|e| io_validation_error(&dir.display().to_string(), e))?
    {
        let name = entry.file_name().to_string_lossy().into_owned();
        let kind = match entry.file_type().await {
            Ok(ft) if ft.is_dir() => DirectoryEntryKind::Directory,
            Ok(ft) if ft.is_symlink() => DirectoryEntryKind::Symlink,
            Ok(ft) if ft.is_file() => DirectoryEntryKind::File,
            _ => DirectoryEntryKind::Other,
        };
        out.push(DirectoryEntry { name, kind });
    }
    Ok(out)
}

async fn grep_at(
    root: &Path,
    allowed_root: &Path,
    pattern: &Regex,
    max_results: usize,
    max_read_bytes: usize,
) -> (Vec<GrepMatch>, usize, bool) {
    let mut matches = Vec::new();
    let mut visited = HashSet::new();
    let mut bytes_scanned = 0usize;
    let mut truncated = false;
    let mut ctx = GrepWalkContext {
        root,
        pattern,
        allowed_root,
        matches: &mut matches,
        visited: &mut visited,
        max_results,
        max_read_bytes,
        bytes_scanned: &mut bytes_scanned,
        truncated: &mut truncated,
    };
    grep_recursive(&mut ctx, root, 0).await;
    (matches, bytes_scanned, truncated)
}

struct GrepWalkContext<'a> {
    root: &'a Path,
    pattern: &'a Regex,
    allowed_root: &'a Path,
    matches: &'a mut Vec<GrepMatch>,
    visited: &'a mut HashSet<PathBuf>,
    max_results: usize,
    max_read_bytes: usize,
    bytes_scanned: &'a mut usize,
    truncated: &'a mut bool,
}

async fn grep_recursive(ctx: &mut GrepWalkContext<'_>, path: &Path, depth: u32) {
    if depth > GREP_MAX_DEPTH || ctx.matches.len() >= ctx.max_results {
        if ctx.matches.len() >= ctx.max_results {
            *ctx.truncated = true;
        }
        return;
    }

    let resolved = match tokio::fs::canonicalize(path).await {
        Ok(p) => p,
        Err(_) => return,
    };
    if !ctx.visited.insert(resolved.clone()) {
        return;
    }
    if resolved != ctx.allowed_root && !resolved.starts_with(ctx.allowed_root) {
        return;
    }

    let meta = match tokio::fs::metadata(&resolved).await {
        Ok(m) => m,
        Err(_) => return,
    };

    if meta.is_file() {
        if meta.len() as usize > ctx.max_read_bytes {
            return;
        }
        if let Ok(content) = tokio::fs::read_to_string(&resolved).await {
            if content.contains('\0') {
                return;
            }
            *ctx.bytes_scanned += content.len();
            let rel = path_relative(&resolved, ctx.root);
            for (line_num, line) in content.lines().enumerate() {
                if ctx.pattern.is_match(line) {
                    if ctx.matches.len() >= ctx.max_results {
                        *ctx.truncated = true;
                        return;
                    }
                    ctx.matches.push(GrepMatch {
                        path: rel.clone(),
                        line: (line_num + 1) as u64,
                        text: line.to_string(),
                    });
                }
            }
        }
    } else if meta.is_dir() {
        let children = collect_sorted_children(&resolved).await;
        for child in children {
            if ctx.matches.len() >= ctx.max_results {
                *ctx.truncated = true;
                return;
            }
            Box::pin(grep_recursive(ctx, &child, depth + 1)).await;
        }
    }
}

async fn find_files_at(
    root: &Path,
    allowed_root: &Path,
    pattern: &str,
    max_results: usize,
) -> (Vec<String>, usize, bool) {
    let mut matches = Vec::new();
    let mut visited = HashSet::new();
    let mut paths_visited = 0usize;
    let mut truncated = false;
    let mut ctx = FindWalkContext {
        root,
        allowed_root,
        matches: &mut matches,
        pattern,
        visited: &mut visited,
        max_results,
        paths_visited: &mut paths_visited,
        truncated: &mut truncated,
    };
    find_files_recursive(&mut ctx, root, 0).await;
    (matches, paths_visited, truncated)
}

struct FindWalkContext<'a> {
    root: &'a Path,
    allowed_root: &'a Path,
    matches: &'a mut Vec<String>,
    pattern: &'a str,
    visited: &'a mut HashSet<PathBuf>,
    max_results: usize,
    paths_visited: &'a mut usize,
    truncated: &'a mut bool,
}

async fn find_files_recursive(ctx: &mut FindWalkContext<'_>, path: &Path, depth: u32) {
    if depth > GREP_MAX_DEPTH || ctx.matches.len() >= ctx.max_results {
        if ctx.matches.len() >= ctx.max_results {
            *ctx.truncated = true;
        }
        return;
    }

    let resolved = match tokio::fs::canonicalize(path).await {
        Ok(p) => p,
        Err(_) => return,
    };
    if !ctx.visited.insert(resolved.clone()) {
        return;
    }
    *ctx.paths_visited += 1;
    if resolved != ctx.allowed_root && !resolved.starts_with(ctx.allowed_root) {
        return;
    }

    if resolved != ctx.root {
        let relative = resolved
            .strip_prefix(ctx.root)
            .unwrap_or(&resolved)
            .to_string_lossy()
            .replace('\\', "/");
        if glob_like_matches(ctx.pattern, &relative) {
            ctx.matches.push(relative);
            if ctx.matches.len() >= ctx.max_results {
                *ctx.truncated = true;
                return;
            }
        }
    }

    let meta = match tokio::fs::metadata(&resolved).await {
        Ok(m) => m,
        Err(_) => return,
    };
    if !meta.is_dir() {
        return;
    }

    for child in collect_sorted_children(&resolved).await {
        if ctx.matches.len() >= ctx.max_results {
            *ctx.truncated = true;
            return;
        }
        Box::pin(find_files_recursive(ctx, &child, depth + 1)).await;
    }
}

async fn collect_sorted_children(dir: &Path) -> Vec<PathBuf> {
    let mut children = Vec::new();
    let Ok(mut entries) = tokio::fs::read_dir(dir).await else {
        return children;
    };
    while let Ok(Some(entry)) = entries.next_entry().await {
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if matches!(name_str.as_ref(), ".git" | "target" | "node_modules") {
            continue;
        }
        if name_str.starts_with('.') && name_str != "." && name_str != ".." {
            continue;
        }
        children.push(entry.path());
    }
    children.sort();
    children
}

fn path_relative(resolved: &Path, project_root: &Path) -> String {
    resolved
        .strip_prefix(project_root)
        .unwrap_or(resolved)
        .to_string_lossy()
        .replace('\\', "/")
}

fn glob_like_matches(pattern: &str, candidate: &str) -> bool {
    if pattern.is_empty() {
        return false;
    }
    if wildcard_match(pattern, candidate) {
        return true;
    }
    let name = candidate.rsplit('/').next().unwrap_or(candidate);
    wildcard_match(pattern, name)
}

fn wildcard_match(pattern: &str, text: &str) -> bool {
    let pattern = pattern.as_bytes();
    let text = text.as_bytes();
    let (mut p, mut t) = (0usize, 0usize);
    let mut star = None;
    let mut star_text = 0usize;

    while t < text.len() {
        if p < pattern.len() && (pattern[p] == b'?' || pattern[p] == text[t]) {
            p += 1;
            t += 1;
        } else if p < pattern.len() && pattern[p] == b'*' {
            star = Some(p);
            p += 1;
            star_text = t;
        } else if let Some(star_pos) = star {
            p = star_pos + 1;
            star_text += 1;
            t = star_text;
        } else {
            return false;
        }
    }

    while p < pattern.len() && pattern[p] == b'*' {
        p += 1;
    }

    p == pattern.len()
}

async fn emit_success_metadata(
    ctx: &HandlerContext,
    method: &str,
    target: &ResolvedObservationTarget,
    mut metadata: serde_json::Value,
) {
    if let Some(obj) = metadata.as_object_mut() {
        obj.insert("request_target".to_string(), json!(target.request_target));
        obj.insert(
            "canonical_target".to_string(),
            json!(target.canonical_target.display().to_string()),
        );
        obj.insert(
            "allowed_root".to_string(),
            json!(target.allowed_root.display().to_string()),
        );
    }
    ctx.record_durable_metadata(metadata.clone());
    ctx.events
        .emit(
            "cep.native_observation_metadata",
            json!({ "method": method, "metadata": metadata }),
        )
        .await;
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
    use lancet_authority::policy::observation::reject_parent_traversal;
    use lancet_authority::policy::CepTurnContext;
    use lancet_protocol::envelope::BaseExecutionEnvelope;
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
                lancet_protocol::envelope::Capability::ReadPath { .. } => {
                    lancet_protocol::envelope::Capability::ReadPath {
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
                invariant_index: Arc::new(lancet_invariants::InvariantCardIndex::new(vec![])),
                cep_mode: lancet_protocol::envelope::CepMode::Off,
                command_isolation:
                    crate::enforcement::isolation::CommandIsolationRuntimeConfig::default(),
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
            invariant_index: Arc::new(lancet_invariants::InvariantCardIndex::new(vec![])),
            cep_mode: lancet_protocol::envelope::CepMode::Off,
            command_isolation:
                crate::enforcement::isolation::CommandIsolationRuntimeConfig::default(),
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
            invariant_index: Arc::new(lancet_invariants::InvariantCardIndex::new(vec![])),
            cep_mode: lancet_protocol::envelope::CepMode::Off,
            command_isolation:
                crate::enforcement::isolation::CommandIsolationRuntimeConfig::default(),
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
            invariant_index: Arc::new(lancet_invariants::InvariantCardIndex::new(vec![])),
            cep_mode: lancet_protocol::envelope::CepMode::Off,
            command_isolation:
                crate::enforcement::isolation::CommandIsolationRuntimeConfig::default(),
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
}
