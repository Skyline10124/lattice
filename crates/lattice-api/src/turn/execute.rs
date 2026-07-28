// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use std::sync::Arc;

use futures::StreamExt;
use lattice_agent::{AgentTurnEvent, AgentTurnResult, AgentTurnStatus};
use lattice_session::{
    GraphMessage, TurnCommitInput, TurnLifecycleEvidence, WorkspaceDisposition,
    derive_commit_message,
};
use sha2::{Digest, Sha256};

use super::cep::{ProductionTurnDepsInput, build_production_turn_deps};
use super::collector::RunTurnCollector;
use super::llm::RealLlmClient;
use super::mapping::resolve_model_and_prompt;
use super::mapping::{map_status, resolve_project_root};
use super::types::{RunTurnRequest, RunTurnResult, TurnEvent, TurnEventStream, TurnUsageSummary};
use crate::session::{Result, SessionApi};

/// Context needed to construct a [`TurnCommitInput`] from the live path.
pub(super) struct TurnCommitContext {
    pub session_id: String,
    pub parent_ids: Vec<String>,
    pub model: String,
    pub provider: String,
    pub agent_name: String,
    pub profile_snapshot_hash: String,
    pub profile_snapshot_json: String,
}

/// Build a [`TurnCommitInput`] from the user input, commit context, and agent
/// result. Shared by the live turn path to ensure commit construction stays
/// consistent.
///
/// `disposition`, `lifecycle`, and `workspace_history_oid` are required
/// parameters (not hardcoded defaults) so a new call site cannot silently
/// produce a commit with `NotApplicable` / no lifecycle evidence / no
/// workspace-history identity. The caller owns the lifecycle classification
/// and the history read, and must pass all three explicitly.
pub(super) fn build_commit_input(
    user_input: &str,
    ctx: &TurnCommitContext,
    agent_result: &AgentTurnResult,
    disposition: WorkspaceDisposition,
    lifecycle: Option<TurnLifecycleEvidence>,
    workspace_history_oid: Option<String>,
) -> TurnCommitInput {
    let commit_status = map_status(&agent_result.status).to_commit_status();

    let mut commit_messages = Vec::new();
    commit_messages.push(GraphMessage {
        role: "user".to_string(),
        content: user_input.to_string(),
        reasoning_content: None,
        tool_calls: None,
        tool_call_id: None,
        name: None,
    });
    commit_messages.extend(agent_result.messages.iter().cloned());

    let commit_message = derive_commit_message(&commit_messages);

    TurnCommitInput {
        workspace_history_oid,
        session_id: ctx.session_id.clone(),
        parent_ids: ctx.parent_ids.clone(),
        messages: commit_messages,
        agent_name: ctx.agent_name.clone(),
        profile_snapshot_hash: ctx.profile_snapshot_hash.clone(),
        profile_snapshot_json: ctx.profile_snapshot_json.clone(),
        model: ctx.model.clone(),
        provider: ctx.provider.clone(),
        status: commit_status,
        disposition,
        lifecycle,
        commit_message,
        usage: Some(agent_result.usage.clone()),
        terminal_reason: agent_result.terminal_reason.clone(),
    }
}

#[derive(Debug, Clone)]
pub(super) struct ProfileSnapshotMetadata {
    pub agent_name: String,
    pub profile_snapshot_hash: String,
    pub profile_snapshot_json: String,
}

pub(super) fn build_profile_snapshot_metadata(
    loaded: &lattice_profile::LoadedAgentProfile,
) -> Result<ProfileSnapshotMetadata> {
    let value = serde_json::json!({
        "agent_profile": loaded.profile,
        "system_prompt": loaded.system_prompt,
    });
    let profile_snapshot_json = serde_json::to_string(&value).map_err(|err| {
        crate::session::ApiError::Internal(format!("failed to serialize profile snapshot: {err}"))
    })?;
    let mut hasher = Sha256::new();
    hasher.update(profile_snapshot_json.as_bytes());
    let profile_snapshot_hash = hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();

    Ok(ProfileSnapshotMetadata {
        agent_name: loaded.profile.agent.name.clone(),
        profile_snapshot_hash,
        profile_snapshot_json,
    })
}

/// Derive the terminal [`TurnEvent`] from the agent result status.
///
/// Returns `None` for `Completed` (no terminal event needed), `Some(TurnFailed)`
/// for `Error`, and `Some(TurnCancelled)` for `Cancelled`. Terminal status is
/// always derived from `AgentTurnResult.status`, never from the presence or
/// absence of `ModelFinished` in the event stream.
pub(super) fn derive_terminal_event(result: &AgentTurnResult) -> Option<TurnEvent> {
    match result.status {
        AgentTurnStatus::Error => Some(TurnEvent::TurnFailed {
            reason: result.terminal_reason.clone().unwrap_or_default(),
        }),
        AgentTurnStatus::Cancelled => Some(TurnEvent::TurnCancelled {
            reason: result.terminal_reason.clone(),
        }),
        AgentTurnStatus::Completed => None,
    }
}

/// Execute a turn and return its final result once it has been committed.
pub async fn run_turn(session: SessionApi, request: RunTurnRequest) -> Result<RunTurnResult> {
    let mut stream = stream_turn(session, request).await?;
    let mut collector = RunTurnCollector::new();
    while let Some(event) = stream.next().await {
        collector.observe(event)?;
    }
    collector.finish()
}

/// Map a single agent process event to its public [`TurnEvent`] equivalent.
///
/// This is a 1:1 mapping over the six process variants. Public terminal
/// events (`TurnFailed`, `TurnCancelled`) and commit events are derived
/// separately from `AgentTurnResult.status` and the commit outcome.
pub(super) fn map_agent_event(event: AgentTurnEvent) -> TurnEvent {
    match event {
        AgentTurnEvent::ModelStarted { provider, model } => {
            TurnEvent::ModelStarted { provider, model }
        }
        AgentTurnEvent::LlmRequestError {
            provider,
            model,
            attempt,
            max_attempts,
            error_kind,
            retryable,
            will_retry,
            delay_ms,
            status_code,
            message,
        } => TurnEvent::LlmRequestError {
            provider,
            model,
            attempt,
            max_attempts,
            error_kind,
            retryable,
            will_retry,
            delay_ms,
            status_code,
            message,
        },
        AgentTurnEvent::ReasoningDelta { content } => TurnEvent::ReasoningDelta { content },
        AgentTurnEvent::TokenDelta { content } => TurnEvent::TokenDelta { content },
        AgentTurnEvent::ToolCallStarted { id, name } => TurnEvent::ToolCallStarted { id, name },
        AgentTurnEvent::ToolCallFinished {
            id,
            name,
            ok,
            output_len,
            error_summary,
        } => TurnEvent::ToolCallFinished {
            id,
            name,
            ok,
            output_len,
            error_summary,
        },
        AgentTurnEvent::ModelFinished {
            finish_reason,
            usage,
        } => TurnEvent::ModelFinished {
            finish_reason,
            usage: usage.map(TurnUsageSummary::from),
        },
        AgentTurnEvent::CepCallStarted {
            call_id,
            method,
            impact_kind,
            audit_id,
            semantic_stage,
            guard_stage,
        } => TurnEvent::CepCallStarted {
            call_id,
            method,
            impact_kind,
            audit_id,
            semantic_stage,
            guard_stage,
        },
        AgentTurnEvent::CepCallFinished {
            call_id,
            method,
            impact_kind,
            audit_id,
            ok,
            semantic_stage,
            guard_stage,
            error_code,
        } => TurnEvent::CepCallFinished {
            call_id,
            method,
            impact_kind,
            audit_id,
            ok,
            semantic_stage,
            guard_stage,
            error_code,
        },
    }
}

fn enabled_tools(request: &RunTurnRequest) -> Vec<String> {
    request
        .loaded_profile
        .as_ref()
        .map(|lp| lp.profile.tools.enabled.clone())
        .unwrap_or_default()
}

fn production_llm(request: &RunTurnRequest) -> Arc<dyn lattice_agent::LlmClient> {
    if let Some(llm) = request.test_llm.clone() {
        return llm;
    }
    Arc::new(RealLlmClient)
}

/// Execute a turn and return its events as a live stream.
pub async fn stream_turn(session: SessionApi, request: RunTurnRequest) -> Result<TurnEventStream> {
    if request.loaded_profile.is_none() {
        return Err(crate::session::ApiError::BadRequest(
            "loaded active agent profile is required".to_string(),
        ));
    }
    let prepared = super::live::prepare_live_turn(&session, &request)?;
    let project_root = resolve_project_root(&request);
    let cep = request.loaded_profile.as_ref().map(|lp| &lp.profile.lancet);
    let (model_id, _, _) = resolve_model_and_prompt(&request);
    let runtime_version = env!("CARGO_PKG_VERSION");
    let enabled_tools = enabled_tools(&request);
    let deps = build_production_turn_deps(ProductionTurnDepsInput {
        request: &request,
        project_root: &project_root,
        enabled_tools: &enabled_tools,
        llm: production_llm(&request),
        lancet: cep,
        model_id: &model_id,
        runtime_version,
    })
    .await?;
    super::live::stream_turn_live_prepared(session, prepared, request, deps).await
}
