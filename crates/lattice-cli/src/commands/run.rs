//! v1.1 official `run` command over `lattice-api::run_turn`.
//!
//! # Exit codes
//!
//! - 0: Turn completed successfully (`RunTurnStatus::Completed`)
//! - 1: Provider/API/tool execution failure (`RunTurnStatus::Failed` or `Cancelled`)
//! - 2: Input/configuration error (missing profile, missing credentials, missing session)

#![allow(dead_code)]
use anyhow::Result;
use colored::Colorize;
use lattice_agent::{AgentLoop, AgentLoopConfig};
use lattice_profile::{ProfileError, ProfileSource};
use lattice_tools::ToolExecutor as _;
use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
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
    async fn execute(&self, call: &lattice_llm::ToolCall) -> String {
        self.inner.execute(call).await
    }
}

// ── Public entry point ───────────────────────────────────────────────────

/// Execute the v1.1 `run` command.
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
    let source = profile_source_from_profile_arg(profile);

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

    // Build SessionApi backed by SQLite
    let session_dir = default_session_dir();
    let session_api = lattice_api::SessionApi::sqlite(session_dir);

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
        tool_executor: Arc::new(ToolExecutorAdapter {
            inner: tool_executor,
        }),
    }));

    let request = RunTurnRequest {
        input: prompt,
        session_id,
        profile: profile.map(str::to_string),
        model,
        loaded_profile: Some(loaded_profile),
    };

    let result = lattice_api::run_turn(session_api, agent_loop, request).await;

    let turn_result = match result {
        Ok(r) => r,
        Err(lattice_api::ApiError::NotFound(id)) => {
            eprintln!("{}: session '{}' not found", "error".red(), id);
            return Ok(std::process::ExitCode::from(2));
        }
        Err(lattice_api::ApiError::Internal(msg)) => {
            eprintln!("{}: {}", "error".red(), msg);
            return Ok(std::process::ExitCode::from(1));
        }
    };

    render_result(&turn_result, verbose, json)?;
    Ok(status_to_exit_code(&turn_result.status))
}

// ── Profile helpers ──────────────────────────────────────────────────────

fn default_session_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("lattice")
        .join("sessions")
}

fn profile_source_from_profile_arg(profile: Option<&str>) -> ProfileSource {
    let Some(value) = profile.map(str::trim).filter(|v| !v.is_empty()) else {
        return ProfileSource::Default;
    };
    let has_path_separator = value.contains('/') || (cfg!(windows) && value.contains('\\'));
    let is_path = Path::new(value).is_absolute()
        || value.starts_with("./")
        || value.starts_with("../")
        || has_path_separator
        || value.ends_with(".toml");
    if is_path {
        ProfileSource::Path(PathBuf::from(value))
    } else {
        ProfileSource::Name(value.to_string())
    }
}

fn load_profile(
    source: &ProfileSource,
    project_root: &Path,
) -> std::result::Result<lattice_profile::LoadedAgentProfile, anyhow::Error> {
    let global_agents_dir = safe_agents_dir(None).map_err(|e| anyhow::anyhow!("{}", e))?;
    let paths = lattice_profile::DiscoveryPaths {
        project_root: project_root.to_path_buf(),
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
            if !std::io::stdin().is_terminal() || !std::io::stderr().is_terminal() {
                eprintln!("{}: {}", "error".red(), missing_default_message(dir));
                return Ok(std::process::ExitCode::from(2));
            }
            eprint!(
                "No default agent profile exists. Create .lattice/agents/default/agent.toml and continue this run? [y/N] "
            );
            std::io::stderr().flush()?;
            let mut answer = String::new();
            std::io::stdin().read_line(&mut answer)?;
            if !matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
                eprintln!("{}: {}", "error".red(), missing_default_message(dir));
                return Ok(std::process::ExitCode::from(2));
            }
            let path = create_default_profile(dir, template)?;
            eprintln!(
                "Created {}. Edit it later to change prompt, tools, model, or security settings.",
                path.strip_prefix(project_root).unwrap_or(&path).display()
            );
            Err(anyhow::anyhow!(
                "profile created at {}; please re-run the command",
                path.display()
            ))
        }
        _ => Err(error),
    }
}

fn missing_default_message(default_agent_dir: &Path) -> String {
    let profile_path = default_agent_dir.join("agent.toml");
    format!(
        "No default agent profile found at {}. Create one before running non-interactively.",
        profile_path.display()
    )
}

fn create_default_profile(dir: &Path, template: &str) -> Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let path = dir.join("agent.toml");
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    let mut file = options.open(&path)?;
    use std::io::Write as _;
    file.write_all(template.as_bytes())?;
    Ok(path)
}

fn safe_agents_dir(override_path: Option<&str>) -> Result<PathBuf, String> {
    let path = if let Some(p) = override_path {
        PathBuf::from(p)
    } else if let Ok(dir) = std::env::var("LATTICE_AGENTS_DIR") {
        PathBuf::from(dir)
    } else if let Ok(home) = std::env::var("HOME") {
        PathBuf::from(home).join(".lattice").join("agents")
    } else {
        PathBuf::from(".lattice/agents")
    };

    validate_agents_dir(&path)?;
    Ok(path)
}

fn validate_agents_dir(path: &Path) -> Result<(), String> {
    let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let path_str = canonical.to_string_lossy();

    let dangerous_prefixes = [
        "/etc", "/usr", "/var", "/sys", "/proc", "/dev", "/boot", "/root",
    ];
    for prefix in &dangerous_prefixes {
        if path_str.starts_with(prefix) {
            return Err(format!(
                "Agents directory '{}' is inside a system path ({}). \
                 Set LATTICE_AGENTS_DIR to a safe location (e.g., under $HOME or your project).",
                path.display(),
                prefix
            ));
        }
    }

    if path_str.starts_with("/tmp") {
        return Err(format!(
            "Agents directory '{}' is inside /tmp which is world-writable and unsafe. \
             Set LATTICE_AGENTS_DIR to a safe location.",
            path.display()
        ));
    }

    Ok(())
}

// ── Rendering ────────────────────────────────────────────────────────────

fn render_result(result: &RunTurnResult, verbose: bool, json: bool) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(result)?);
    } else {
        print!("{}", result.assistant_output);
        println!();
    }

    if verbose && !json {
        eprintln!("{}: {}", "session".dimmed(), result.session_id);
        eprintln!("{}: {}", "commit".dimmed(), result.commit_id);
        eprintln!("{}: {:?}", "status".dimmed(), result.status);
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

fn status_to_exit_code(status: &RunTurnStatus) -> std::process::ExitCode {
    match status {
        RunTurnStatus::Completed => std::process::ExitCode::SUCCESS,
        RunTurnStatus::Failed | RunTurnStatus::Cancelled => std::process::ExitCode::from(1),
    }
}

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_status_to_exit_code_completed_is_success() {
        assert_eq!(
            status_to_exit_code(&RunTurnStatus::Completed),
            std::process::ExitCode::SUCCESS
        );
    }

    #[test]
    fn run_status_to_exit_code_failed_is_1() {
        let code = status_to_exit_code(&RunTurnStatus::Failed);
        assert_ne!(
            format!("{:?}", code),
            format!("{:?}", std::process::ExitCode::SUCCESS)
        );
    }

    #[test]
    fn run_status_to_exit_code_cancelled_is_1() {
        let code = status_to_exit_code(&RunTurnStatus::Cancelled);
        assert_ne!(
            format!("{:?}", code),
            format!("{:?}", std::process::ExitCode::SUCCESS)
        );
    }

    #[test]
    fn run_profile_source_none_is_default() {
        assert!(matches!(
            profile_source_from_profile_arg(None),
            ProfileSource::Default
        ));
    }

    #[test]
    fn run_profile_source_name() {
        match profile_source_from_profile_arg(Some("reader")) {
            ProfileSource::Name(name) => assert_eq!(name, "reader"),
            other => panic!("expected Name, got {other:?}"),
        }
    }

    #[test]
    fn run_profile_source_path() {
        assert!(matches!(
            profile_source_from_profile_arg(Some("./agent.toml")),
            ProfileSource::Path(_)
        ));
    }

    #[test]
    fn run_missing_default_message_references_profile_path() {
        let dir = Path::new("/tmp/.lattice/agents/default");
        let msg = missing_default_message(dir);
        assert!(msg.contains("agent.toml"));
        assert!(msg.contains("/tmp/.lattice/agents/default"));
    }

    #[test]
    fn run_default_session_dir_under_lattice_sessions() {
        let dir = default_session_dir();
        assert!(dir.to_string_lossy().contains("lattice"));
        assert!(dir.to_string_lossy().contains("sessions"));
    }

    // ── Integration: run_turn with stub LLM ────────────────────────────────

    struct StubLlmClient;

    #[async_trait::async_trait]
    impl lattice_agent::agent_loop::LlmClient for StubLlmClient {
        async fn chat(
            &self,
            _request: lattice_llm::ChatRequest,
        ) -> std::result::Result<lattice_llm::ChatResponse, Box<dyn std::error::Error + Send + Sync>>
        {
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

    struct NoopToolExecutor;

    #[async_trait::async_trait]
    impl lattice_agent::agent_loop::ToolCallExecutor for NoopToolExecutor {
        async fn execute(&self, _call: &lattice_llm::ToolCall) -> String {
            "no-op".to_string()
        }
    }

    #[tokio::test]
    async fn run_integration_creates_session_and_returns_commit() {
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
                assert!(!turn_result.session_id.is_empty());
                assert!(!turn_result.commit_id.is_empty());
                assert_eq!(turn_result.assistant_output, "stub response");
                assert_eq!(turn_result.status, RunTurnStatus::Completed);
            }
            Err(e) => {
                panic!("run_turn failed: {e}");
            }
        }
    }

    #[tokio::test]
    async fn run_integration_missing_session_returns_not_found() {
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

        assert!(result.is_err());
        match result.unwrap_err() {
            lattice_api::ApiError::NotFound(id) => {
                assert_eq!(id, "nonexistent-session-id");
            }
            other => panic!("expected NotFound, got {other:?}"),
        }
    }

    // ── Boundary: no old crate imports ──────────────────────────────────────

    #[test]
    fn run_source_has_no_old_crate_imports() {
        let source = include_str!("run.rs");
        let test_start = source.find("#[cfg(test)]").unwrap_or(source.len());
        let prod = &source[..test_start];
        let forbidden = [
            "lattice_core",
            "lattice_runtime",
            "lattice_prompt",
            "lattice_sandbox",
            "lattice_policy",
        ];
        for pattern in forbidden {
            assert!(
                !prod.contains(pattern),
                "run.rs production code must not contain '{pattern}'"
            );
        }
    }
}
