// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Live turn streaming path (live turn).
//!
//! This module implements the live `stream_turn` path: startup validation via
//! [`prepare_live_turn`], a spawned background task ([`run_live_turn_task`])
//! that drains agent events concurrently via `tokio::select!`, and emits public
//! [`TurnEvent`] values through a bounded channel before committing.
//!
//! The spawned task classifies the terminal outcome using the lifecycle state
//! machine, ensuring every committed turn carries an explicit
//! `(status, disposition)` pair.

use std::collections::{HashSet, VecDeque};
use std::time::Duration;

use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

use lattice_agent::{AgentTurnEvent, AgentTurnResult, AgentTurnStatus};
use lattice_session::{GraphHead, TurnCommitStatus};

use lancet_api::store_turn_attestation;

use super::cep::{ProductionTurnDeps, map_cep_tool_event};
use super::execute::{
    TurnCommitContext, build_commit_input, build_profile_snapshot_metadata, derive_terminal_event,
    map_agent_event,
};
use super::lifecycle::{
    LifecycleFailureKind, TurnLifecycleOutcome, classify_lifecycle_failure, outcome_workspace_less,
};
use super::mapping::{build_agent_request, map_status, resolve_model_and_prompt};
use super::types::{RunTurnRequest, TurnEvent, TurnEventStream};
use super::upper::UpperPrepareError;
use crate::session::{OpenSessionRequest, Result, SessionApi};

/// Bounded channel capacity for the turn event stream.
pub(super) const TURN_EVENT_CHANNEL_CAPACITY: usize = 256;

/// Max wait when forwarding a non-delta [`TurnEvent`] to a slow observer.
/// After timeout, further public events are skipped but the turn still commits.
const OBSERVER_SEND_TIMEOUT: Duration = Duration::from_millis(500);

/// Short grace period for CEP side-channel events after the agent future
/// returns. The CEP server emits call-finished before the tool response, but
/// live forwarding crosses a separate channel.
const CEP_EVENT_DRAIN_GRACE: Duration = Duration::from_millis(50);

/// All state gathered during startup validation, before the live task spawns.
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
pub(crate) fn prepare_live_turn(
    session: &SessionApi,
    request: &RunTurnRequest,
) -> Result<PreparedTurn> {
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

/// Best-effort public observer sink.
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

#[derive(Default)]
struct CepOrderedEmitter {
    pending_calls: HashSet<u64>,
    buffered: VecDeque<TurnEvent>,
}

impl CepOrderedEmitter {
    async fn emit(&mut self, observer: &mut ObserverSink, event: TurnEvent) {
        match event {
            TurnEvent::CepCallStarted { call_id, .. } => {
                self.pending_calls.insert(call_id);
                observer.emit(event).await;
            }
            TurnEvent::CepCallFinished { call_id, .. } => {
                observer.emit(event).await;
                self.pending_calls.remove(&call_id);
                if self.pending_calls.is_empty() {
                    self.flush_buffered(observer).await;
                }
            }
            event if is_cep_event(&event) => {
                observer.emit(event).await;
                if self.pending_calls.is_empty() {
                    self.flush_buffered(observer).await;
                }
            }
            event if self.pending_calls.is_empty() && !is_tool_call_finished(&event) => {
                self.flush_buffered(observer).await;
                observer.emit(event).await;
            }
            event => {
                self.buffered.push_back(event);
            }
        }
    }

    async fn finish(&mut self, observer: &mut ObserverSink) {
        self.pending_calls.clear();
        self.flush_buffered(observer).await;
    }

    async fn flush_buffered(&mut self, observer: &mut ObserverSink) {
        while let Some(event) = self.buffered.pop_front() {
            observer.emit(event).await;
        }
    }
}

fn is_tool_call_finished(event: &TurnEvent) -> bool {
    matches!(event, TurnEvent::ToolCallFinished { .. })
}

fn is_cep_event(event: &TurnEvent) -> bool {
    matches!(
        event,
        TurnEvent::CepCallStarted { .. }
            | TurnEvent::CepCallFinished { .. }
            | TurnEvent::CepVerifyFailed { .. }
    )
}

/// Classify an agent result into a typed lifecycle outcome.
///
/// For the current non-promotion flow (no candidate export, no workspace
/// verification), the classification is:
///
/// - **Completed** → `outcome_workspace_less()` (NotApplicable, no resource action).
/// - **Error** → `Aborted + Destroy` (agent error before candidate).
/// - **Cancelled** → `Aborted + Destroy` (cancelled before candidate).
///
/// When promotion is wired in a future plan, this function will be extended
/// to handle `candidate_exported = true` with verification and promotion
/// outcomes.
pub(super) fn classify_live_outcome(
    agent_result: &AgentTurnResult,
    _candidate_exported: bool,
) -> TurnLifecycleOutcome {
    match agent_result.status {
        AgentTurnStatus::Completed => outcome_workspace_less(),
        AgentTurnStatus::Error => classify_lifecycle_failure(
            TurnCommitStatus::Error,
            false,
            false,
            LifecycleFailureKind::AgentErrorBeforeCandidate,
        ),
        AgentTurnStatus::Cancelled => classify_lifecycle_failure(
            TurnCommitStatus::Cancelled,
            false,
            false,
            LifecycleFailureKind::CancelledBeforeCandidate,
        ),
    }
}

/// Background task that runs the agent loop, maps events live, and commits.
///
/// Event emission order: TurnStarted -> [agent events] -> [terminal] ->
/// TurnCommitting -> TurnCommitted/CommitFailed. The sender is dropped after
/// the final event, closing the stream.
///
/// The committed outcome carries an explicit `(status, disposition)` pair
/// classified by [`classify_live_outcome`].
async fn run_live_turn_task(
    session: SessionApi,
    prepared: PreparedTurn,
    request: RunTurnRequest,
    turn_tx: mpsc::Sender<TurnEvent>,
    deps: ProductionTurnDeps,
) {
    let ProductionTurnDeps {
        agent_loop,
        tool_definitions,
        runtime,
        cep_client,
        audit,
        audit_context,
        upper_lease,
        ..
    } = deps;

    let mut upper_lease = upper_lease;
    let mut runtime = runtime;

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
            if let Some(agent_event) = map_cep_tool_event(event)
                && cep_agent_tx.send(agent_event).await.is_err()
            {
                break;
            }
        }
    });

    // 4. Pin agent future
    let mut agent_fut = Box::pin(agent_loop.run_turn_live(agent_request, agent_tx));
    let mut ordered_events = CepOrderedEmitter::default();

    // 5. Concurrent drain loop
    let mut agent_result: AgentTurnResult = loop {
        tokio::select! {
            maybe_event = agent_rx.recv() => {
                if let Some(event) = maybe_event {
                    ordered_events.emit(&mut observer, map_agent_event(event)).await;
                }
            }
            result = &mut agent_fut => {
                break result;
            }
        }
    };

    // 6. Post-loop drain
    while let Ok(Some(event)) = tokio::time::timeout(CEP_EVENT_DRAIN_GRACE, agent_rx.recv()).await {
        ordered_events
            .emit(&mut observer, map_agent_event(event))
            .await;
    }

    cep_forwarder.abort();
    while let Ok(event) = agent_rx.try_recv() {
        ordered_events
            .emit(&mut observer, map_agent_event(event))
            .await;
    }
    ordered_events.finish(&mut observer).await;

    // 6a. Classify the lifecycle outcome BEFORE resource teardown.
    let outcome = classify_live_outcome(&agent_result, false);

    runtime.shutdown().await;
    audit.await_pending().await;

    // 6b. Resource teardown: destroy the Upper (no snapshot retained in the
    // current non-promotion flow).
    if let Err(err) = upper_lease.destroy().await {
        agent_result = fold_upper_destroy_error(agent_result, err);
    }

    {
        let ctx = audit_context.read().await;
        if let Err(err) = store_turn_attestation(&audit, &ctx, &turn_id) {
            eprintln!("failed to store turn attestation for {turn_id}: {err}");
        }
    }

    // 7. Terminal event derived from AgentTurnResult.status.
    if let Some(terminal) = derive_terminal_event(&agent_result) {
        observer.emit(terminal).await;
    }

    // 8. Emit TurnCommitting
    observer.emit(TurnEvent::TurnCommitting).await;

    // 9. Build commit context and input with lifecycle-classified disposition.
    let ctx = TurnCommitContext {
        session_id: prepared.session_id.clone(),
        parent_ids: prepared.parent_ids,
        model: prepared.model,
        provider: prepared.provider,
        agent_name: prepared.agent_name,
        profile_snapshot_hash: prepared.profile_snapshot_hash,
        profile_snapshot_json: prepared.profile_snapshot_json,
    };

    // pass the classified disposition and lifecycle evidence
    // explicitly so build_commit_input cannot silently default them. The live
    // path attaches no TurnLifecycleEvidence at this point (it is populated by
    // the promotion/finalize path); pass None to preserve prior behavior.
    let commit_input = build_commit_input(
        &request.input,
        &ctx,
        &agent_result,
        outcome.disposition.clone(),
        None,
    );

    // 10. Commit and emit result.
    match session.commit_turn(commit_input) {
        Ok(commit) => {
            let status = map_status(&agent_result.status);
            observer
                .emit(TurnEvent::TurnCommitted {
                    session_id: prepared.session_id,
                    commit_id: commit.commit_id,
                    status,
                    disposition: commit.disposition,
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
}

fn fold_upper_destroy_error(
    mut result: AgentTurnResult,
    err: UpperPrepareError,
) -> AgentTurnResult {
    if matches!(result.status, lattice_agent::AgentTurnStatus::Completed) {
        result.status = lattice_agent::AgentTurnStatus::Error;
        result.terminal_reason = Some(format!("Upper teardown failed: {err}"));
    } else if let Some(reason) = result.terminal_reason.as_mut() {
        reason.push_str(&format!("; Upper teardown failed: {err}"));
    } else {
        result.terminal_reason = Some(format!("Upper teardown failed: {err}"));
    }
    result
}

/// Execute a turn with an already-prepared session snapshot and return a live stream.
pub(super) async fn stream_turn_live_prepared(
    session: SessionApi,
    prepared: PreparedTurn,
    request: RunTurnRequest,
    deps: ProductionTurnDeps,
) -> Result<TurnEventStream> {
    let (tx, rx) = mpsc::channel::<TurnEvent>(TURN_EVENT_CHANNEL_CAPACITY);

    tokio::spawn(async move {
        run_live_turn_task(session, prepared, request, tx, deps).await;
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
    let prepared = prepare_live_turn(&session, &request)?;
    stream_turn_live_prepared(session, prepared, request, deps).await
}

#[cfg(test)]
mod tests {
    use tokio::sync::mpsc::error::TryRecvError;

    use super::*;

    fn cep_started(call_id: u64) -> TurnEvent {
        TurnEvent::CepCallStarted {
            call_id,
            method: "session.todo_write".to_string(),
            impact_kind: "session_state_mutation".to_string(),
            audit_id: 7,
            semantic_stage: "cep_native".to_string(),
            guard_stage: "cep_policy_lifecycle".to_string(),
        }
    }

    fn cep_finished(call_id: u64) -> TurnEvent {
        TurnEvent::CepCallFinished {
            call_id,
            method: "session.todo_write".to_string(),
            impact_kind: "session_state_mutation".to_string(),
            audit_id: 7,
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
            Some(TurnEvent::CepCallStarted { call_id: 1, .. })
        ));
        assert!(matches!(
            rx.recv().await,
            Some(TurnEvent::CepCallFinished { call_id: 1, .. })
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

    // ── Live activation lifecycle tests (Task 3) ────────────────────────────

    fn make_agent_result(status: AgentTurnStatus) -> AgentTurnResult {
        let terminal_reason = if matches!(status, AgentTurnStatus::Completed) {
            None
        } else {
            Some("test reason".to_string())
        };
        AgentTurnResult {
            status,
            messages: vec![],
            assistant_output: String::new(),
            usage: lattice_session::TurnUsage {
                prompt_tokens: 0,
                completion_tokens: 0,
                total_tokens: 0,
            },
            tool_results: vec![],
            terminal_reason,
            events: vec![],
            cleanup_exhausted: false,
        }
    }

    #[test]
    fn live_activation_completed_classifies_as_workspace_less() {
        let result = make_agent_result(AgentTurnStatus::Completed);
        let outcome = classify_live_outcome(&result, false);

        assert_eq!(outcome.status, TurnCommitStatus::Completed);
        assert_eq!(
            outcome.disposition,
            lattice_session::WorkspaceDisposition::NotApplicable
        );
        assert_eq!(
            outcome.resource_action,
            super::super::lifecycle::ResourceAction::Destroy
        );
    }

    #[test]
    fn live_activation_error_classifies_as_aborted() {
        let result = make_agent_result(AgentTurnStatus::Error);
        let outcome = classify_live_outcome(&result, false);

        assert_eq!(outcome.status, TurnCommitStatus::Error);
        assert_eq!(
            outcome.disposition,
            lattice_session::WorkspaceDisposition::Aborted
        );
        assert_eq!(
            outcome.resource_action,
            super::super::lifecycle::ResourceAction::Destroy
        );
    }

    #[test]
    fn live_activation_cancelled_classifies_as_aborted() {
        let result = make_agent_result(AgentTurnStatus::Cancelled);
        let outcome = classify_live_outcome(&result, false);

        assert_eq!(outcome.status, TurnCommitStatus::Cancelled);
        assert_eq!(
            outcome.disposition,
            lattice_session::WorkspaceDisposition::Aborted
        );
        assert_eq!(
            outcome.resource_action,
            super::super::lifecycle::ResourceAction::Destroy
        );
    }

    #[test]
    fn live_activation_outcome_messages_are_typed() {
        let completed =
            classify_live_outcome(&make_agent_result(AgentTurnStatus::Completed), false);
        assert!(
            completed
                .messages
                .iter()
                .any(|m| m.contains("no workspace"))
        );

        let errored = classify_live_outcome(&make_agent_result(AgentTurnStatus::Error), false);
        assert!(errored.messages.iter().any(|m| m.contains("agent error")));

        let cancelled =
            classify_live_outcome(&make_agent_result(AgentTurnStatus::Cancelled), false);
        assert!(cancelled.messages.iter().any(|m| m.contains("cancelled")));
    }

    #[test]
    fn live_activation_no_candidate_exported_flag() {
        let statuses: [(AgentTurnStatus, &str); 3] = [
            (AgentTurnStatus::Completed, "Completed"),
            (AgentTurnStatus::Error, "Error"),
            (AgentTurnStatus::Cancelled, "Cancelled"),
        ];
        for (status, name) in statuses {
            let result = make_agent_result(status);
            let outcome = classify_live_outcome(&result, false);
            assert!(
                !outcome.candidate_exported,
                "candidate_exported must be false for {name}"
            );
        }
    }
}
