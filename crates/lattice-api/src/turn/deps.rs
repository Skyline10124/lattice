use std::sync::Arc;

use lattice_agent::{AgentLoop, AgentTurnEvent, AgentTurnRequest, AgentTurnResult};
use lattice_session::{HeadTranscript, TurnCommitInput};

use super::types::RunTurnRequest;
use crate::session::{ApiError, Result, SessionApi};

/// Dependency injection seam for turn execution.
///
/// Tests inject a fake implementation; production uses [`RealTurnDeps`].
#[async_trait::async_trait]
pub trait TurnDeps: Send + Sync {
    /// Create a new session and return its ID.
    fn create_session(&self, request: &RunTurnRequest) -> std::result::Result<String, ApiError>;

    /// Open an existing session and return its ID (or error if not found).
    fn open_session(&self, session_id: &str) -> std::result::Result<String, ApiError>;

    /// Read the current HEAD transcript for a session.
    fn head_transcript(&self, session_id: &str) -> std::result::Result<HeadTranscript, ApiError>;

    /// Execute the agent turn. Returns (events, result).
    async fn execute_agent_turn(
        &self,
        request: AgentTurnRequest,
    ) -> std::result::Result<(Vec<AgentTurnEvent>, AgentTurnResult), ApiError>;

    /// Commit a turn to durable storage. Returns the commit ID.
    fn commit_turn(&self, input: TurnCommitInput) -> std::result::Result<String, ApiError>;
}

/// Production implementation of [`TurnDeps`] backed by [`SessionApi`] and
/// [`AgentLoop`].
pub struct RealTurnDeps {
    session: SessionApi,
    agent_loop: Arc<AgentLoop>,
}

impl RealTurnDeps {
    pub fn new(session: SessionApi, agent_loop: Arc<AgentLoop>) -> Self {
        Self {
            session,
            agent_loop,
        }
    }
}

#[async_trait::async_trait]
impl TurnDeps for RealTurnDeps {
    fn create_session(&self, request: &RunTurnRequest) -> Result<String> {
        let session = self
            .session
            .create_session(crate::session::CreateSessionRequest {
                label: None,
                profile: request.profile.clone(),
                model: request.model.clone(),
            })?;
        Ok(session.session_id)
    }

    fn open_session(&self, session_id: &str) -> Result<String> {
        let session = self
            .session
            .open_session(crate::session::OpenSessionRequest {
                id: session_id.to_string(),
            })?;
        Ok(session.session_id)
    }

    fn head_transcript(&self, session_id: &str) -> Result<HeadTranscript> {
        self.session.head_transcript(session_id)
    }

    async fn execute_agent_turn(
        &self,
        request: AgentTurnRequest,
    ) -> std::result::Result<(Vec<AgentTurnEvent>, AgentTurnResult), ApiError> {
        let result = self.agent_loop.run_turn(request).await;
        Ok((result.events.clone(), result))
    }

    fn commit_turn(&self, input: TurnCommitInput) -> Result<String> {
        let commit = self.session.commit_turn(input)?;
        Ok(commit.commit_id)
    }
}
