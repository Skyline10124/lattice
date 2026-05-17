//! Isolated v1.1 skeleton command handler.
//!
//! Calls through `lattice-api::run_turn` with a `RunTurnRequest` constructed
//! from the CLI prompt, then prints the stub assistant output to stdout.
//! Uses the deterministic `StubLlm` so no provider credentials are needed.

use std::sync::Arc;

use lattice_agent::agent_loop::{LlmClient, ToolCallExecutor};
use lattice_agent::{AgentLoop, AgentLoopConfig};
use lattice_llm::StubLlm;

/// Adapter that wraps [`StubLlm`] to satisfy the [`LlmClient`] trait.
struct StubLlmAdapter(StubLlm);

#[async_trait::async_trait]
impl LlmClient for StubLlmAdapter {
    async fn chat(
        &self,
        request: lattice_llm::ChatRequest,
    ) -> Result<lattice_llm::ChatResponse, Box<dyn std::error::Error + Send + Sync>> {
        self.0.chat(request).await.map_err(|e| e.into())
    }
}

/// No-op tool executor for the skeleton path — tools are not exercised.
struct NoopToolExecutor;

#[async_trait::async_trait]
impl ToolCallExecutor for NoopToolExecutor {
    async fn execute(&self, _call: &lattice_core::types::ToolCall) -> String {
        String::new()
    }
}

/// Build the default [`SessionApi`] and [`AgentLoop`] for the skeleton CLI path.
fn build_deps() -> (lattice_api::SessionApi, Arc<AgentLoop>) {
    let session = lattice_api::SessionApi::sqlite(".lattice/sessions");
    let agent_loop = Arc::new(AgentLoop::new(AgentLoopConfig {
        llm: Arc::new(StubLlmAdapter(StubLlm::new())),
        tool_executor: Arc::new(NoopToolExecutor),
    }));
    (session, agent_loop)
}

/// Run the v1.1 skeleton path with the given prompt.
///
/// Builds a `lattice_api::RunTurnRequest` from the prompt with all optionals
/// set to `None`, calls `lattice_api::run_turn`, and prints the resulting
/// assistant output to stdout.
pub async fn run(prompt: String) -> anyhow::Result<()> {
    let (session, agent_loop) = build_deps();
    let request = lattice_api::RunTurnRequest {
        input: prompt,
        session_id: None,
        profile: None,
        model: None,
        loaded_profile: None,
    };
    let result = lattice_api::run_turn(session, agent_loop, request).await?;
    let output = render_result(&result);
    println!("{}", output);
    Ok(())
}

/// Render the assistant output from a `RunTurnResult`.
///
/// Pure helper that extracts the displayable output from a turn result.
/// Factored for testability so that stdout capture is not needed in unit tests.
fn render_result(result: &lattice_api::RunTurnResult) -> String {
    result.assistant_output.clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_result(assistant_output: &str) -> lattice_api::RunTurnResult {
        lattice_api::RunTurnResult {
            session_id: "test-session".to_string(),
            commit_id: "test-commit".to_string(),
            assistant_output: assistant_output.to_string(),
            status: lattice_api::RunTurnStatus::Completed,
            usage: None,
            tool_results: vec![],
            stages: vec![
                lattice_api::RunTurnStage::Profile,
                lattice_api::RunTurnStage::Session,
                lattice_api::RunTurnStage::Agent,
                lattice_api::RunTurnStage::Llm,
                lattice_api::RunTurnStage::Tools,
                lattice_api::RunTurnStage::Commit,
                lattice_api::RunTurnStage::Render,
            ],
        }
    }

    #[test]
    fn render_result_returns_assistant_output() {
        let result = make_result("stub response: hello");
        let rendered = render_result(&result);
        assert_eq!(rendered, "stub response: hello");
    }

    #[tokio::test]
    async fn run_v11_skeleton_produces_output() {
        let tmp = tempfile::tempdir().unwrap();
        let session = lattice_api::SessionApi::sqlite(tmp.path());
        let agent_loop = Arc::new(AgentLoop::new(AgentLoopConfig {
            llm: Arc::new(StubLlmAdapter(StubLlm::new())),
            tool_executor: Arc::new(NoopToolExecutor),
        }));
        let request = lattice_api::RunTurnRequest {
            input: "hello".to_string(),
            session_id: None,
            profile: None,
            model: None,
            loaded_profile: None,
        };
        let result = lattice_api::run_turn(session, agent_loop, request).await;
        assert!(result.is_ok(), "skeleton run should succeed: {:?}", result.err());
    }
}
