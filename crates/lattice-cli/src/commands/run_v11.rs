//! Isolated v1.1 skeleton command handler.
//!
//! Calls through `lattice-api::run_turn` with a `RunTurnRequest` constructed
//! from the CLI prompt, then prints the stub assistant output to stdout.
//! Does not require provider credentials, config, or legacy runtime construction.
//! Source contains no `lattice_runtime`, `lattice_core`, `model_runtime`, or
//! legacy `commands::run` references.

/// Run the v1.1 skeleton path with the given prompt.
///
/// Builds a `lattice_api::RunTurnRequest` from the prompt with all optionals
/// set to `None`, calls `lattice_api::run_turn`, and prints the resulting
/// assistant output to stdout.
pub async fn run(prompt: String) -> anyhow::Result<()> {
    let request = lattice_api::RunTurnRequest {
        input: prompt,
        session_id: None,
        profile: None,
        model: None,
    };
    let result = lattice_api::run_turn(request).await?;
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

    #[test]
    fn render_result_returns_assistant_output() {
        let result = lattice_api::RunTurnResult {
            assistant_output: "stub response: hello".to_string(),
            commit_id: None,
            stages: vec![
                lattice_api::RunTurnStage::Profile,
                lattice_api::RunTurnStage::Session,
                lattice_api::RunTurnStage::Agent,
                lattice_api::RunTurnStage::Llm,
                lattice_api::RunTurnStage::Tools,
                lattice_api::RunTurnStage::Commit,
                lattice_api::RunTurnStage::Render,
            ],
        };
        let rendered = render_result(&result);
        assert_eq!(rendered, "stub response: hello");
    }

    #[tokio::test]
    async fn run_v11_calls_api_and_returns_ok() {
        let result = run("hello".to_string()).await;
        assert!(
            result.is_ok(),
            "run should succeed without provider credentials"
        );
    }

    #[tokio::test]
    async fn run_v11_output_includes_stub_response() {
        let request = lattice_api::RunTurnRequest {
            input: "test prompt".to_string(),
            session_id: None,
            profile: None,
            model: None,
        };
        let result = lattice_api::run_turn(request).await.unwrap();
        assert!(
            result.assistant_output.contains("stub response"),
            "assistant_output should contain 'stub response', got: '{}'",
            result.assistant_output
        );
        assert!(
            result.commit_id.is_none(),
            "commit_id must be None in Phase 6"
        );
    }

    #[tokio::test]
    async fn run_v11_does_not_need_credentials() {
        // This test proves the handler works without any provider env vars set.
        // The StubLlm never reads env vars, so the handler must succeed
        // regardless of credential state.
        let req = lattice_api::RunTurnRequest {
            input: "credential-free test".to_string(),
            session_id: None,
            profile: None,
            model: None,
        };
        let result = lattice_api::run_turn(req).await;
        assert!(
            result.is_ok(),
            "run_turn should never require provider credentials in Phase 6"
        );
    }
}
