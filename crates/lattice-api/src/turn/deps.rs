use std::sync::Arc;

use lattice_agent::{AgentLoop, AgentTurnEvent, AgentTurnRequest, AgentTurnResult};
use lattice_session::{HeadTranscript, TurnCommitInput};

use super::types::RunTurnRequest;
use crate::session::{ApiError, Result, SessionApi};

/// Dependency injection seam for turn execution.
#[async_trait::async_trait]
pub trait TurnDeps: Send + Sync {
    fn create_session(&self, request: &RunTurnRequest) -> std::result::Result<String, ApiError>;

    fn open_session(&self, session_id: &str) -> std::result::Result<String, ApiError>;

    fn head_transcript(&self, session_id: &str) -> std::result::Result<HeadTranscript, ApiError>;

    async fn execute_agent_turn(
        &self,
        request: AgentTurnRequest,
    ) -> std::result::Result<(Vec<AgentTurnEvent>, AgentTurnResult), ApiError>;

    fn commit_turn(&self, input: TurnCommitInput) -> std::result::Result<String, ApiError>;
}

/// Production implementation of TurnDeps backed by SessionApi and AgentLoop.
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
        let mut result = self.agent_loop.run_turn(request).await;
        let events = std::mem::take(&mut result.events);
        Ok((events, result))
    }

    fn commit_turn(&self, input: TurnCommitInput) -> Result<String> {
        let commit = self.session.commit_turn(input)?;
        Ok(commit.commit_id)
    }
}
