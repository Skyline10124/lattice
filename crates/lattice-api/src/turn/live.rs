// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Live turn streaming path (live turn).
//!
//! Startup validation via [`prepare_live_turn_locked`], then a spawned task
//! that publishes durable `TurnStarted` and hands the Turn to the #139-B
//! [`super::saga`] coordinator loop.

use std::sync::Arc;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

use lattice_session::{FlockGuard, GraphHead, ProjectStoreKey};

use super::cep::ProductionTurnDeps;
use super::mapping::resolve_model_and_prompt;
#[cfg(test)]
use super::observer::CepOrderedEmitter;
use super::observer::ObserverSink;
use super::types::{RunTurnRequest, TurnEvent, TurnEventStream};
use crate::session::{OpenSessionRequest, Result, SessionApi};

/// Bounded channel capacity for the turn event stream.
pub(super) const TURN_EVENT_CHANNEL_CAPACITY: usize = 256;

/// All state gathered during startup validation, before the live task spawns.
pub(crate) struct PreparedTurn {
    pub session_id: String,
    pub parent_commit_id: Option<String>,
    pub branch: Option<String>,
    pub model: String,
    pub provider: String,
    pub system_prompt: Option<String>,
    pub transcript: lattice_session::HeadTranscript,
    pub agent_name: String,
    pub profile_snapshot_hash: String,
    pub profile_snapshot_json: String,
    pub project_store_key: ProjectStoreKey,
}

/// Prepare a live Turn while the caller already holds the full
/// `SessionExecutionLock`.
pub(crate) fn prepare_live_turn_locked(
    session: &SessionApi,
    request: &RunTurnRequest,
    project_root: &std::path::Path,
    execution_lock: &FlockGuard,
) -> Result<PreparedTurn> {
    let (model, system_prompt, provider) = resolve_model_and_prompt(request);

    let s = session.open_session(OpenSessionRequest {
        id: request.session_id.clone(),
    })?;
    let session_id = s.session_id;

    let workspace = crate::root::LocalWorkspaceRoot::from_path(project_root)?;
    let expected_key = ProjectStoreKey::parse(&s.project_store_key).map_err(|err| {
        crate::session::ApiError::Internal(format!(
            "session '{session_id}' has an invalid ProjectStoreKey binding: {err}"
        ))
    })?;
    if expected_key != workspace.store_key() {
        return Err(crate::session::ApiError::Conflict(format!(
            "session '{session_id}' is bound to project key {expected_key}, but the current Local Workspace resolves to {}",
            workspace.store_key()
        )));
    }

    let transcript = session.head_transcript(&session_id)?;
    let parent_commit_id = transcript.parent_commit_id.clone();

    let status = session.status_locked(&session_id, execution_lock)?;
    let branch = match status.head {
        GraphHead::Branch { name } => Some(name),
        GraphHead::Detached { .. } => None,
    };

    let loaded_profile = request.loaded_profile.as_ref().ok_or_else(|| {
        crate::session::ApiError::BadRequest("loaded active agent profile is required".to_string())
    })?;
    let profile_snapshot = super::execute::build_profile_snapshot_metadata(loaded_profile)?;

    Ok(PreparedTurn {
        session_id,
        parent_commit_id,
        branch,
        model,
        provider,
        system_prompt,
        transcript,
        agent_name: profile_snapshot.agent_name,
        profile_snapshot_hash: profile_snapshot.profile_snapshot_hash,
        profile_snapshot_json: profile_snapshot.profile_snapshot_json,
        project_store_key: expected_key,
    })
}

async fn run_live_turn_task(
    _session: SessionApi,
    prepared: PreparedTurn,
    request: RunTurnRequest,
    turn_tx: mpsc::Sender<TurnEvent>,
    deps: ProductionTurnDeps,
    coordinator: Arc<super::coordinator::TurnCoordinator>,
    execution_lock: Arc<FlockGuard>,
) {
    let ProductionTurnDeps {
        model_driver,
        tool_catalog,
        runtime,
        cep_client,
        workspace_history,
        ..
    } = deps;

    let turn_id = coordinator.ids.new_turn_id();
    let loaded = request.loaded_profile.as_ref().expect("validated profile");
    let catalog = lattice_session::pending::ToolCatalogSnapshot::new(
        env!("CARGO_PKG_VERSION").to_string(),
        tool_catalog
            .iter()
            .map(|tool| lattice_session::pending::ToolCatalogEntry {
                name: tool.name.clone(),
                descriptor_identity: tool.descriptor_identity.clone(),
                description: tool.description.clone(),
                input_schema: tool.parameters.clone(),
            })
            .collect(),
    );
    let accepted_at = chrono::Utc::now().to_rfc3339();
    let begin = coordinator
        .io
        .begin_turn(
            lattice_session::pending::TurnStartedFacts {
                turn_id,
                session_id: lancet_protocol::SessionId::parse(&prepared.session_id)
                    .expect("SessionApi stores canonical SessionIds"),
                parent_conversation_tip: prepared.parent_commit_id.clone(),
                user_message: lattice_conversation::NativeMessage::new(
                    lattice_conversation::MessageRole::User,
                    None,
                    vec![lattice_conversation::MessagePart::semantic_only(
                        lattice_conversation::MessageSemantic::Text {
                            text: request.input.clone(),
                        },
                    )],
                ),
                config: lattice_session::pending::FrozenTurnConfig {
                    agent_name: prepared.agent_name.clone(),
                    model: prepared.model.clone(),
                    provider: prepared.provider.clone(),
                    system_prompt: prepared.system_prompt.clone(),
                    profile_snapshot_json: prepared.profile_snapshot_json.clone(),
                    policy_snapshot_json: serde_json::to_string(&loaded.profile.lancet)
                        .unwrap_or_else(|_| "{}".to_string()),
                    catalog: match catalog {
                        Ok(catalog) => catalog,
                        Err(err) => {
                            let mut observer = ObserverSink::new(turn_tx);
                            observer
                                .emit(TurnEvent::CommitFailed {
                                    reason: err.to_string(),
                                })
                                .await;
                            return;
                        }
                    },
                    limits: lattice_session::pending::TurnExecutionLimits {
                        timeout_secs: loaded.profile.llm.timeout_secs.unwrap_or(300),
                        max_model_steps: lattice_session::pending::MAX_MODEL_STEPS,
                        max_model_attempts: loaded.profile.llm.retry.max_retries.saturating_add(1),
                    },
                },
                accepted_at,
            },
            Arc::clone(&execution_lock),
        )
        .await;
    if let Err(err) = begin {
        let mut observer = ObserverSink::new(turn_tx);
        observer
            .emit(TurnEvent::CommitFailed {
                reason: err.to_string(),
            })
            .await;
        return;
    }
    let mut observer = ObserverSink::new(turn_tx);
    observer
        .emit(TurnEvent::TurnStarted {
            session_id: prepared.session_id.clone(),
            turn_id: turn_id.to_string(),
            parent_commit_id: prepared.parent_commit_id.clone(),
            branch: prepared.branch.clone(),
        })
        .await;

    let llm_tools: Vec<lattice_llm::ToolDefinition> = tool_catalog
        .iter()
        .cloned()
        .map(|tool| lattice_llm::ToolDefinition::new(tool.name, tool.description, tool.parameters))
        .collect();

    super::saga::run_coordinator_saga(
        prepared,
        request,
        turn_id,
        model_driver,
        llm_tools,
        tool_catalog,
        runtime,
        cep_client,
        workspace_history,
        coordinator,
        execution_lock,
        observer,
    )
    .await;
}

/// Execute a turn with an already-prepared session snapshot and return a live stream.
pub(super) async fn stream_turn_live_prepared(
    session: SessionApi,
    prepared: PreparedTurn,
    request: RunTurnRequest,
    deps: ProductionTurnDeps,
    coordinator: Arc<super::coordinator::TurnCoordinator>,
    execution_lock: Arc<FlockGuard>,
) -> Result<TurnEventStream> {
    let (tx, rx) = mpsc::channel::<TurnEvent>(TURN_EVENT_CHANNEL_CAPACITY);

    tokio::spawn(async move {
        run_live_turn_task(
            session,
            prepared,
            request,
            tx,
            deps,
            coordinator,
            execution_lock,
        )
        .await;
    });

    Ok(Box::pin(ReceiverStream::new(rx)))
}

/// Execute a turn and return its events as a live stream.
#[cfg(test)]
pub(super) async fn stream_turn_live(
    session: SessionApi,
    request: RunTurnRequest,
    deps: ProductionTurnDeps,
) -> Result<TurnEventStream> {
    let requested_root = super::mapping::resolve_project_root(&request)?;
    let workspace_root = crate::root::LocalWorkspaceRoot::from_path(&requested_root)?;
    let session_for_lock = session.clone();
    let session_id = request.session_id.clone();
    let (_, execution_lock) =
        tokio::task::spawn_blocking(move || session_for_lock.lock_execution(&session_id))
            .await
            .map_err(|err| {
                crate::session::ApiError::Internal(format!("acquire session lock task: {err}"))
            })??;
    let execution_lock = Arc::new(execution_lock);
    let prepared = prepare_live_turn_locked(
        &session,
        &request,
        workspace_root.canonical_path(),
        execution_lock.as_ref(),
    )?;
    let layout = session.state_layout().ok_or_else(|| {
        crate::session::ApiError::Internal(
            "test live Turn requires a StateLayout-backed SessionApi".to_string(),
        )
    })?;
    let key = prepared.project_store_key;
    let invocations = Arc::new(
        lancet_audit::invocation_store::InvocationStore::open(&layout, &key).map_err(|err| {
            crate::session::ApiError::Internal(format!("open invocation store: {err}"))
        })?,
    );
    let coordinator = Arc::new(super::coordinator::TurnCoordinator::new(
        super::coordinator::CoordinatorStoreIo::new(
            session.git_store_for_coordinator()?,
            invocations,
        ),
        Arc::new(super::coordinator::RandomIdSource),
    ));
    stream_turn_live_prepared(
        session,
        prepared,
        request,
        deps,
        coordinator,
        execution_lock,
    )
    .await
}

#[cfg(test)]
mod tests {
    use tokio::sync::mpsc;
    use tokio::sync::mpsc::error::TryRecvError;

    use super::*;
    use crate::turn::types::TurnEvent;

    fn cep_started(ordinal: u64) -> TurnEvent {
        TurnEvent::CepCallStarted {
            invocation_id: lancet_protocol::InvocationId::parse(&format!(
                "00000000-0000-4000-8000-{ordinal:012x}"
            ))
            .unwrap(),
            method: "session.todo_write".to_string(),
            semantic_stage: "cep_native".to_string(),
            guard_stage: "cep_policy_lifecycle".to_string(),
        }
    }

    fn cep_finished(ordinal: u64) -> TurnEvent {
        TurnEvent::CepCallFinished {
            invocation_id: lancet_protocol::InvocationId::parse(&format!(
                "00000000-0000-4000-8000-{ordinal:012x}"
            ))
            .unwrap(),
            method: "session.todo_write".to_string(),
            ok: true,
            semantic_stage: "cep_native".to_string(),
            guard_stage: "cep_policy_lifecycle".to_string(),
            error_code: None,
        }
    }

    fn tool_finished() -> TurnEvent {
        TurnEvent::ToolCallFinished {
            id: "c1".to_string(),
            name: "session.todo_write".to_string(),
            ok: true,
            output_len: 2,
            error_summary: None,
        }
    }

    #[tokio::test]
    async fn cep_ordered_emitter_defers_tool_finish_until_late_cep_lifecycle() {
        let (tx, mut rx) = mpsc::channel(8);
        let mut observer = ObserverSink::new(tx);
        let mut ordered = CepOrderedEmitter::default();

        ordered.emit(&mut observer, tool_finished()).await;
        assert_eq!(rx.try_recv(), Err(TryRecvError::Empty));

        ordered.emit(&mut observer, cep_started(1)).await;
        ordered.emit(&mut observer, cep_finished(1)).await;

        assert!(matches!(
            rx.recv().await,
            Some(TurnEvent::CepCallStarted { invocation_id, .. }) if invocation_id.to_string() == "00000000-0000-4000-8000-000000000001"
        ));
        assert!(matches!(
            rx.recv().await,
            Some(TurnEvent::CepCallFinished { invocation_id, .. }) if invocation_id.to_string() == "00000000-0000-4000-8000-000000000001"
        ));
        assert!(matches!(
            rx.recv().await,
            Some(TurnEvent::ToolCallFinished { id, .. }) if id == "c1"
        ));
    }

    #[tokio::test]
    async fn cep_ordered_emitter_releases_deferred_tool_finish_before_next_plain_event() {
        let (tx, mut rx) = mpsc::channel(8);
        let mut observer = ObserverSink::new(tx);
        let mut ordered = CepOrderedEmitter::default();

        ordered.emit(&mut observer, tool_finished()).await;
        ordered
            .emit(
                &mut observer,
                TurnEvent::ModelStarted {
                    provider: Some("test".to_string()),
                    model: "model".to_string(),
                },
            )
            .await;

        assert!(matches!(
            rx.recv().await,
            Some(TurnEvent::ToolCallFinished { id, .. }) if id == "c1"
        ));
        assert!(matches!(
            rx.recv().await,
            Some(TurnEvent::ModelStarted { model, .. }) if model == "model"
        ));
    }
}
