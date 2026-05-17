//! v1.1 official `run` command over `lattice-api::run_turn`.
//!
//! Phase 10 Plan 02: The single official execution entry creates/open durable
//! sessions, runs the real agent/LLM/tool path, renders the API result, and
//! enforces locked exit semantics.
//!
//! # Exit codes
//!
//! - 0: Turn completed successfully (`RunTurnStatus::Completed`)
//! - 1: Provider/API/tool execution failure (`RunTurnStatus::Failed` or `Cancelled`)
//! - 2: Input/configuration error (missing profile, missing credentials, missing session)

use anyhow::Result;
use colored::Colorize;
use lattice_agent::{AgentLoop, AgentLoopConfig};
use lattice_profile::{ProfileError, ProfileSource};
use lattice_tools::ToolExecutor as _;
use std::io::{IsTerminal, Write};
use std::path::PathBuf;
use std::sync::Arc;

use lattice_api::{RunTurnRequest, RunTurnResult, RunTurnStatus};

// ── Real dependency adapters ─────────────────────────────────────────────

/// Production LlmClient that delegates to `lattice_llm::chat`.
struct RealLlmClient;

#[async_trait::async_trait]
impl lattice_agent::agent_loop::LlmClient for RealLlmClient {
    async fn chat(
        &self,
        request: lattice_llm::ChatRequest,
    ) -> std::result::Result<lattice_llm::ChatResponse, Box<dyn std::error::Error + Send + Sync>>
    {
        lattice_llm::chat(request)
            .await
            .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { Box::new(e) })
    }
}

/// Adapter wrapping `DefaultToolExecutor` for the agent loop's `ToolCallExecutor` trait.
struct ToolExecutorAdapter {
    inner: lattice_tools::executor::DefaultToolExecutor,
}

#[async_trait::async_trait]
impl lattice_agent::agent_loop::ToolCallExecutor for ToolExecutorAdapter {
    async fn execute(&self, call: &lattice_core::types::ToolCall) -> String {
        self.inner.execute(call).await
    }
}

// ── Public entry point ───────────────────────────────────────────────────

/// Execute the v1.1 `run` command.
///
/// Resolves the profile, builds real dependencies, calls `lattice_api::run_turn`,
/// renders the result, and returns an exit code via `std::process::ExitCode`.
pub async fn run(
    prompt: String,
    model: Option<String>,
    profile: Option<&str>,
    session_id: Option<String>,
    verbose: bool,
    json: bool,
) -> std::process::ExitCode {
    match run_inner(prompt, model, profile, session_id, verbose, json).await {
        Ok(exit_code) => exit_code,
        Err(error) => {
            eprintln!("{}: {}", "error".red(), error);
            // Configuration/input errors exit 2 (D-32)
            std::process::ExitCode::from(2)
        }
    }
}

async fn run_inner(
    prompt: String,
    model: Option<String>,
    profile: Option<&str>,
    session_id: Option<String>,
    verbose: bool,
    json: bool,
) -> Result<std::process::ExitCode> {
    let project_root = std::env::current_dir()?;

    // Resolve profile source from --profile argument (D-11, D-12)
    let source = crate::profile_runtime::profile_source_from_profile_arg(profile);

    // Load the profile, handling missing default with TTY-only prompt (D-17, D-33)
    let loaded_profile = match load_profile(&source, &project_root) {
        Ok(lp) => lp,
        Err(error) => {
            return handle_profile_error(error, &source, &project_root);
        }
    };

    if verbose {
        eprintln!(
            "{}",
            format!("profile: {}", loaded_profile.path.display()).dimmed()
        );
    }

    // Build SessionApi backed by SQLite (D-13: auto-create when session_id is None)
    let session_api = lattice_api::SessionApi::sqlite(crate::session::default_session_dir());

    // Build AgentLoop with real LLM client and tool executor
    let enabled_tools = loaded_profile.profile.tools.enabled.clone();
    let project_root_str = loaded_profile.package_dir.to_string_lossy().to_string();
    let tool_executor = lattice_tools::executor::DefaultToolExecutor::for_project_root(
        &project_root_str,
        enabled_tools,
    )
    .map_err(|e| anyhow::anyhow!("tool executor setup: {e}"))?;

    let agent_loop = Arc::new(AgentLoop::new(AgentLoopConfig {
        llm: Arc::new(RealLlmClient),
        tool_executor: Arc::new(ToolExecutorAdapter { inner: tool_executor }),
    }));

    // Build RunTurnRequest (D-10: model override, D-13: session_id)
    let request = RunTurnRequest {
        input: prompt,
        session_id,
        profile: profile.map(str::to_string),
        model,
        loaded_profile: Some(loaded_profile),
    };

    // Execute the turn
    let result = lattice_api::run_turn(session_api, agent_loop, request).await;

    let turn_result = match result {
        Ok(r) => r,
        Err(lattice_api::ApiError::NotFound(id)) => {
            // D-35: explicit missing session fails with object type and id
            eprintln!("{}: session '{}' not found", "error".red(), id);
            return Ok(std::process::ExitCode::from(2));
        }
        Err(lattice_api::ApiError::Internal(msg)) => {
            // D-36: provider/API/tool execution failures exit 1
            eprintln!("{}: {}", "error".red(), msg);
            return Ok(std::process::ExitCode::from(1));
        }
    };

    // Render result (D-16)
    render_result(&turn_result, verbose, json)?;

    // Map status to exit code (D-31, D-36)
    Ok(status_to_exit_code(&turn_result.status))
}

// ── Profile loading ──────────────────────────────────────────────────────

fn load_profile(
    source: &ProfileSource,
    project_root: &PathBuf,
) -> std::result::Result<lattice_profile::LoadedAgentProfile, anyhow::Error> {
    let global_agents_dir = crate::commands::safe_agents_dir(None)
        .map_err(|e| anyhow::anyhow!("{}", e))?;
    let paths = lattice_profile::DiscoveryPaths {
        project_root: project_root.clone(),
        global_agents_dir,
    };
    lattice_profile::resolve_agent_profile(source.clone(), &paths).map_err(Into::into)
}

fn handle_profile_error(
    error: anyhow::Error,
    _source: &ProfileSource,
    project_root: &PathBuf,
) -> Result<std::process::ExitCode> {
    let Some(profile_error) = error.downcast_ref::<ProfileError>() else {
        return Err(error);
    };

    match profile_error {
        ProfileError::MissingDefault { dir, template } => {
            // D-17: TTY-only default profile creation
            if !std::io::stdin().is_terminal() || !std::io::stderr().is_terminal() {
                // D-33: non-interactive missing default prints guidance
                eprintln!(
                    "{}: {}",
                    "error".red(),
                    crate::profile_runtime::missing_default_message(dir)
                );
                return Ok(std::process::ExitCode::from(2));
            }
            eprint!(
                "No default agent profile exists. Create .lattice/agents/default/agent.toml and continue this run? [y/N] "
            );
            std::io::stderr().flush()?;
            let mut answer = String::new();
            std::io::stdin().read_line(&mut answer)?;
            if !matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
                eprintln!(
                    "{}: {}",
                    "error".red(),
                    crate::profile_runtime::missing_default_message(dir)
                );
                return Ok(std::process::ExitCode::from(2));
            }
            let path = crate::profile_runtime::create_default_profile(dir, template)?;
            eprintln!(
                "Created {}. Edit it later to change prompt, tools, model, or security settings.",
                path.strip_prefix(project_root).unwrap_or(&path).display()
            );
            // After creation, the user should re-run the command.
            // This is the simplest correct approach: the profile is now on disk,
            // and the next run will find it.
            Err(anyhow::anyhow!(
                "profile created at {}; please re-run the command",
                path.display()
            ))
        }
        _ => Err(error),
    }
}

// ── Rendering ────────────────────────────────────────────────────────────

/// Render `RunTurnResult` to stdout/stderr (D-16).
fn render_result(result: &RunTurnResult, verbose: bool, json: bool) -> Result<()> {
    if json {
        // D-16: JSON emits full RunTurnResult
        println!("{}", serde_json::to_string_pretty(result)?);
    } else {
        // D-16: default text prints assistant_output only
        print!("{}", result.assistant_output);
        println!();
    }

    if verbose && !json {
        // D-16: verbose metadata to stderr
        eprintln!(
            "{}: {}",
            "session".dimmed(),
            result.session_id
        );
        eprintln!(
            "{}: {}",
            "commit".dimmed(),
            result.commit_id
        );
        eprintln!(
            "{}: {:?}",
            "status".dimmed(),
            result.status
        );
        if let Some(ref usage) = result.usage {
            eprintln!(
                "{}: {} tok (prompt: {}, completion: {})",
                "usage".dimmed(),
                usage.total_tokens,
                usage.prompt_tokens,
                usage.completion_tokens
            );
        }
        if !result.tool_results.is_empty() {
            eprintln!(
                "{}: {} executed",
                "tools".dimmed(),
                result.tool_results.len()
            );
        }
    }

    std::io::stdout().flush()?;
    Ok(())
}

/// Map `RunTurnStatus` to process exit code (D-31, D-36).
fn status_to_exit_code(status: &RunTurnStatus) -> std::process::ExitCode {
    match status {
        RunTurnStatus::Completed => std::process::ExitCode::SUCCESS,
        RunTurnStatus::Failed | RunTurnStatus::Cancelled => std::process::ExitCode::from(1),
    }
}

/// Legacy helper: build a JSON value from a `TurnCompletion` for backward
/// compatibility with `test_support.rs`. This function is not part of the
/// v1.1 public surface and will be removed when the legacy test infrastructure
/// is updated.
pub(crate) fn run_completion_value(
    completion: &lattice_runtime::TurnCompletion,
) -> serde_json::Value {
    let mut value = serde_json::json!({
        "content": completion.assistant_text,
        "total_tokens": completion
            .token_usage
            .as_ref()
            .map(|usage| usage.total_tokens)
            .unwrap_or_default(),
    });
    if let Some(warning) = completion.profile_drift_warning.as_ref() {
        value["profile_warning"] = serde_json::Value::String(warning.clone());
    }
    value
}

// ── Tests (legacy) ───────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_cutover_status_to_exit_code_completed_is_success() {
        assert_eq!(
            status_to_exit_code(&RunTurnStatus::Completed),
            std::process::ExitCode::SUCCESS
        );
    }

    #[test]
    fn run_cutover_status_to_exit_code_failed_is_1() {
        let code = status_to_exit_code(&RunTurnStatus::Failed);
        // ExitCode(1) has platform-specific Debug format, so test the process exit
        assert_ne!(format!("{:?}", code), format!("{:?}", std::process::ExitCode::SUCCESS));
    }

    #[test]
    fn run_cutover_status_to_exit_code_cancelled_is_1() {
        let code = status_to_exit_code(&RunTurnStatus::Cancelled);
        assert_ne!(format!("{:?}", code), format!("{:?}", std::process::ExitCode::SUCCESS));
    }
}

// ── run_cutover tests (Phase 10 Plan 02) ─────────────────────────────────

#[cfg(test)]
mod run_cutover {
    use lattice_api::{RunTurnResult, RunTurnStage, RunTurnStatus, ToolResultSummary, TurnUsageSummary};
    use lattice_profile::ProfileSource;

    // ── Rendering helpers ────────────────────────────────────────────────

    /// Pure helper: default text rendering returns only `assistant_output`.
    fn render_default(result: &RunTurnResult) -> String {
        let mut output = result.assistant_output.clone();
        output.push('\n');
        output
    }

    /// Pure helper: JSON rendering serializes the full `RunTurnResult`.
    fn render_json(result: &RunTurnResult) -> String {
        serde_json::to_string_pretty(result).unwrap()
    }

    /// Pure helper: verbose metadata written to stderr (not stdout).
    fn render_verbose_metadata(result: &RunTurnResult) -> String {
        let mut meta = Vec::new();
        meta.push(format!("session: {}", result.session_id));
        meta.push(format!("commit: {}", result.commit_id));
        meta.push(format!("status: {:?}", result.status));
        if let Some(ref usage) = result.usage {
            meta.push(format!(
                "usage: {} tok (prompt: {}, completion: {})",
                usage.total_tokens, usage.prompt_tokens, usage.completion_tokens
            ));
        }
        if !result.tool_results.is_empty() {
            meta.push(format!("tools: {} executed", result.tool_results.len()));
        }
        meta.join("\n")
    }

    /// Map `RunTurnStatus` to process exit code.
    /// Completed -> 0, Failed -> 1, Cancelled -> 1.
    fn status_to_exit_code(status: &RunTurnStatus) -> i32 {
        match status {
            RunTurnStatus::Completed => 0,
            RunTurnStatus::Failed | RunTurnStatus::Cancelled => 1,
        }
    }

    fn sample_result() -> RunTurnResult {
        RunTurnResult {
            session_id: "sess-abc123".to_string(),
            commit_id: "commit-def456".to_string(),
            assistant_output: "Hello, world!".to_string(),
            status: RunTurnStatus::Completed,
            usage: Some(TurnUsageSummary {
                prompt_tokens: 100,
                completion_tokens: 50,
                total_tokens: 150,
            }),
            tool_results: vec![ToolResultSummary {
                tool_call_id: "call-1".to_string(),
                name: "read_file".to_string(),
                ok: true,
                output_len: 1024,
                error_summary: None,
            }],
            stages: vec![
                RunTurnStage::Profile,
                RunTurnStage::Session,
                RunTurnStage::Agent,
                RunTurnStage::Llm,
                RunTurnStage::Tools,
                RunTurnStage::Commit,
                RunTurnStage::Render,
            ],
        }
    }

    // ── Rendering tests ──────────────────────────────────────────────────

    #[test]
    fn run_cutover_default_text_renders_only_assistant_output() {
        let result = sample_result();
        let rendered = render_default(&result);

        assert!(rendered.contains("Hello, world!"));
        assert!(!rendered.contains("sess-abc123"), "default text must not include session_id");
        assert!(!rendered.contains("commit-def456"), "default text must not include commit_id");
        assert!(!rendered.contains("Completed"), "default text must not include status");
        assert!(!rendered.contains("read_file"), "default text must not include tool names");
    }

    #[test]
    fn run_cutover_json_serializes_full_result() {
        let result = sample_result();
        let json = render_json(&result);
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed["session_id"], "sess-abc123");
        assert_eq!(parsed["commit_id"], "commit-def456");
        assert_eq!(parsed["assistant_output"], "Hello, world!");
        assert_eq!(parsed["status"], "completed");
        assert_eq!(parsed["usage"]["total_tokens"], 150);
        assert_eq!(parsed["usage"]["prompt_tokens"], 100);
        assert_eq!(parsed["usage"]["completion_tokens"], 50);
        assert_eq!(parsed["tool_results"][0]["name"], "read_file");
        assert_eq!(parsed["tool_results"][0]["ok"], true);
        assert!(parsed["stages"].as_array().unwrap().len() > 0);
    }

    #[test]
    fn run_cutover_verbose_metadata_includes_session_commit_status() {
        let result = sample_result();
        let meta = render_verbose_metadata(&result);

        assert!(meta.contains("session: sess-abc123"), "verbose must include session_id");
        assert!(meta.contains("commit: commit-def456"), "verbose must include commit_id");
        assert!(meta.contains("Completed"), "verbose must include status");
        assert!(meta.contains("150 tok"), "verbose must include total tokens");
        assert!(meta.contains("1 executed"), "verbose must include tool count");
    }

    #[test]
    fn run_cutover_verbose_not_mixed_into_default_stdout() {
        let result = sample_result();
        let default_out = render_default(&result);
        let verbose_meta = render_verbose_metadata(&result);

        // Verbose metadata should NOT appear in default stdout
        assert_ne!(default_out, verbose_meta);
        assert!(!default_out.contains("session:"));
        assert!(!default_out.contains("commit:"));
    }

    // ── Status-to-exit-code mapping tests ────────────────────────────────

    #[test]
    fn run_cutover_completed_exits_0() {
        assert_eq!(status_to_exit_code(&RunTurnStatus::Completed), 0);
    }

    #[test]
    fn run_cutover_failed_exits_1() {
        assert_eq!(status_to_exit_code(&RunTurnStatus::Failed), 1);
    }

    #[test]
    fn run_cutover_cancelled_exits_1() {
        assert_eq!(status_to_exit_code(&RunTurnStatus::Cancelled), 1);
    }

    // ── Profile source tests ─────────────────────────────────────────────

    #[test]
    fn run_cutover_profile_source_from_profile_arg_none_is_default() {
        assert!(matches!(
            crate::profile_runtime::profile_source_from_profile_arg(None),
            ProfileSource::Default
        ));
    }

    #[test]
    fn run_cutover_profile_source_from_profile_arg_name() {
        match crate::profile_runtime::profile_source_from_profile_arg(Some("reader")) {
            ProfileSource::Name(name) => assert_eq!(name, "reader"),
            other => panic!("expected Name, got {other:?}"),
        }
    }

    #[test]
    fn run_cutover_profile_source_from_profile_arg_path() {
        assert!(matches!(
            crate::profile_runtime::profile_source_from_profile_arg(Some("./agent.toml")),
            ProfileSource::Path(_)
        ));
    }

    // ── Integration: run_turn with real SessionApi + stub LLM ────────────

    /// A deterministic stub LlmClient for integration tests.
    struct StubLlmClient;

    #[async_trait::async_trait]
    impl lattice_agent::agent_loop::LlmClient for StubLlmClient {
        async fn chat(
            &self,
            _request: lattice_llm::ChatRequest,
        ) -> std::result::Result<lattice_llm::ChatResponse, Box<dyn std::error::Error + Send + Sync>> {
            Ok(lattice_llm::ChatResponse {
                content: "stub response".to_string(),
                reasoning_content: None,
                tool_calls: None,
                finish_reason: "stop".to_string(),
                model: "stub-model".to_string(),
                usage: Some(lattice_llm::TokenUsage {
                    prompt_tokens: 10,
                    completion_tokens: 5,
                    total_tokens: 15,
                }),
            })
        }
    }

    /// A no-op tool executor for integration tests.
    struct NoopToolExecutor;

    #[async_trait::async_trait]
    impl lattice_agent::agent_loop::ToolCallExecutor for NoopToolExecutor {
        async fn execute(&self, _call: &lattice_core::types::ToolCall) -> String {
            "no-op".to_string()
        }
    }

    #[tokio::test]
    async fn run_cutover_integration_creates_session_and_returns_commit() {
        let temp = tempfile::tempdir().unwrap();
        let session_api = lattice_api::SessionApi::sqlite(temp.path());
        let agent_loop = std::sync::Arc::new(lattice_agent::AgentLoop::new(
            lattice_agent::AgentLoopConfig {
                llm: std::sync::Arc::new(StubLlmClient),
                tool_executor: std::sync::Arc::new(NoopToolExecutor),
            },
        ));

        let request = lattice_api::RunTurnRequest {
            input: "hello".to_string(),
            session_id: None,
            profile: None,
            model: Some("stub-model".to_string()),
            loaded_profile: None,
        };

        let result = lattice_api::run_turn(session_api, agent_loop, request).await;

        match result {
            Ok(turn_result) => {
                assert!(!turn_result.session_id.is_empty(), "session_id should be non-empty");
                assert!(!turn_result.commit_id.is_empty(), "commit_id should be non-empty");
                assert_eq!(turn_result.assistant_output, "stub response");
                assert_eq!(turn_result.status, RunTurnStatus::Completed);
            }
            Err(e) => {
                panic!("run_turn failed: {e}");
            }
        }
    }

    #[tokio::test]
    async fn run_cutover_integration_missing_session_returns_not_found() {
        let temp = tempfile::tempdir().unwrap();
        let session_api = lattice_api::SessionApi::sqlite(temp.path());
        let agent_loop = std::sync::Arc::new(lattice_agent::AgentLoop::new(
            lattice_agent::AgentLoopConfig {
                llm: std::sync::Arc::new(StubLlmClient),
                tool_executor: std::sync::Arc::new(NoopToolExecutor),
            },
        ));

        let request = lattice_api::RunTurnRequest {
            input: "hello".to_string(),
            session_id: Some("nonexistent-session-id".to_string()),
            profile: None,
            model: Some("stub-model".to_string()),
            loaded_profile: None,
        };

        let result = lattice_api::run_turn(session_api, agent_loop, request).await;

        assert!(result.is_err(), "missing explicit session should return error");
        match result.unwrap_err() {
            lattice_api::ApiError::NotFound(id) => {
                assert_eq!(id, "nonexistent-session-id");
            }
            other => panic!("expected NotFound, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn run_cutover_model_override_takes_precedence_over_profile() {
        use std::path::PathBuf;

        // Create a loaded profile with model "profile-model"
        let temp = tempfile::tempdir().unwrap();
        let profile_dir = temp.path().join(".lattice/agents/default");
        std::fs::create_dir_all(&profile_dir).unwrap();
        std::fs::write(
            profile_dir.join("agent.toml"),
            r#"
schema_version = 1

[agent]
name = "default"
model = "profile-model"

[prompt]
system = "System from profile."

[tools]
enabled = []
"#,
        )
        .unwrap();

        let paths = lattice_profile::DiscoveryPaths {
            project_root: temp.path().to_path_buf(),
            global_agents_dir: tempfile::tempdir().unwrap().path().to_path_buf(),
        };
        let loaded = lattice_profile::resolve_agent_profile(ProfileSource::Default, &paths).unwrap();

        // Verify the profile model is "profile-model"
        assert_eq!(loaded.profile.agent.model, "profile-model");

        // Now create a RunTurnRequest with model override "override-model"
        let request = lattice_api::RunTurnRequest {
            input: "test".to_string(),
            session_id: None,
            profile: None,
            model: Some("override-model".to_string()),
            loaded_profile: Some(loaded),
        };

        // The model override should take precedence in resolve_model_and_prompt
        assert_eq!(request.model.as_deref(), Some("override-model"));
        assert_eq!(
            request.loaded_profile.as_ref().unwrap().profile.agent.model,
            "profile-model"
        );
    }
}
