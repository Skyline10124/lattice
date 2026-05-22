//! Live turn streaming path (Phase 16).
//!
//! This module implements the live `stream_turn` path: startup validation via
//! [`prepare_live_turn`], a spawned background task ([`run_live_turn_task`])
//! that drains agent events concurrently via `tokio::select!`, and emits public
//! [`TurnEvent`] values through a bounded channel before committing.

use std::sync::Arc;

use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

use lattice_agent::{AgentLoop, AgentTurnEvent, AgentTurnResult};
use lattice_session::GraphHead;

use super::execute::{
    build_commit_input, derive_terminal_event, map_agent_event, TurnCommitContext,
};
use super::mapping::{build_agent_request, map_status, resolve_model_and_prompt};
use super::types::{RunTurnRequest, TurnEvent, TurnEventStream};
use crate::session::{CreateSessionRequest, OpenSessionRequest, Result, SessionApi};

/// Bounded channel capacity for the turn event stream (D-17).
pub(super) const TURN_EVENT_CHANNEL_CAPACITY: usize = 256;

/// All state gathered during startup validation, before the live task spawns.
///
/// Constructed by [`prepare_live_turn`]; consumed by [`run_live_turn_task`].
pub(crate) struct PreparedTurn {
    pub session_id: String,
    pub parent_commit_id: Option<String>,
    pub parent_ids: Vec<String>,
    pub branch: Option<String>,
    pub model: String,
    pub provider: String,
    pub system_prompt: Option<String>,
    pub transcript: lattice_session::HeadTranscript,
    pub profile_snapshot_id: Option<String>,
}

/// Perform all startup validation for a live turn.
///
/// Resolves model/provider, creates or opens the session, reads the HEAD
/// transcript, and extracts branch information. Any failure here is returned
/// as `Result::Err` before the stream is created.
fn prepare_live_turn(session: &SessionApi, request: &RunTurnRequest) -> Result<PreparedTurn> {
    let (model, system_prompt, provider) = resolve_model_and_prompt(request);

    let session_id = match &request.session_id {
        Some(id) => {
            let s = session.open_session(OpenSessionRequest { id: id.clone() })?;
            s.session_id
        }
        None => {
            let s = session.create_session(CreateSessionRequest {
                label: None,
                profile: request.profile.clone(),
                model: request.model.clone(),
            })?;
            s.session_id
        }
    };

    let transcript = session.head_transcript(&session_id)?;
    let parent_commit_id = transcript.parent_commit_id.clone();
    let parent_ids = parent_commit_id.clone().into_iter().collect::<Vec<_>>();

    let branch = match session.status(&session_id)?.head {
        GraphHead::Branch { name } => Some(name),
        GraphHead::Detached { .. } => None,
    };

    let profile_snapshot_id = request
        .loaded_profile
        .as_ref()
        .map(|lp| lp.profile.agent.name.clone());

    Ok(PreparedTurn {
        session_id,
        parent_commit_id,
        parent_ids,
        branch,
        model,
        provider,
        system_prompt,
        transcript,
        profile_snapshot_id,
    })
}

/// Fire-and-forget event emission (D-03).
///
/// Send errors are silently ignored — receiver drop means the caller stopped
/// observing, but the turn continues to completion.
async fn emit_turn_event(sender: &mpsc::Sender<TurnEvent>, event: TurnEvent) {
    let _ = sender.send(event).await;
}

/// Background task that runs the agent loop, maps events live, and commits.
///
/// Event emission order: TurnStarted -> [agent events] -> [terminal] ->
/// TurnCommitting -> TurnCommitted/CommitFailed. The sender is dropped after
/// the final event, closing the stream.
async fn run_live_turn_task(
    session: SessionApi,
    agent_loop: Arc<AgentLoop>,
    prepared: PreparedTurn,
    request: RunTurnRequest,
    turn_tx: mpsc::Sender<TurnEvent>,
) {
    // 1. Emit TurnStarted
    emit_turn_event(
        &turn_tx,
        TurnEvent::TurnStarted {
            session_id: prepared.session_id.clone(),
            parent_commit_id: prepared.parent_commit_id.clone(),
            branch: prepared.branch.clone(),
        },
    )
    .await;

    // 2. Build agent request
    let agent_request = build_agent_request(
        &request,
        &prepared.session_id,
        prepared.transcript,
        prepared.model.clone(),
        prepared.system_prompt.clone(),
        Some(prepared.provider.clone()).filter(|p| p != "unknown"),
    );

    // 3. Create agent event channel
    let (agent_tx, mut agent_rx) = mpsc::channel::<AgentTurnEvent>(TURN_EVENT_CHANNEL_CAPACITY);

    // 4. Pin agent future
    let mut agent_fut = Box::pin(agent_loop.run_turn_live(agent_request, agent_tx));

    // 5. MANDATORY select! concurrent drain loop (D-20)
    let agent_result: AgentTurnResult = loop {
        tokio::select! {
            maybe_event = agent_rx.recv() => {
                if let Some(event) = maybe_event {
                    emit_turn_event(&turn_tx, map_agent_event(event)).await;
                }
            }
            result = &mut agent_fut => {
                break result;
            }
        }
    };

    // 6. Post-loop drain: flush any remaining buffered events
    while let Some(event) = agent_rx.recv().await {
        emit_turn_event(&turn_tx, map_agent_event(event)).await;
    }

    // 7. Terminal event derived from AgentTurnResult.status (D-10, WR-01)
    if let Some(terminal) = derive_terminal_event(&agent_result) {
        emit_turn_event(&turn_tx, terminal).await;
    }

    // 8. Emit TurnCommitting
    emit_turn_event(&turn_tx, TurnEvent::TurnCommitting).await;

    // 9. Build commit context and input
    let ctx = TurnCommitContext {
        session_id: prepared.session_id.clone(),
        parent_ids: prepared.parent_ids,
        model: prepared.model,
        provider: prepared.provider,
        profile_snapshot_id: prepared.profile_snapshot_id,
    };

    let commit_input = build_commit_input(&request.input, &ctx, &agent_result);

    // 10. Commit and emit result (D-13)
    match session.commit_turn(commit_input) {
        Ok(commit) => {
            let status = map_status(&agent_result.status);
            emit_turn_event(
                &turn_tx,
                TurnEvent::TurnCommitted {
                    session_id: prepared.session_id,
                    commit_id: commit.commit_id,
                    status,
                },
            )
            .await;
        }
        Err(e) => {
            emit_turn_event(
                &turn_tx,
                TurnEvent::CommitFailed {
                    reason: e.to_string(),
                },
            )
            .await;
        }
    }

    // Sender drops here, closing the stream.
}

/// Execute a turn and return its events as a live stream.
///
/// Startup failures (model resolution, session open/create, transcript read,
/// status check) are returned as `Result::Err`. Runtime outcomes — including
/// commit success or failure — are emitted as [`TurnEvent`] values within the
/// stream.
pub(super) async fn stream_turn_live(
    session: SessionApi,
    agent_loop: Arc<AgentLoop>,
    request: RunTurnRequest,
) -> Result<TurnEventStream> {
    let prepared = prepare_live_turn(&session, &request)?;

    let (tx, rx) = mpsc::channel::<TurnEvent>(TURN_EVENT_CHANNEL_CAPACITY);

    tokio::spawn(async move {
        run_live_turn_task(session, agent_loop, prepared, request, tx).await;
    });

    Ok(Box::pin(ReceiverStream::new(rx)))
}
