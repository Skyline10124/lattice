use std::sync::Arc;

use futures::stream;
use lattice_agent::{AgentLoop, AgentTurnEvent, AgentTurnResult};
use lattice_session::{derive_commit_message, TurnCommitInput};

use super::deps::{RealTurnDeps, TurnDeps};
use super::mapping::{build_agent_request, map_status, map_tool_results, resolve_model_and_prompt};
use super::types::{
    RunTurnRequest, RunTurnResult, RunTurnStage, StreamTurnRequest, TurnEvent, TurnEventStream,
    TurnUsageSummary,
};
use crate::session::{ApiError, Result, SessionApi};

/// Core turn orchestration shared by `run_turn_with_deps` and `stream_turn_with_deps`.
///
/// Returns: (session_id, stages, agent_events, agent_result, commit_id)
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

    // Stage: Profile
    stages.push(RunTurnStage::Profile);
    let (model, system_prompt, provider) = resolve_model_and_prompt(request);

    // Stage: Session — create or open
    stages.push(RunTurnStage::Session);
    let session_id = match &request.session_id {
        Some(id) => deps.open_session(id)?,
        None => deps.create_session(request)?,
    };

    // Read transcript
    let transcript = deps.head_transcript(&session_id)?;

    // Stage: Agent
    stages.push(RunTurnStage::Agent);
    let agent_request = build_agent_request(request, &session_id, transcript, model, system_prompt);

    let (agent_events, agent_result) = deps
        .execute_agent_turn(agent_request)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;

    // Stage: LLM (agent loop completed the LLM calls)
    stages.push(RunTurnStage::Llm);

    // Stage: Tools (agent loop executed tools if any)
    stages.push(RunTurnStage::Tools);

    // Build commit input from agent result
    let commit_status = map_status(&agent_result.status).to_commit_status();

    // Build the messages to commit: user input + all generated messages
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

    // Re-read transcript to get parent_commit_id for the commit
    let transcript_for_parent = deps.head_transcript(&session_id)?;
    let parent_ids = transcript_for_parent
        .parent_commit_id
        .into_iter()
        .collect::<Vec<_>>();

    let profile_snapshot_id = request
        .loaded_profile
        .as_ref()
        .map(|lp| lp.profile.agent.name.clone());

    let model_name = request
        .loaded_profile
        .as_ref()
        .map(|lp| lp.profile.agent.model.clone())
        .or_else(|| request.model.clone())
        .unwrap_or_else(|| "sonnet".to_string());

    let commit_input = TurnCommitInput {
        session_id: session_id.clone(),
        parent_ids,
        messages: commit_messages,
        profile_snapshot_id,
        model: model_name,
        provider,
        status: commit_status,
        commit_message,
        usage: Some(agent_result.usage.clone()),
        terminal_reason: agent_result.terminal_reason.clone(),
    };

    // Stage: Commit
    stages.push(RunTurnStage::Commit);
    let commit_id = deps.commit_turn(commit_input)?;

    // Stage: Render
    stages.push(RunTurnStage::Render);

    Ok((session_id, stages, agent_events, agent_result, commit_id))
}

/// Execute a synchronous turn with injected dependencies.
///
/// This is the testable entry point. Tests inject a fake [`TurnDeps`] to
/// avoid live provider calls, direct SQLite, or public exposure of internal
/// handles.
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

/// Execute a synchronous turn.
///
/// Creates or opens a session, reads the current transcript, invokes the
/// agent loop, commits exactly one turn-level commit, and returns the
/// complete typed result.
///
/// When `session_id` is `None`, a new session is automatically created (D-42).
/// When `session_id` is provided, the existing session is opened and its
/// transcript is used as context.
pub async fn run_turn(
    session: SessionApi,
    agent_loop: Arc<AgentLoop>,
    request: RunTurnRequest,
) -> Result<RunTurnResult> {
    let deps = RealTurnDeps::new(session, agent_loop);
    run_turn_with_deps(&deps, request).await
}

/// Execute a streaming turn with injected dependencies.
///
/// This is the testable entry point for streaming.
pub async fn stream_turn_with_deps(
    deps: &dyn TurnDeps,
    request: RunTurnRequest,
) -> Result<TurnEventStream> {
    let (session_id, _stages, agent_events, agent_result, commit_id) =
        execute_turn_inner(deps, &request).await?;

    let status = map_status(&agent_result.status);

    // Convert agent events to public TurnEvents
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

    // Append final Committed event after persistence
    turn_events.push(TurnEvent::Committed {
        session_id,
        commit_id,
        status,
    });

    Ok(Box::pin(stream::iter(turn_events)))
}

/// Execute a streaming turn.
///
/// Creates or opens a session, invokes the agent loop, forwards live
/// agent/token/tool events, commits the turn, and appends a final
/// `Committed` event with the durable persistence outcome.
pub async fn stream_turn(
    session: SessionApi,
    agent_loop: Arc<AgentLoop>,
    request: StreamTurnRequest,
) -> Result<TurnEventStream> {
    let run_request = RunTurnRequest {
        input: request.input,
        session_id: request.session_id,
        profile: request.profile,
        model: request.model,
        loaded_profile: request.loaded_profile,
    };
    let deps = RealTurnDeps::new(session, agent_loop);
    stream_turn_with_deps(&deps, run_request).await
}
