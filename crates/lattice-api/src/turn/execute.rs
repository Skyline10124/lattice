use std::sync::Arc;

use futures::stream;
use lattice_agent::{AgentLoop, AgentTurnEvent, AgentTurnResult, AgentTurnStatus};
use lattice_session::{derive_commit_message, TurnCommitInput};

use super::deps::{RealTurnDeps, TurnDeps};
use super::mapping::{build_agent_request, map_status, map_tool_results, resolve_model_and_prompt};
use super::types::{
    RunTurnRequest, RunTurnResult, RunTurnStage, TurnEvent, TurnEventStream, TurnUsageSummary,
};
use crate::session::{ApiError, Result, SessionApi};

/// Output of [`execute_turn_inner`] threaded back to the public facades.
struct ExecutedTurn {
    session_id: String,
    parent_commit_id: Option<String>,
    stages: Vec<RunTurnStage>,
    agent_events: Vec<AgentTurnEvent>,
    agent_result: AgentTurnResult,
    commit_id: String,
}

/// Core turn orchestration.
async fn execute_turn_inner(deps: &dyn TurnDeps, request: &RunTurnRequest) -> Result<ExecutedTurn> {
    let mut stages = Vec::new();

    stages.push(RunTurnStage::Profile);
    let (model, system_prompt, provider) = resolve_model_and_prompt(request);

    stages.push(RunTurnStage::Session);
    let session_id = match &request.session_id {
        Some(id) => deps.open_session(id)?,
        None => deps.create_session(request)?,
    };

    let transcript = deps.head_transcript(&session_id)?;
    let parent_commit_id = transcript.parent_commit_id.clone();
    let parent_ids = parent_commit_id.clone().into_iter().collect::<Vec<_>>();

    stages.push(RunTurnStage::Agent);
    let agent_request = build_agent_request(
        request,
        &session_id,
        transcript,
        model.clone(),
        system_prompt,
        Some(provider.clone()).filter(|p| p != "unknown"),
    );

    let (agent_events, agent_result) = deps
        .execute_agent_turn(agent_request)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;

    stages.push(RunTurnStage::Llm);

    stages.push(RunTurnStage::Tools);

    let commit_status = map_status(&agent_result.status).to_commit_status();

    let mut commit_messages = Vec::new();
    commit_messages.push(lattice_session::GraphMessage {
        role: "user".to_string(),
        content: request.input.clone(),
        reasoning_content: None,
        tool_calls: None,
        tool_call_id: None,
        name: None,
    });
    commit_messages.extend(agent_result.messages.iter().cloned());

    let commit_message = derive_commit_message(&commit_messages);

    let profile_snapshot_id = request
        .loaded_profile
        .as_ref()
        .map(|lp| lp.profile.agent.name.clone());

    let commit_input = TurnCommitInput {
        session_id: session_id.clone(),
        parent_ids,
        messages: commit_messages,
        profile_snapshot_id,
        model: model.clone(),
        provider,
        status: commit_status,
        commit_message,
        usage: Some(agent_result.usage.clone()),
        terminal_reason: agent_result.terminal_reason.clone(),
    };

    stages.push(RunTurnStage::Commit);
    let commit_id = deps.commit_turn(commit_input)?;

    stages.push(RunTurnStage::Render);

    Ok(ExecutedTurn {
        session_id,
        parent_commit_id,
        stages,
        agent_events,
        agent_result,
        commit_id,
    })
}

/// Execute a synchronous turn with injected dependencies.
pub async fn run_turn_with_deps(
    deps: &dyn TurnDeps,
    request: RunTurnRequest,
) -> Result<RunTurnResult> {
    let ExecutedTurn {
        session_id,
        stages,
        agent_result,
        commit_id,
        ..
    } = execute_turn_inner(deps, &request).await?;

    Ok(RunTurnResult {
        session_id,
        commit_id,
        assistant_output: agent_result.assistant_output,
        status: map_status(&agent_result.status),
        usage: Some(TurnUsageSummary::from(agent_result.usage)),
        tool_results: map_tool_results(&agent_result.tool_results),
        stages,
    })
}

/// Execute a turn and return its final result once it has been committed.
///
/// This is the synchronous facade: callers get a single
/// [`RunTurnResult`] after the turn completes.
pub async fn run_turn(
    session: SessionApi,
    agent_loop: Arc<AgentLoop>,
    request: RunTurnRequest,
) -> Result<RunTurnResult> {
    let deps = RealTurnDeps::new(session, agent_loop);
    run_turn_with_deps(&deps, request).await
}

/// Map a single agent process event to its public [`TurnEvent`] equivalent.
///
/// This is a 1:1 mapping over the six process variants. Public terminal
/// events (`TurnFailed`, `TurnCancelled`) and commit events are derived
/// separately from `AgentTurnResult.status` and the commit outcome.
fn map_agent_event(event: AgentTurnEvent) -> TurnEvent {
    match event {
        AgentTurnEvent::ModelStarted { provider, model } => {
            TurnEvent::ModelStarted { provider, model }
        }
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
    }
}

/// Execute a turn and replay its buffered events through injected deps.
///
/// See [`stream_turn`] for the live-vs-buffered note.
pub async fn stream_turn_with_deps(
    deps: &dyn TurnDeps,
    request: RunTurnRequest,
) -> Result<TurnEventStream> {
    let ExecutedTurn {
        session_id,
        parent_commit_id,
        agent_events,
        agent_result,
        commit_id,
        ..
    } = execute_turn_inner(deps, &request).await?;

    let status = map_status(&agent_result.status);

    let mut turn_events: Vec<TurnEvent> = Vec::with_capacity(agent_events.len() + 4);

    // First public event: TurnStarted carries session and lineage context.
    // `branch` is None in the buffered path; the live path (Phase 16) will
    // populate it from the prepared turn context.
    turn_events.push(TurnEvent::TurnStarted {
        session_id: session_id.clone(),
        parent_commit_id,
        branch: None,
    });

    // Process events: 1:1 mapping over the agent's process-only vocabulary.
    turn_events.extend(agent_events.into_iter().map(map_agent_event));

    // Terminal event derived from agent_result.status.
    // (No terminal events come from the agent loop itself — that's by design.)
    match agent_result.status {
        AgentTurnStatus::Error => {
            turn_events.push(TurnEvent::TurnFailed {
                reason: agent_result.terminal_reason.clone().unwrap_or_default(),
            });
        }
        AgentTurnStatus::Cancelled => {
            turn_events.push(TurnEvent::TurnCancelled {
                reason: agent_result.terminal_reason.clone(),
            });
        }
        AgentTurnStatus::Completed => {}
    }

    // Commit lifecycle. The buffered path commits before constructing the
    // stream; if the commit had failed, we'd have returned `Err` from
    // `execute_turn_inner` before reaching here. `CommitFailed` exists in
    // the contract for Phase 16's live path.
    turn_events.push(TurnEvent::TurnCommitting);
    turn_events.push(TurnEvent::TurnCommitted {
        session_id,
        commit_id,
        status,
    });

    Ok(Box::pin(stream::iter(turn_events)))
}

/// Execute a turn and return its events as a stream.
///
/// **Current behavior (v1.1):** the turn is executed to completion and
/// committed first, then the buffered events — including the terminal
/// [`TurnEvent::TurnCommitted`] — are replayed as a stream. This is *not* a
/// live event feed: callers will not observe tokens or tool results until
/// the turn finishes. The live event stream lands in alpha-v1.2.
pub async fn stream_turn(
    session: SessionApi,
    agent_loop: Arc<AgentLoop>,
    request: RunTurnRequest,
) -> Result<TurnEventStream> {
    let deps = RealTurnDeps::new(session, agent_loop);
    stream_turn_with_deps(&deps, request).await
}
