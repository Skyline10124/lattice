use std::sync::Arc;

use futures::stream;
use lattice_agent::{AgentLoop, AgentTurnEvent, AgentTurnResult};
use lattice_session::{derive_commit_message, TurnCommitInput};

use super::deps::{RealTurnDeps, TurnDeps};
use super::mapping::{build_agent_request, map_status, map_tool_results, resolve_model_and_prompt};
use super::types::{
    RunTurnRequest, RunTurnResult, RunTurnStage, TurnEvent, TurnEventStream, TurnUsageSummary,
};
use crate::session::{ApiError, Result, SessionApi};

/// Core turn orchestration.
async fn execute_turn_inner(
    deps: &dyn TurnDeps,
    request: &RunTurnRequest,
) -> Result<(
    String,
    Vec<RunTurnStage>,
    Vec<AgentTurnEvent>,
    AgentTurnResult,
    String,
)> {
    let mut stages = Vec::new();

    stages.push(RunTurnStage::Profile);
    let (model, system_prompt, provider) = resolve_model_and_prompt(request);

    stages.push(RunTurnStage::Session);
    let session_id = match &request.session_id {
        Some(id) => deps.open_session(id)?,
        None => deps.create_session(request)?,
    };

    let transcript = deps.head_transcript(&session_id)?;
    let parent_ids = transcript
        .parent_commit_id
        .clone()
        .into_iter()
        .collect::<Vec<_>>();

    stages.push(RunTurnStage::Agent);
    let agent_request = build_agent_request(
        request,
        &session_id,
        transcript,
        model.clone(),
        system_prompt,
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

    Ok((session_id, stages, agent_events, agent_result, commit_id))
}

/// Execute a synchronous turn with injected dependencies.
pub async fn run_turn_with_deps(
    deps: &dyn TurnDeps,
    request: RunTurnRequest,
) -> Result<RunTurnResult> {
    let (session_id, stages, _agent_events, agent_result, commit_id) =
        execute_turn_inner(deps, &request).await?;

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

/// Execute a turn and replay its buffered events through injected deps.
///
/// See [`stream_turn`] for the live-vs-buffered note.
pub async fn stream_turn_with_deps(
    deps: &dyn TurnDeps,
    request: RunTurnRequest,
) -> Result<TurnEventStream> {
    let (session_id, _stages, agent_events, agent_result, commit_id) =
        execute_turn_inner(deps, &request).await?;

    let status = map_status(&agent_result.status);

    let mut turn_events: Vec<TurnEvent> = agent_events
        .into_iter()
        .map(|e| match e {
            AgentTurnEvent::Started => TurnEvent::Started,
            AgentTurnEvent::Token { content } => TurnEvent::Token { content },
            AgentTurnEvent::ToolCallStart { id, name } => TurnEvent::ToolCallStart { id, name },
            AgentTurnEvent::ToolCallResult {
                id,
                name,
                ok,
                output_len,
                ..
            } => TurnEvent::ToolResult {
                id,
                name,
                ok,
                output_len,
            },
            AgentTurnEvent::Done { finish_reason } => TurnEvent::Done { finish_reason },
            AgentTurnEvent::Error { message } => TurnEvent::Error { message },
            AgentTurnEvent::Cancelled => TurnEvent::Cancelled,
        })
        .collect();

    turn_events.push(TurnEvent::Committed {
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
/// [`TurnEvent::Committed`] — are replayed as a stream. This is *not* a
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
