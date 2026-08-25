// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use std::sync::Arc;

use futures::StreamExt;
use sha2::{Digest, Sha256};

use super::cep::{ProductionTurnDepsInput, build_production_turn_deps};
use super::collector::RunTurnCollector;
use super::coordinator::{CoordinatorStoreIo, RandomIdSource, TurnCoordinator};
use super::llm::RealLlmClient;
use super::mapping::resolve_project_root;
use super::types::{RunTurnRequest, RunTurnResult, TurnEventStream};
use crate::session::{Result, SessionApi};

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

/// Execute a turn and return its final result once it has been committed.
pub async fn run_turn(session: SessionApi, request: RunTurnRequest) -> Result<RunTurnResult> {
    let mut stream = stream_turn(session, request).await?;
    let mut collector = RunTurnCollector::new();
    while let Some(event) = stream.next().await {
        collector.observe(event)?;
    }
    collector.finish()
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
    let (locked_session_id, execution_lock) =
        tokio::task::spawn_blocking(move || session_for_lock.lock_execution(&requested_session_id))
            .await
            .map_err(|err| {
                crate::session::ApiError::Internal(format!("acquire session lock task: {err}"))
            })??;
    let execution_lock = Arc::new(execution_lock);

    // SessionState and the PendingTurn root are the project authority. Validate
    // both against the canonical request hint before any project-keyed store is
    // opened or queried.
    let (session_id, project_store_key) =
        session.turn_project_binding_locked(&locked_session_id, execution_lock.as_ref())?;
    if project_store_key != workspace_root.store_key() {
        return Err(crate::session::ApiError::Conflict(format!(
            "session '{session_id}' is bound to project key {project_store_key}, but the requested Local Workspace resolves to {}",
            workspace_root.store_key()
        )));
    }

    let session_store = session.git_store_for_coordinator()?;
    let layout = session.state_layout().ok_or_else(|| {
        crate::session::ApiError::Internal(
            "TurnCoordinator requires a StateLayout-backed SessionApi".to_string(),
        )
    })?;
    let invocation_store = Arc::new(
        lancet_audit::invocation_store::InvocationStore::open(&layout, &project_store_key)
            .map_err(|err| {
                crate::session::ApiError::Internal(format!("open invocation store: {err}"))
            })?,
    );
    let coordinator = TurnCoordinator::new(
        CoordinatorStoreIo::new(session_store, invocation_store, project_store_key).map_err(
            |err| crate::session::ApiError::Internal(format!("compose TurnCoordinator: {err}")),
        )?,
        Arc::new(RandomIdSource),
    );

    // Compose and strictly open both project stores before reconciliation can
    // advance the PendingTurn or append ledger facts. Build performs the final
    // canonical-root/key/layout consistency proof; it starts no provider or
    // Tool work.
    let cep = request.loaded_profile.as_ref().map(|lp| &lp.profile.lancet);
    let enabled_tools = enabled_tools(&request);
    let deps = build_production_turn_deps(ProductionTurnDepsInput {
        request: &request,
        project_root: &project_root,
        project_store_key: Some(project_store_key),
        state_layout: Some(layout),
        enabled_tools: &enabled_tools,
        llm: production_llm(&request),
        lancet: cep,
    })
    .await?;

    coordinator
        .reconcile_pending(session_id, Arc::clone(&execution_lock))
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
