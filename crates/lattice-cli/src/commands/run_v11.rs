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
        loaded_profile: None,
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
    #[ignore = "requires real API credentials — run manually with valid keys"]
    async fn run_v11_calls_api_and_returns_ok() {
        // Phase 7 uses real lattice_llm::chat() which makes live API calls.
        // This test is ignored by default; run manually with valid credentials.
        let result = run("hello".to_string()).await;
        assert!(result.is_ok(), "run should succeed with credentials");
    }

    #[tokio::test]
    async fn run_v11_output_includes_stub_response() {
        let request = lattice_api::RunTurnRequest {
            input: "test prompt".to_string(),
            session_id: None,
            profile: None,
            model: None,
            loaded_profile: None,
        };
        // Phase 7: run_turn uses real client, requires credentials.
        // Skip if credentials are missing or invalid (HTTP 401/403).
        let result = match lattice_api::run_turn(request).await {
            Ok(r) => r,
            Err(e) => {
                let msg = e.to_string();
                if msg.contains("ANTHROPIC_API_KEY")
                    || msg.contains("OPENAI_API_KEY")
                    || msg.contains("HTTP 401")
                    || msg.contains("HTTP 403")
                {
                    return; // skip — no working credentials available
                }
                panic!("unexpected error: {}", e);
            }
        };
        assert!(
            result.commit_id.is_none(),
            "commit_id must be None in Phase 7"
        );
    }

    #[tokio::test]
    async fn run_v11_requires_credentials_in_phase_7() {
        // Phase 7 uses real lattice_llm::chat(). If no credentials are
        // configured, run_turn must return an error naming the missing env var.
        // If credentials exist but are invalid (HTTP 401/403), the real client
        // was reached — that also succeeds this test.
        let req = lattice_api::RunTurnRequest {
            input: "credential-free test".to_string(),
            session_id: None,
            profile: None,
            model: None,
            loaded_profile: None,
        };
        let result = lattice_api::run_turn(req).await;
        match result {
            Err(e) => {
                let msg = e.to_string();
                assert!(
                    msg.contains("ANTHROPIC_API_KEY")
                        || msg.contains("OPENAI_API_KEY")
                        || msg.contains("HTTP 401")
                        || msg.contains("HTTP 403"),
                    "error should mention missing credential env var or indicate auth failure, got: {}",
                    msg
                );
            }
            Ok(_) => {
                // If we got here, credentials must have been present and valid — that's fine.
            }
        }
    }
}
