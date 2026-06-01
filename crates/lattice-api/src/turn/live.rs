//! Live turn streaming path (live turn).
//!
//! This module implements the live `stream_turn` path: startup validation via
//! [`prepare_live_turn`], a spawned background task ([`run_live_turn_task`])
//! that drains agent events concurrently via `tokio::select!`, and emits public
//! [`TurnEvent`] values through a bounded channel before committing.

#[cfg(test)]
use std::sync::Arc;

use std::time::Duration;

use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

#[cfg(test)]
use lattice_agent::AgentLoop;

use lattice_agent::{AgentTurnEvent, AgentTurnResult};
use lattice_session::GraphHead;

use super::cep::{generate_and_store_attestation, map_cep_tool_event, ProductionTurnDeps};
use super::execute::{
    build_commit_input, build_profile_snapshot_metadata, derive_terminal_event, map_agent_event,
    TurnCommitContext,
};
use super::mapping::{build_agent_request, map_status, resolve_model_and_prompt};
use super::types::{RunTurnRequest, TurnEvent, TurnEventStream};
use crate::session::{OpenSessionRequest, Result, SessionApi};

/// Bounded channel capacity for the turn event stream.
pub(super) const TURN_EVENT_CHANNEL_CAPACITY: usize = 256;

/// Max wait when forwarding a non-delta [`TurnEvent`] to a slow observer.
/// After timeout, further public events are skipped but the turn still commits.
const OBSERVER_SEND_TIMEOUT: Duration = Duration::from_millis(500);

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
    pub agent_name: String,
    pub profile_snapshot_hash: String,
    pub profile_snapshot_json: String,
}

/// Perform all startup validation for a live turn.
///
/// Resolves model/provider, opens the required session, reads the HEAD
/// transcript, snapshots the active profile, and extracts branch information.
/// Any failure here is returned as `Result::Err` before the stream is created.
fn prepare_live_turn(session: &SessionApi, request: &RunTurnRequest) -> Result<PreparedTurn> {
    let (model, system_prompt, provider) = resolve_model_and_prompt(request);

    let s = session.open_session(OpenSessionRequest {
        id: request.session_id.clone(),
    })?;
    let session_id = s.session_id;

    let transcript = session.head_transcript(&session_id)?;
    let parent_commit_id = transcript.parent_commit_id.clone();
    let parent_ids = parent_commit_id.clone().into_iter().collect::<Vec<_>>();

    let branch = match session.status(&session_id)?.head {
        GraphHead::Branch { name } => Some(name),
        GraphHead::Detached { .. } => None,
    };

    let loaded_profile = request.loaded_profile.as_ref().ok_or_else(|| {
        crate::session::ApiError::BadRequest("loaded active agent profile is required".to_string())
    })?;
    let profile_snapshot = build_profile_snapshot_metadata(loaded_profile)?;

    Ok(PreparedTurn {
        session_id,
        parent_commit_id,
        parent_ids,
        branch,
        model,
        provider,
        system_prompt,
        transcript,
        agent_name: profile_snapshot.agent_name,
        profile_snapshot_hash: profile_snapshot.profile_snapshot_hash,
        profile_snapshot_json: profile_snapshot.profile_snapshot_json,
    })
}

fn is_streaming_delta(event: &TurnEvent) -> bool {
    matches!(
        event,
        TurnEvent::TokenDelta { .. } | TurnEvent::ReasoningDelta { .. }
    )
}

/// Best-effort public observer sink (M-19b).
///
/// Streaming deltas use non-blocking `try_send` and may be dropped when the
/// channel is full. Lifecycle, terminal, and commit events use a bounded wait;
/// on timeout or failure the sink stops forwarding but the background task
/// keeps draining the agent channel and completes the durable commit.
struct ObserverSink {
    sender: mpsc::Sender<TurnEvent>,
    gave_up: bool,
}

impl ObserverSink {
    fn new(sender: mpsc::Sender<TurnEvent>) -> Self {
        Self {
            sender,
            gave_up: false,
        }
    }

    async fn emit(&mut self, event: TurnEvent) {
        if self.gave_up {
            return;
        }
        if is_streaming_delta(&event) {
            let _ = self.sender.try_send(event);
            return;
        }
        let send = self.sender.send(event);
        match tokio::time::timeout(OBSERVER_SEND_TIMEOUT, send).await {
            Ok(Ok(())) => {}
            _ => self.gave_up = true,
        }
    }
}

/// Background task that runs the agent loop, maps events live, and commits.
///
/// Event emission order: TurnStarted -> [agent events] -> [terminal] ->
/// TurnCommitting -> TurnCommitted/CommitFailed. The sender is dropped after
/// the final event, closing the stream.
async fn run_live_turn_task(
    session: SessionApi,
    prepared: PreparedTurn,
    request: RunTurnRequest,
    turn_tx: mpsc::Sender<TurnEvent>,
    mut deps: ProductionTurnDeps,
) {
    let ProductionTurnDeps {
        agent_loop,
        tool_definitions,
        runtime,
        cep_client,
        audit,
        audit_context,
        #[cfg(test)]
            build_metrics: _build_metrics,
        ..
    } = &mut deps;

    let turn_id = format!(
        "{}:{}",
        prepared.session_id,
        prepared.parent_commit_id.as_deref().unwrap_or("root")
    );
    {
        let mut ctx = audit_context.write().await;
        ctx.turn_id = turn_id.clone();
        ctx.session_id = prepared.session_id.clone();
        ctx.model_id = prepared.model.clone();
        ctx.runtime_version = env!("CARGO_PKG_VERSION").to_string();
    }

    let mut observer = ObserverSink::new(turn_tx);

    // 1. Emit TurnStarted
    observer
        .emit(TurnEvent::TurnStarted {
            session_id: prepared.session_id.clone(),
            parent_commit_id: prepared.parent_commit_id.clone(),
            branch: prepared.branch.clone(),
        })
        .await;

    // 2. Build agent request
    let agent_request = build_agent_request(
        &request,
        &prepared.session_id,
        prepared.transcript,
        prepared.model.clone(),
        prepared.system_prompt.clone(),
        Some(prepared.provider.clone()).filter(|p| p != "unknown"),
        tool_definitions.clone(),
    );

    // 3. Create agent event channel
    let (agent_tx, mut agent_rx) = mpsc::channel::<AgentTurnEvent>(TURN_EVENT_CHANNEL_CAPACITY);

    let cep_client = cep_client.clone();
    let cep_agent_tx = agent_tx.clone();
    let cep_forwarder = tokio::spawn(async move {
        while let Some(event) = cep_client.next_event().await {
            if let Some(agent_event) = map_cep_tool_event(event) {
                let _ = cep_agent_tx.try_send(agent_event);
            }
        }
    });

    // 4. Pin agent future
    let mut agent_fut = Box::pin(agent_loop.run_turn_live(agent_request, agent_tx));

    // 5. Concurrent drain loop. Forwarding uses non-blocking / bounded-wait
    // observer emission so a slow consumer cannot stall agent_rx drain.
    let agent_result: AgentTurnResult = loop {
        tokio::select! {
            maybe_event = agent_rx.recv() => {
                if let Some(event) = maybe_event {
                    observer.emit(map_agent_event(event)).await;
                }
            }
            result = &mut agent_fut => {
                cep_forwarder.abort();
                break result;
            }
        }
    };

    // 6. Post-loop drain: flush agent channel without blocking on observer.
    while let Some(event) = agent_rx.recv().await {
        observer.emit(map_agent_event(event)).await;
    }

    cep_forwarder.abort();

    runtime.shutdown().await;
    audit.await_pending().await;

    {
        let ctx = audit_context.read().await;
        if let Err(err) = generate_and_store_attestation(audit, &ctx, &turn_id) {
            eprintln!("failed to store turn attestation for {turn_id}: {err}");
        }
    }

    // 7. Terminal event derived from AgentTurnResult.status.
    if let Some(terminal) = derive_terminal_event(&agent_result) {
        observer.emit(terminal).await;
    }

    // 8. Emit TurnCommitting
    observer.emit(TurnEvent::TurnCommitting).await;

    // 9. Build commit context and input
    let ctx = TurnCommitContext {
        session_id: prepared.session_id.clone(),
        parent_ids: prepared.parent_ids,
        model: prepared.model,
        provider: prepared.provider,
        agent_name: prepared.agent_name,
        profile_snapshot_hash: prepared.profile_snapshot_hash,
        profile_snapshot_json: prepared.profile_snapshot_json,
    };

    let commit_input = build_commit_input(&request.input, &ctx, &agent_result);

    // 10. Commit and emit result. Commit runs even when the observer stopped
    // accepting events.
    match session.commit_turn(commit_input) {
        Ok(commit) => {
            let status = map_status(&agent_result.status);
            observer
                .emit(TurnEvent::TurnCommitted {
                    session_id: prepared.session_id,
                    commit_id: commit.commit_id,
                    status,
                })
                .await;
        }
        Err(e) => {
            observer
                .emit(TurnEvent::CommitFailed {
                    reason: e.to_string(),
                })
                .await;
        }
    }

    // Sender drops here, closing the stream.
}

/// Execute a turn and return its events as a live stream.
pub(super) async fn stream_turn_live(
    session: SessionApi,
    request: RunTurnRequest,
    deps: ProductionTurnDeps,
) -> Result<TurnEventStream> {
    let prepared = prepare_live_turn(&session, &request)?;

    let (tx, rx) = mpsc::channel::<TurnEvent>(TURN_EVENT_CHANNEL_CAPACITY);

    tokio::spawn(async move {
        run_live_turn_task(session, prepared, request, tx, deps).await;
    });

    Ok(Box::pin(ReceiverStream::new(rx)))
}

/// Test-only live path that injects a prebuilt AgentLoop without Cep runtime construction.
#[cfg(test)]
pub(super) async fn stream_turn_live_with_agent(
    session: SessionApi,
    agent_loop: Arc<AgentLoop>,
    request: RunTurnRequest,
    tool_definitions: Vec<lattice_llm::ToolDefinition>,
) -> Result<TurnEventStream> {
    let _prepared = prepare_live_turn(&session, &request)?;
    let deps = ProductionTurnDeps {
        agent_loop,
        tool_definitions,
        runtime: super::cep::CepTurnRuntime::dummy(),
        cep_client: {
            let (client, _server) = lancet_native::CepTransport::pair(1);
            client
        },
        audit: Arc::new(lancet_native::CepAuditStore::in_memory().expect("in-memory audit store")),
        audit_context: Arc::new(tokio::sync::RwLock::new(
            lancet_native::CepAuditContext::default(),
        )),
        build_metrics: super::cep::CepRuntimeBuildMetrics {
            adapter_instances: 0,
            transport_pairs: 0,
        },
    };
    stream_turn_live(session, request, deps).await
}
