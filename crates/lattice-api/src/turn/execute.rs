// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use std::sync::Arc;

use futures::StreamExt;
use lancet_protocol::InvocationId;
use sha2::{Digest, Sha256};

use super::cep::{ProductionTurnDepsInput, build_production_turn_deps};
use super::collector::RunTurnCollector;
use super::coordinator::{CoordinatorStoreIo, RandomIdSource, TurnCoordinator};
use super::llm::RealLlmClient;
use super::mapping::resolve_project_root;
use super::types::{RunTurnRequest, RunTurnResult, TurnEvent, TurnEventStream};
use crate::session::{Result, SessionApi};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum AgentTurnStatus {
    Completed,
    Error,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum AgentTurnEvent {
    ReasoningDelta {
        content: String,
    },
    TokenDelta {
        content: String,
    },
    ToolCallStarted {
        id: String,
        name: String,
    },
    CepCallStarted {
        invocation_id: InvocationId,
        method: String,
        semantic_stage: String,
        guard_stage: String,
    },
    CepCallFinished {
        invocation_id: InvocationId,
        method: String,
        ok: bool,
        semantic_stage: String,
        guard_stage: String,
        error_code: Option<String>,
    },
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
/// Returns `None` for `Completed`, `Some(TurnFailed)` for `Error`, and
/// `Some(TurnCancelled)` for `Cancelled`.
pub(super) fn derive_terminal_event(
    status: &AgentTurnStatus,
    terminal_reason: Option<&str>,
) -> Option<TurnEvent> {
    match status {
        AgentTurnStatus::Error => Some(TurnEvent::TurnFailed {
            reason: terminal_reason.unwrap_or_default().to_string(),
        }),
        AgentTurnStatus::Cancelled => Some(TurnEvent::TurnCancelled {
            reason: terminal_reason.map(str::to_string),
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
/// Maps the model/CEP events forwarded by the coordinator to the public stream.
pub(super) fn map_agent_event(event: AgentTurnEvent) -> TurnEvent {
    match event {
        AgentTurnEvent::ReasoningDelta { content } => TurnEvent::ReasoningDelta { content },
        AgentTurnEvent::TokenDelta { content } => TurnEvent::TokenDelta { content },
        AgentTurnEvent::ToolCallStarted { id, name } => TurnEvent::ToolCallStarted { id, name },
        AgentTurnEvent::CepCallStarted {
            invocation_id,
            method,
            semantic_stage,
            guard_stage,
        } => TurnEvent::CepCallStarted {
            invocation_id,
            method,
            semantic_stage,
            guard_stage,
        },
        AgentTurnEvent::CepCallFinished {
            invocation_id,
            method,
            ok,
            semantic_stage,
            guard_stage,
            error_code,
        } => TurnEvent::CepCallFinished {
            invocation_id,
            method,
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

/// Idle timeout shared by every `stream_turn` consumer (CLI `run` and the TUI).
pub const STREAM_IDLE_TIMEOUT_SECS: u64 = 300;

/// Execute a turn and return its events as a live stream.
pub async fn stream_turn(session: SessionApi, request: RunTurnRequest) -> Result<TurnEventStream> {
    if request.loaded_profile.is_none() {
        return Err(crate::session::ApiError::BadRequest(
            "loaded active agent profile is required".to_string(),
        ));
    }
    let requested_root = resolve_project_root(&request)?;
    let workspace_root = crate::root::LocalWorkspaceRoot::from_path(&requested_root)?;
    let project_root = workspace_root
        .canonical_path()
        .to_str()
        .ok_or_else(|| {
            crate::session::ApiError::BadRequest(
                "Local Workspace root must be valid UTF-8 for the runtime policy path".into(),
            )
        })?
        .to_owned();
    // Acquire the full SessionExecutionLock before taking the live snapshot;
    // otherwise a concurrent Turn could move HEAD between preparation and
    // dispatch. Blocking lock acquisition runs on Tokio's blocking pool.
    let session_for_lock = session.clone();
    let requested_session_id = request.session_id.clone();
    let (_, execution_lock) =
        tokio::task::spawn_blocking(move || session_for_lock.lock_execution(&requested_session_id))
            .await
            .map_err(|err| {
                crate::session::ApiError::Internal(format!("acquire session lock task: {err}"))
            })??;
    let execution_lock = Arc::new(execution_lock);
    let session_store = session.git_store_for_coordinator()?;
    let layout = session.state_layout().ok_or_else(|| {
        crate::session::ApiError::Internal(
            "TurnCoordinator requires a StateLayout-backed SessionApi".to_string(),
        )
    })?;
    let invocation_store = Arc::new(
        lancet_audit::invocation_store::InvocationStore::open(&layout, &workspace_root.store_key())
            .map_err(|err| {
                crate::session::ApiError::Internal(format!("open invocation store: {err}"))
            })?,
    );
    let coordinator = TurnCoordinator::new(
        CoordinatorStoreIo::new(session_store, invocation_store),
        Arc::new(RandomIdSource),
    );
    coordinator
        .reconcile_pending(request.session_id.clone(), Arc::clone(&execution_lock))
        .await
        .map_err(|err| match err {
            super::coordinator::CoordinatorIoError::EvidenceDurabilityFailed { attempts } => {
                crate::session::ApiError::RecoveryRequired(
                    crate::session::RecoveryRequired::EvidenceDurabilityFailed(
                        crate::session::EvidenceDurabilityFailed { attempts },
                    ),
                )
            }
            other => crate::session::ApiError::Internal(format!("reconcile pending Turn: {other}")),
        })?;
    let prepared = super::live::prepare_live_turn_locked(
        &session,
        &request,
        workspace_root.canonical_path(),
        execution_lock.as_ref(),
    )?;
    let cep = request.loaded_profile.as_ref().map(|lp| &lp.profile.lancet);
    let enabled_tools = enabled_tools(&request);
    let deps = build_production_turn_deps(ProductionTurnDepsInput {
        request: &request,
        project_root: &project_root,
        project_store_key: Some(prepared.project_store_key),
        state_layout: session.state_layout(),
        enabled_tools: &enabled_tools,
        llm: production_llm(&request),
        lancet: cep,
    })
    .await?;
    super::live::stream_turn_live_prepared(
        session,
        prepared,
        request,
        deps,
        Arc::new(coordinator),
        execution_lock,
    )
    .await
}
