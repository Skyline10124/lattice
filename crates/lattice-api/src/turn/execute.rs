use std::sync::Arc;

use futures::StreamExt;
use lattice_agent::{AgentLoop, AgentTurnEvent, AgentTurnResult, AgentTurnStatus};
use lattice_session::{derive_commit_message, GraphMessage, TurnCommitInput};

use super::collector::RunTurnCollector;
use super::mapping::map_status;
use super::types::{RunTurnRequest, RunTurnResult, TurnEvent, TurnEventStream, TurnUsageSummary};
use crate::session::{Result, SessionApi};

/// Context needed to construct a [`TurnCommitInput`] from the live path.
pub(super) struct TurnCommitContext {
    pub session_id: String,
    pub parent_ids: Vec<String>,
    pub model: String,
    pub provider: String,
    pub profile_snapshot_id: Option<String>,
}

/// Build a [`TurnCommitInput`] from the user input, commit context, and agent
/// result. Shared by the live turn path to ensure commit construction stays
/// consistent (D-16).
pub(super) fn build_commit_input(
    user_input: &str,
    ctx: &TurnCommitContext,
    agent_result: &AgentTurnResult,
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
        session_id: ctx.session_id.clone(),
        parent_ids: ctx.parent_ids.clone(),
        messages: commit_messages,
        profile_snapshot_id: ctx.profile_snapshot_id.clone(),
        model: ctx.model.clone(),
        provider: ctx.provider.clone(),
        status: commit_status,
        commit_message,
        usage: Some(agent_result.usage.clone()),
        terminal_reason: agent_result.terminal_reason.clone(),
    }
}

/// Derive the terminal [`TurnEvent`] from the agent result status (D-10).
///
/// Returns `None` for `Completed` (no terminal event needed), `Some(TurnFailed)`
/// for `Error`, and `Some(TurnCancelled)` for `Cancelled`. Terminal status is
/// always derived from `AgentTurnResult.status`, never from the presence or
/// absence of `ModelFinished` in the event stream (WR-01).
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
///
/// This is the synchronous facade: callers get a single
/// [`RunTurnResult`] after the turn completes. Internally this drains the
/// live [`stream_turn`] event stream via [`RunTurnCollector`].
pub async fn run_turn(
    session: SessionApi,
    agent_loop: Arc<AgentLoop>,
    request: RunTurnRequest,
) -> Result<RunTurnResult> {
    let mut stream = stream_turn(session, agent_loop, request).await?;
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

/// Execute a turn and return its events as a live stream.
///
/// Startup failures are returned as `Result::Err`; runtime outcomes are
/// emitted as [`TurnEvent`] values within the stream.
pub async fn stream_turn(
    session: SessionApi,
    agent_loop: Arc<AgentLoop>,
    request: RunTurnRequest,
) -> Result<TurnEventStream> {
    super::live::stream_turn_live(session, agent_loop, request).await
}
