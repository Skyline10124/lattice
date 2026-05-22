//! `lattice run` command built on `lattice_api::stream_turn`.
//!
//! Consumes a live `TurnEventStream` and renders events incrementally:
//! `TokenDelta` content goes to stdout, all status/lifecycle/error lines
//! go to stderr in human mode, and `--json` emits one `TurnEvent` JSON
//! object per line on stdout (JSONL).
//!
//! # JSONL mode (`--json`)
//!
//! `--json` emits one `TurnEvent` JSON object per line on stdout,
//! terminated by `\n`. This is an alpha behavior change from the v1.1
//! pretty-printed `RunTurnResult`. Human status lines and verbose epilogue
//! are suppressed in JSON mode. JSONL emits public `TurnEvent` values as
//! received, including `ReasoningDelta` when present (D-D3 / D-D4).
//!
//! # Exit codes
//!
//! - 0: Turn committed successfully (`RunTurnStatus::Completed`)
//! - 1:  Provider/API/tool failure, commit failure, terminal without commit, or protocol violation
//! - 2: Input/configuration error (missing profile, missing credentials, missing session)

use anyhow::Result;
use colored::Colorize;
use lattice_agent::{AgentLoop, AgentLoopConfig};
use lattice_profile::{ProfileError, ProfileSource};
use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use lattice_api::{RunTurnRequest, RunTurnStatus, TurnEvent, TurnUsageSummary};

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

    async fn stream_chat(
        &self,
        request: lattice_llm::ChatRequest,
    ) -> std::result::Result<lattice_llm::LlmStream, Box<dyn std::error::Error + Send + Sync>> {
        lattice_llm::stream(request)
            .await
            .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { Box::new(e) })
    }
}

/// Terminal signal accumulated from `TurnFailed` or `TurnCancelled`.
#[derive(Debug, Clone, PartialEq, Eq)]
enum TerminalSignal {
    Failed { reason: String },
    Cancelled { reason: Option<String> },
}

/// Commit signal accumulated from `TurnCommitted` or `CommitFailed`.
#[derive(Debug, Clone, PartialEq, Eq)]
enum CommitSignal {
    Committed,
    Failed { reason: String },
}

/// Accumulated state from a live `TurnEvent` stream, used to derive
/// the CLI exit code after the stream drains (D-A3 / D-A4).
///
/// `usage_total` is summed across multiple `ModelFinished` events;
/// `TurnCommitted` is NOT a usage source (D-A3).
#[derive(Debug, Default)]
struct RunRenderState {
    last_terminal: Option<TerminalSignal>,
    last_commit: Option<CommitSignal>,
    last_status: Option<RunTurnStatus>,
    tokens_emitted_without_newline: bool,
    mid_token_stderr_newline_applied: bool,
    session_id: Option<String>,
    commit_id: Option<String>,
    branch: Option<String>,
    usage_total: Option<TurnUsageSummary>,
    tool_finished_count: usize,
}

/// Update `RunRenderState` from each `TurnEvent` variant (D-A3).
fn apply_event_to_state(event: &TurnEvent, state: &mut RunRenderState) {
    match event {
        TurnEvent::TurnStarted {
            session_id, branch, ..
        } => {
            state.session_id = Some(session_id.clone());
            state.branch = branch.clone();
        }
        TurnEvent::ModelFinished { usage: Some(u), .. } => {
            if let Some(existing) = &mut state.usage_total {
                existing.prompt_tokens += u.prompt_tokens;
                existing.completion_tokens += u.completion_tokens;
                existing.total_tokens += u.total_tokens;
            } else {
                state.usage_total = Some(u.clone());
            }
        }
        TurnEvent::TokenDelta { content } if !content.is_empty() => {
            state.tokens_emitted_without_newline = !content.ends_with('\n');
            if !state.tokens_emitted_without_newline {
                state.mid_token_stderr_newline_applied = false;
            }
        }
        TurnEvent::ToolCallFinished { .. } => {
            state.tool_finished_count += 1;
        }
        TurnEvent::TurnFailed { reason } => {
            state.last_terminal = Some(TerminalSignal::Failed {
                reason: reason.clone(),
            });
        }
        TurnEvent::TurnCancelled { reason } => {
            state.last_terminal = Some(TerminalSignal::Cancelled {
                reason: reason.clone(),
            });
        }
        TurnEvent::TurnCommitting => {}
        TurnEvent::TurnCommitted {
            session_id,
            commit_id,
            status,
        } => {
            state.last_commit = Some(CommitSignal::Committed);
            state.session_id = Some(session_id.clone());
            state.commit_id = Some(commit_id.clone());
            state.last_status = Some(status.clone());
        }
        TurnEvent::CommitFailed { reason } => {
            state.last_commit = Some(CommitSignal::Failed {
                reason: reason.clone(),
            });
        }
        // All other variants (ModelStarted, ReasoningDelta,
        // ToolCallStarted, ModelFinished without usage): no state mutation.
        _ => {}
    }
}

/// Derive exit code from accumulated stream signals (D-A4 rows 1-7).
///
/// Row 8 (ApiError::NotFound -> exit 2) and Row 9 (ApiError::Internal -> exit 1)
/// are handled in `run_inner` before the stream starts.
fn exit_code_from_state(state: &RunRenderState) -> std::process::ExitCode {
    // Row 4: CommitFailed -> 1 (regardless of last_status)
    if matches!(state.last_commit, Some(CommitSignal::Failed { .. })) {
        return std::process::ExitCode::from(1);
    }
    // Defensive: if TurnFailed/TurnCancelled was seen, exit 1 regardless of commit status
    if state.last_terminal.is_some() {
        return std::process::ExitCode::from(1);
    }
    // Row 1: TurnCommitted{Completed} -> 0
    if state.last_status == Some(RunTurnStatus::Completed) {
        return std::process::ExitCode::SUCCESS;
    }
    // Row 2/3: TurnCommitted{Error|Cancelled} -> 1
    if state.last_status == Some(RunTurnStatus::Error)
        || state.last_status == Some(RunTurnStatus::Cancelled)
    {
        return std::process::ExitCode::from(1);
    }
    // Row 7: Stream ended without any terminal/commit signal -> 1
    std::process::ExitCode::from(1)
}

/// Render a `TurnEvent` as one JSONL line on `out` (D-D1).
///
/// Each call emits exactly one `serde_json::to_string` line followed
/// by `\n` via `writeln!`. No ANSI escapes are produced (D-B6 / D-D3).
fn render_json_event<W: std::io::Write>(event: &TurnEvent, out: &mut W) -> std::io::Result<()> {
    let line = serde_json::to_string(event).expect("TurnEvent serializes infallibly");
    writeln!(out, "{}", line)
}

/// Render a `TurnEvent` for human consumption.
///
/// `TokenDelta` content goes to `out`; all status/lifecycle lines go to `err`.
/// Before writing any status line to `err`, if `state.tokens_emitted_without_newline`
/// is true, the mid-token newline trick is applied: flush `out`, emit `\n` on `err`
/// (D-B3). This function does not mutate `state` — `apply_event_to_state` owns that.
fn render_human_event<WOut: Write, WErr: Write>(
    event: &TurnEvent,
    state: &mut RunRenderState,
    out: &mut WOut,
    err: &mut WErr,
    verbose: bool,
) -> std::io::Result<()> {
    match event {
        TurnEvent::TokenDelta { content } => {
            write!(out, "{}", content)?;
        }
        TurnEvent::TurnStarted {
            session_id, branch, ..
        } => {
            if verbose {
                flush_before_status(out, err, state)?;
                if let Some(b) = branch {
                    writeln!(err, "{} turn {} on {}", "·".dimmed(), session_id, b)?;
                } else {
                    writeln!(err, "{} session {}", "·".dimmed(), session_id)?;
                }
            }
        }
        TurnEvent::ModelStarted { provider, model } => {
            if verbose {
                flush_before_status(out, err, state)?;
                if let Some(p) = provider {
                    writeln!(err, "{} model {}/{model}", "·".dimmed(), p)?;
                } else {
                    writeln!(err, "{} model {model}", "·".dimmed())?;
                }
            }
        }
        TurnEvent::ReasoningDelta { content } => {
            if verbose {
                flush_before_status(out, err, state)?;
                writeln!(err, "reasoning: {}", content.dimmed())?;
            }
        }
        TurnEvent::ModelFinished { finish_reason, .. } => {
            if verbose {
                flush_before_status(out, err, state)?;
                writeln!(err, "{} model finished {finish_reason}", "✓".green())?;
            }
        }
        TurnEvent::ToolCallStarted { name, .. } => {
            flush_before_status(out, err, state)?;
            writeln!(err, "{} tool {name}", "·".dimmed())?;
        }
        TurnEvent::ToolCallFinished {
            name,
            ok,
            output_len,
            error_summary,
            ..
        } => {
            flush_before_status(out, err, state)?;
            if *ok {
                writeln!(err, "{} tool {name} {output_len}B", "✓".green())?;
            } else {
                let summary = error_summary.as_deref().unwrap_or("error");
                writeln!(err, "{} tool {name} {output_len}B: {summary}", "✗".red())?;
            }
        }
        TurnEvent::TurnFailed { reason } => {
            flush_before_status(out, err, state)?;
            writeln!(err, "{} failed: {reason}", "✗".red())?;
        }
        TurnEvent::TurnCancelled { reason } => {
            flush_before_status(out, err, state)?;
            if let Some(r) = reason {
                writeln!(err, "{} cancelled: {r}", "✗".red())?;
            } else {
                writeln!(err, "{} cancelled", "✗".red())?;
            }
        }
        TurnEvent::TurnCommitting => {
            flush_before_status(out, err, state)?;
            writeln!(err, "{} committing", "·".dimmed())?;
        }
        TurnEvent::TurnCommitted {
            commit_id, status, ..
        } => {
            flush_before_status(out, err, state)?;
            let status_str = match status {
                RunTurnStatus::Completed => "completed",
                RunTurnStatus::Error => "error",
                RunTurnStatus::Cancelled => "cancelled",
            };
            writeln!(err, "{} committed {commit_id} {status_str}", "✓".green())?;
        }
        TurnEvent::CommitFailed { reason } => {
            flush_before_status(out, err, state)?;
            writeln!(err, "{} commit failed: {reason}", "✗".red())?;
        }
    }
    Ok(())
}

/// Apply mid-token newline trick (D-B3): if stdout has emitted tokens
/// without a trailing newline, flush stdout and emit `\n` on stderr so
/// the status line begins on its own row in mixed-stream terminals.
fn flush_before_status<WOut: Write, WErr: Write>(
    out: &mut WOut,
    err: &mut WErr,
    state: &mut RunRenderState,
) -> std::io::Result<()> {
    if state.tokens_emitted_without_newline && !state.mid_token_stderr_newline_applied {
        out.flush()?;
        writeln!(err)?;
        state.mid_token_stderr_newline_applied = true;
    }
    Ok(())
}

/// Render the verbose epilogue after the stream drains (D-C3).
///
/// Emits dimmed labels for populated state fields on stderr only:
/// session, commit, status, usage (from RunRenderState.usage_total, NOT TurnCommitted),
/// tools (from tool_finished_count).
fn render_verbose_epilogue<WErr: Write>(
    state: &RunRenderState,
    err: &mut WErr,
) -> std::io::Result<()> {
    if let Some(ref session_id) = state.session_id {
        writeln!(err, "{}: {}", "session".dimmed(), session_id)?;
    }
    if let Some(ref commit_id) = state.commit_id {
        writeln!(err, "{}: {}", "commit".dimmed(), commit_id)?;
    }
    if let Some(ref status) = state.last_status {
        let status_str = match status {
            RunTurnStatus::Completed => "completed",
            RunTurnStatus::Error => "error",
            RunTurnStatus::Cancelled => "cancelled",
        };
        writeln!(err, "{}: {}", "status".dimmed(), status_str)?;
    }
    if let Some(ref usage) = state.usage_total {
        writeln!(
            err,
            "{}: {} tok (prompt: {}, completion: {})",
            "usage".dimmed(),
            usage.total_tokens,
            usage.prompt_tokens,
            usage.completion_tokens
        )?;
    }
    if state.tool_finished_count > 0 {
        writeln!(
            err,
            "{}: {} executed",
            "tools".dimmed(),
            state.tool_finished_count
        )?;
    }
    Ok(())
}

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
    let project_root_str = project_root.to_string_lossy().to_string();
    let tool_executor = lattice_tools::executor::DefaultToolExecutor::for_project_root(
        &project_root_str,
        enabled_tools,
    )
    .map_err(|e| anyhow::anyhow!("tool executor setup: {e}"))?;

    let agent_loop = Arc::new(AgentLoop::new(AgentLoopConfig {
        llm: Arc::new(RealLlmClient),
        tool_executor: Arc::new(tool_executor),
    }));

    let request = RunTurnRequest {
        input: prompt,
        session_id,
        profile: profile.map(str::to_string),
        model,
        loaded_profile: Some(loaded_profile),
    };

    // D-A1: drive stream_turn instead of run_turn
    let mut stream = match lattice_api::stream_turn(session_api, agent_loop, request).await {
        Ok(s) => s,
        Err(lattice_api::ApiError::NotFound(id)) => {
            eprintln!("{}: session '{}' not found", "error".red(), id);
            return Ok(std::process::ExitCode::from(2));
        }
        Err(lattice_api::ApiError::Internal(msg)) => {
            eprintln!("{}: {}", "error".red(), msg);
            return Ok(std::process::ExitCode::from(1));
        }
    };

    // D-A6: drain the stream to completion; never break on a terminal event
    let mut state = RunRenderState::default();
    let stdout = std::io::stdout();
    let stderr = std::io::stderr();
    let mut out = stdout.lock();
    let mut err = stderr.lock();

    use futures::StreamExt;
    while let Some(event) = stream.next().await {
        if json {
            render_json_event(&event, &mut out)?;
        } else {
            render_human_event(&event, &mut state, &mut out, &mut err, verbose)?;
        }
        apply_event_to_state(&event, &mut state);
    }

    // D-B4: final stdout newline (human mode only; JSONL already terminates each line)
    if !json && state.tokens_emitted_without_newline {
        writeln!(out)?;
    }
    out.flush()?;

    // D-C3: verbose epilogue after stream drains
    if !json && verbose {
        render_verbose_epilogue(&state, &mut err)?;
    }

    Ok(exit_code_from_state(&state))
}

fn default_session_dir() -> PathBuf {
    dirs::config_dir()
        .or_else(|| dirs::home_dir().map(|h| h.join(".config")))
        .expect("cannot determine config directory: neither XDG_CONFIG_HOME, config_dir, nor HOME is set")
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
                "No default agent profile exists. Create .lattice/agents/default/agent.toml now? [y/N] "
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
                "Created {}. Edit it if needed, then re-run the command.",
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
    // Try canonicalize; if path doesn't exist yet, normalize manually to
    // prevent traversal attacks while still allowing non-existent directories.
    let canonical = match path.canonicalize() {
        Ok(p) => p,
        Err(_) => {
            use std::path::Component;
            let mut normalized = PathBuf::new();
            for component in path.components() {
                match component {
                    Component::ParentDir => {
                        if !normalized.pop() {
                            return Err(format!(
                                "Agents directory '{}' contains path traversal that escapes root.",
                                path.display()
                            ));
                        }
                    }
                    Component::CurDir => {}
                    other => normalized.push(other),
                }
            }
            normalized
        }
    };
    let path_str = canonical.to_string_lossy();

    let dangerous_prefixes = ["/etc", "/usr", "/var", "/sys", "/proc", "/dev", "/boot"];
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

/// Kept for the preserved `run_status_to_exit_code_*` tests.
#[allow(dead_code)]
fn status_to_exit_code(status: &RunTurnStatus) -> std::process::ExitCode {
    match status {
        RunTurnStatus::Completed => std::process::ExitCode::SUCCESS,
        RunTurnStatus::Error | RunTurnStatus::Cancelled => std::process::ExitCode::from(1),
    }
}

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
    fn run_status_to_exit_code_error_is_1() {
        let code = status_to_exit_code(&RunTurnStatus::Error);
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
        async fn execute(&self, _call: &lattice_llm::ToolCall) -> Result<String, String> {
            Ok("no-op".to_string())
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

    #[tokio::test]
    async fn tool_executor_uses_workspace_root_not_profile_package_dir() {
        // Regression: run_inner used to pass loaded_profile.package_dir
        // (.lattice/agents/default) as the tool executor's project_root,
        // which broke read_file/grep/etc for files outside that subdirectory.
        let workspace = tempfile::tempdir().unwrap();
        let ws = workspace.path();

        // Create workspace content: src/lib.rs at the workspace root
        std::fs::create_dir_all(ws.join("src")).unwrap();
        std::fs::write(ws.join("src/lib.rs"), "fn main() {}").unwrap();

        // Create profile directory that is a *subdirectory* of the workspace
        let profile_dir = ws.join(".lattice/agents/default");
        std::fs::create_dir_all(&profile_dir).unwrap();
        std::fs::write(profile_dir.join("agent.toml"), "[agent]\nname = \"test\"\n").unwrap();

        // Build tool executor with the *workspace root* (the fix), not profile_dir
        let enabled = vec!["read_file".to_string()];
        let executor = lattice_tools::executor::DefaultToolExecutor::for_project_root(
            &ws.to_string_lossy(),
            enabled,
        )
        .expect("executor creation must succeed");

        // read_file("src/lib.rs") must resolve against workspace root, not profile_dir
        let call = lattice_llm::ToolCall::new(
            "test-call",
            lattice_llm::FunctionCall::new("read_file", r#"{"path":"src/lib.rs"}"#),
        );
        use lattice_tools::ToolExecutor as _;
        let result = executor
            .execute(&call)
            .await
            .unwrap_or_else(|e| format!("Error: {e}"));

        assert!(
            result.contains("fn main()"),
            "read_file must succeed via workspace root; got: {result}"
        );
        assert!(
            !result.contains("outside the project root"),
            "must not be rejected as out-of-root"
        );
    }

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

    // ─── JSON renderer tests (Task 2) ──────────────────────────────

    #[test]
    fn render_json_event_emits_single_line_with_trailing_newline() {
        let event = TurnEvent::TokenDelta {
            content: "hi".into(),
        };
        let mut buf = Vec::new();
        render_json_event(&event, &mut buf).unwrap();
        let output = String::from_utf8(buf).unwrap();
        assert_eq!(output, "{\"type\":\"token_delta\",\"content\":\"hi\"}\n");
    }

    #[test]
    fn render_json_event_roundtrips_every_variant() {
        let events: Vec<TurnEvent> = vec![
            TurnEvent::TurnStarted {
                session_id: "s1".into(),
                parent_commit_id: None,
                branch: Some("main".into()),
            },
            TurnEvent::ModelStarted {
                provider: Some("anthropic".into()),
                model: "claude".into(),
            },
            TurnEvent::ReasoningDelta {
                content: "thinking".into(),
            },
            TurnEvent::TokenDelta {
                content: "hello".into(),
            },
            TurnEvent::ToolCallStarted {
                id: "tc1".into(),
                name: "read_file".into(),
            },
            TurnEvent::ToolCallFinished {
                id: "tc1".into(),
                name: "read_file".into(),
                ok: true,
                output_len: 412,
                error_summary: None,
            },
            TurnEvent::ModelFinished {
                finish_reason: "stop".into(),
                usage: Some(TurnUsageSummary {
                    prompt_tokens: 10,
                    completion_tokens: 5,
                    total_tokens: 15,
                }),
            },
            TurnEvent::TurnFailed {
                reason: "boom".into(),
            },
            TurnEvent::TurnCancelled {
                reason: Some("caller".into()),
            },
            TurnEvent::TurnCommitting,
            TurnEvent::TurnCommitted {
                session_id: "s1".into(),
                commit_id: "c1".into(),
                status: RunTurnStatus::Completed,
            },
            TurnEvent::CommitFailed {
                reason: "parent mismatch".into(),
            },
        ];

        for event in &events {
            let mut buf = Vec::new();
            render_json_event(event, &mut buf).unwrap();
            let line = String::from_utf8(buf).unwrap();
            let trimmed = line.trim_end();
            let parsed: TurnEvent = serde_json::from_str(trimmed).unwrap();
            assert_eq!(&parsed, event, "roundtrip failed for {:?}", event);
        }
    }

    #[test]
    fn render_json_event_emits_no_ansi_escapes() {
        let event = TurnEvent::TurnFailed {
            reason: "boom".into(),
        };
        let mut buf = Vec::new();
        render_json_event(&event, &mut buf).unwrap();
        assert!(
            !buf.contains(&0x1B),
            "JSON output must not contain ANSI ESC bytes (D-B6 / D-D3)"
        );
    }

    // ─── Human renderer tests (Task 3) ─────────────────────────────

    #[test]
    fn render_human_token_delta_writes_only_to_stdout() {
        let event = TurnEvent::TokenDelta {
            content: "hi".into(),
        };
        let mut state = RunRenderState::default();
        let mut out = Vec::new();
        let mut err = Vec::new();
        render_human_event(&event, &mut state, &mut out, &mut err, false).unwrap();
        assert_eq!(out, b"hi");
        assert!(err.is_empty());
    }

    #[test]
    fn render_human_status_line_after_partial_token_inserts_stderr_newline() {
        // First: emit a partial token (no trailing newline)
        let mut state = RunRenderState {
            tokens_emitted_without_newline: true,
            ..Default::default()
        };
        let mut out = Vec::new();
        let mut err = Vec::new();
        render_human_event(
            &TurnEvent::TokenDelta {
                content: "foo".into(),
            },
            &mut state,
            &mut out,
            &mut err,
            false,
        )
        .unwrap();
        apply_event_to_state(
            &TurnEvent::TokenDelta {
                content: "foo".into(),
            },
            &mut state,
        );

        // Then: a status line event (ToolCallStarted)
        let event = TurnEvent::ToolCallStarted {
            id: "t1".into(),
            name: "read_file".into(),
        };
        let mut err2 = Vec::new();
        render_human_event(&event, &mut state, &mut out, &mut err2, false).unwrap();

        // D-B3: err must start with \n (the mid-token newline trick)
        assert!(
            err2.starts_with(b"\n"),
            "stderr should begin with newline after mid-token status line (D-B3); got: {:?}",
            String::from_utf8_lossy(&err2)
        );
    }

    #[test]
    fn flush_before_status_resets_mid_token_flag_after_first_status() {
        // WR-01 regression: after the mid-token newline trick fires for the
        // first status event, subsequent status events must NOT produce
        // duplicate blank lines on stderr.
        let events = vec![
            TurnEvent::TokenDelta {
                content: "foo".into(),
            },
            TurnEvent::ToolCallStarted {
                id: "t1".into(),
                name: "read_file".into(),
            },
            TurnEvent::ToolCallFinished {
                id: "t1".into(),
                name: "read_file".into(),
                ok: true,
                output_len: 412,
                error_summary: None,
            },
            TurnEvent::TurnCommitting,
        ];
        let (out, err, _state) = fake_event_loop(events, false);
        // stdout: "foo\n" (token + D-B4 final newline)
        assert!(out.starts_with(b"foo\n"));
        // stderr: only ONE leading \n (from the first status event's D-B3 trick),
        // not three (from each of the three status events).
        let err_str = String::from_utf8(err).unwrap();
        let leading_newlines = err_str.chars().take_while(|c| *c == '\n').count();
        assert_eq!(
            leading_newlines, 1,
            "only one D-B3 newline trick, not duplicates; got: {:?}",
            err_str
        );
    }

    #[test]
    fn render_human_tool_finished_ok_renders_byte_size() {
        let event = TurnEvent::ToolCallFinished {
            id: "t1".into(),
            name: "read_file".into(),
            ok: true,
            output_len: 412,
            error_summary: None,
        };
        let mut state = RunRenderState::default();
        let mut out = Vec::new();
        let mut err = Vec::new();
        render_human_event(&event, &mut state, &mut out, &mut err, false).unwrap();
        let err_str = String::from_utf8(err).unwrap();
        assert!(err_str.contains("412B"));
        assert!(err_str.contains("read_file"));
    }

    #[test]
    fn render_human_tool_finished_err_renders_summary() {
        let event = TurnEvent::ToolCallFinished {
            id: "t1".into(),
            name: "read_file".into(),
            ok: false,
            output_len: 120,
            error_summary: Some("permission denied".into()),
        };
        let mut state = RunRenderState::default();
        let mut out = Vec::new();
        let mut err = Vec::new();
        render_human_event(&event, &mut state, &mut out, &mut err, false).unwrap();
        let err_str = String::from_utf8(err).unwrap();
        assert!(err_str.contains("120B"));
        assert!(err_str.contains("permission denied"));
    }

    #[test]
    fn render_human_reasoning_hidden_default() {
        let event = TurnEvent::ReasoningDelta {
            content: "thinking".into(),
        };
        let mut state = RunRenderState::default();
        let mut out = Vec::new();
        let mut err = Vec::new();
        render_human_event(&event, &mut state, &mut out, &mut err, false).unwrap();
        assert!(
            err.is_empty(),
            "reasoning should be hidden in non-verbose mode"
        );
    }

    #[test]
    fn render_human_reasoning_visible_verbose() {
        let event = TurnEvent::ReasoningDelta {
            content: "thinking".into(),
        };
        let mut state = RunRenderState::default();
        let mut out = Vec::new();
        let mut err = Vec::new();
        render_human_event(&event, &mut state, &mut out, &mut err, true).unwrap();
        let err_str = String::from_utf8(err).unwrap();
        assert!(err_str.contains("reasoning"));
        assert!(err_str.contains("thinking"));
    }

    #[test]
    fn render_human_turn_started_silent_default() {
        let event = TurnEvent::TurnStarted {
            session_id: "s1".into(),
            parent_commit_id: None,
            branch: None,
        };
        let mut state = RunRenderState::default();
        let mut out = Vec::new();
        let mut err = Vec::new();
        render_human_event(&event, &mut state, &mut out, &mut err, false).unwrap();
        assert!(
            err.is_empty(),
            "TurnStarted should be silent in non-verbose human mode (D-C2)"
        );
    }

    #[test]
    fn render_human_committed_lowercases_status() {
        let event = TurnEvent::TurnCommitted {
            session_id: "s1".into(),
            commit_id: "c1".into(),
            status: RunTurnStatus::Completed,
        };
        let mut state = RunRenderState::default();
        let mut out = Vec::new();
        let mut err = Vec::new();
        render_human_event(&event, &mut state, &mut out, &mut err, false).unwrap();
        let err_str = String::from_utf8(err).unwrap();
        assert!(
            err_str.contains("completed"),
            "status must be lowercase in human mode; got: {err_str}"
        );
        assert!(
            !err_str.contains("Completed"),
            "status must NOT contain uppercase Completed"
        );
    }

    // ─── Verbose epilogue tests (Task 4) ────────────────────────────

    #[test]
    fn verbose_epilogue_emits_all_fields_when_populated() {
        let state = RunRenderState {
            session_id: Some("s1".into()),
            commit_id: Some("c1".into()),
            last_status: Some(RunTurnStatus::Completed),
            usage_total: Some(TurnUsageSummary {
                prompt_tokens: 20,
                completion_tokens: 10,
                total_tokens: 30,
            }),
            tool_finished_count: 3,
            ..Default::default()
        };
        let mut err = Vec::new();
        render_verbose_epilogue(&state, &mut err).unwrap();
        let err_str = String::from_utf8(err).unwrap();
        assert!(err_str.contains("session"));
        assert!(err_str.contains("s1"));
        assert!(err_str.contains("commit"));
        assert!(err_str.contains("c1"));
        assert!(err_str.contains("status"));
        assert!(err_str.contains("usage"));
        assert!(err_str.contains("30 tok"));
        assert!(err_str.contains("tools"));
        assert!(err_str.contains("3 executed"));
    }

    #[test]
    fn verbose_epilogue_skips_unset_fields() {
        let state = RunRenderState::default();
        let mut err = Vec::new();
        render_verbose_epilogue(&state, &mut err).unwrap();
        assert!(
            err.is_empty(),
            "empty state should produce no epilogue output"
        );
    }

    #[test]
    fn verbose_epilogue_usage_from_state_not_committed() {
        // Simulate two ModelFinished events summed into usage_total
        let state = RunRenderState {
            usage_total: Some(TurnUsageSummary {
                prompt_tokens: 20,
                completion_tokens: 10,
                total_tokens: 30,
            }),
            ..Default::default()
        };
        let mut err = Vec::new();
        render_verbose_epilogue(&state, &mut err).unwrap();
        let err_str = String::from_utf8(err).unwrap();
        assert!(err_str.contains("30 tok (prompt: 20, completion: 10)"));
    }

    // ─── D-A4 exit-code matrix (Task 6) ────────────────────────────

    #[test]
    fn exit_code_from_state_covers_all_da4_rows() {
        use std::process::ExitCode;
        fn ec_to_dbg(c: ExitCode) -> String {
            format!("{:?}", c)
        }

        // Row 1: TurnCommitted{Completed} -> 0
        let mut s = RunRenderState {
            last_status: Some(RunTurnStatus::Completed),
            last_commit: Some(CommitSignal::Committed),
            ..Default::default()
        };
        assert_eq!(
            ec_to_dbg(exit_code_from_state(&s)),
            ec_to_dbg(ExitCode::SUCCESS)
        );

        // Row 2: TurnCommitted{Error} -> 1
        s.last_status = Some(RunTurnStatus::Error);
        assert_ne!(
            ec_to_dbg(exit_code_from_state(&s)),
            ec_to_dbg(ExitCode::SUCCESS)
        );

        // Row 3: TurnCommitted{Cancelled} -> 1
        s.last_status = Some(RunTurnStatus::Cancelled);
        assert_ne!(
            ec_to_dbg(exit_code_from_state(&s)),
            ec_to_dbg(ExitCode::SUCCESS)
        );

        // Row 4: CommitFailed -> 1 (regardless of last_status)
        s.last_commit = Some(CommitSignal::Failed {
            reason: "parent mismatch".into(),
        });
        assert_ne!(
            ec_to_dbg(exit_code_from_state(&s)),
            ec_to_dbg(ExitCode::SUCCESS)
        );

        // Row 5: TurnFailed without TurnCommitted -> 1
        let s5 = RunRenderState {
            last_terminal: Some(TerminalSignal::Failed {
                reason: "boom".into(),
            }),
            ..Default::default()
        };
        assert_ne!(
            ec_to_dbg(exit_code_from_state(&s5)),
            ec_to_dbg(ExitCode::SUCCESS)
        );

        // Row 6: TurnCancelled without TurnCommitted -> 1
        let s6 = RunRenderState {
            last_terminal: Some(TerminalSignal::Cancelled { reason: None }),
            ..Default::default()
        };
        assert_ne!(
            ec_to_dbg(exit_code_from_state(&s6)),
            ec_to_dbg(ExitCode::SUCCESS)
        );

        // Row 7: stream ended without any terminal/commit -> 1 (protocol violation)
        let s7 = RunRenderState::default();
        assert_ne!(
            ec_to_dbg(exit_code_from_state(&s7)),
            ec_to_dbg(ExitCode::SUCCESS)
        );
    }

    // ─── Sequence test helpers (Plan 02, Task 1) ──────────────────

    /// Drive a Vec<TurnEvent> through the human renderer with Vec<u8>
    /// writers, applying D-B4 final-newline policy, and return the
    /// captured stdout bytes, stderr bytes, and accumulated state.
    fn fake_event_loop(
        events: Vec<TurnEvent>,
        verbose: bool,
    ) -> (Vec<u8>, Vec<u8>, RunRenderState) {
        let mut state = RunRenderState::default();
        let mut out: Vec<u8> = Vec::new();
        let mut err: Vec<u8> = Vec::new();
        for event in &events {
            render_human_event(event, &mut state, &mut out, &mut err, verbose)
                .expect("render_human_event must succeed against Vec<u8> writers");
            apply_event_to_state(event, &mut state);
        }
        // Mirror run_inner D-B4 final-stdout-newline policy.
        if state.tokens_emitted_without_newline {
            use std::io::Write;
            writeln!(&mut out).expect("final newline must succeed against Vec<u8>");
        }
        (out, err, state)
    }

    /// Drive a Vec<TurnEvent> through the JSONL renderer with a
    /// Vec<u8> writer and return the captured stdout bytes and
    /// accumulated state.
    fn fake_jsonl_loop(events: Vec<TurnEvent>) -> (Vec<u8>, RunRenderState) {
        let mut state = RunRenderState::default();
        let mut out: Vec<u8> = Vec::new();
        for event in &events {
            render_json_event(event, &mut out)
                .expect("render_json_event must succeed against Vec<u8>");
            apply_event_to_state(event, &mut state);
        }
        (out, state)
    }

    /// Convert raw test buffer bytes to a String (asserts valid UTF-8).
    fn s(bytes: &[u8]) -> String {
        String::from_utf8(bytes.to_vec()).expect("test buffers must be valid UTF-8")
    }

    // ─── Happy-path + interleave sequence tests (Plan 02, Task 2) ──

    #[test]
    fn seq_happy_path_human() {
        let events = vec![
            TurnEvent::TurnStarted {
                session_id: "S1".into(),
                parent_commit_id: None,
                branch: Some("main".into()),
            },
            TurnEvent::ModelStarted {
                provider: Some("anthropic".into()),
                model: "claude-3".into(),
            },
            TurnEvent::TokenDelta {
                content: "Hello".into(),
            },
            TurnEvent::TokenDelta {
                content: " world\n".into(),
            },
            TurnEvent::ModelFinished {
                finish_reason: "stop".into(),
                usage: Some(TurnUsageSummary {
                    prompt_tokens: 10,
                    completion_tokens: 5,
                    total_tokens: 15,
                }),
            },
            TurnEvent::TurnCommitting,
            TurnEvent::TurnCommitted {
                session_id: "S1".into(),
                commit_id: "C1".into(),
                status: RunTurnStatus::Completed,
            },
        ];
        let (out, err, state) = fake_event_loop(events, false);
        // stdout = TokenDelta concatenation; " world\n" ends with \n,
        // so D-B4 final-newline logic does NOT append an extra \n.
        assert_eq!(s(&out), "Hello world\n");
        // stderr contains committing, commit id, completed status
        let err_str = s(&err);
        assert!(err_str.contains("committing"));
        assert!(err_str.contains("C1"));
        assert!(err_str.contains("completed"));
        // D-C2: non-verbose hides reasoning and model-finished
        assert!(!err_str.contains("reasoning"));
        assert!(!err_str.contains("model finished"));
        // Exit code = SUCCESS
        assert_eq!(
            format!("{:?}", exit_code_from_state(&state)),
            format!("{:?}", std::process::ExitCode::SUCCESS)
        );
    }

    #[test]
    fn seq_happy_path_verbose_with_epilogue() {
        let events = vec![
            TurnEvent::TurnStarted {
                session_id: "S1".into(),
                parent_commit_id: None,
                branch: Some("main".into()),
            },
            TurnEvent::ModelStarted {
                provider: Some("anthropic".into()),
                model: "claude-3".into(),
            },
            TurnEvent::TokenDelta {
                content: "Hello".into(),
            },
            TurnEvent::TokenDelta {
                content: " world\n".into(),
            },
            TurnEvent::ModelFinished {
                finish_reason: "stop".into(),
                usage: Some(TurnUsageSummary {
                    prompt_tokens: 10,
                    completion_tokens: 5,
                    total_tokens: 15,
                }),
            },
            TurnEvent::TurnCommitting,
            TurnEvent::TurnCommitted {
                session_id: "S1".into(),
                commit_id: "C1".into(),
                status: RunTurnStatus::Completed,
            },
        ];
        let (out, mut err, state) = fake_event_loop(events, true);
        // stdout unchanged
        assert_eq!(s(&out), "Hello world\n");
        // Append verbose epilogue to the same err buffer
        render_verbose_epilogue(&state, &mut err).unwrap();
        let err_str = s(&err);
        // Verbose stderr contains session, commit, status, usage labels
        assert!(err_str.contains("session"));
        assert!(err_str.contains("commit"));
        assert!(err_str.contains("status"));
        assert!(err_str.contains("usage"));
        assert!(err_str.contains("15 tok"));
        assert!(err_str.contains("prompt: 10"));
        assert!(err_str.contains("completion: 5"));
    }

    #[test]
    fn seq_trailing_token_newline_emitted() {
        let events = vec![
            TurnEvent::TokenDelta {
                content: "foo".into(),
            },
            TurnEvent::ModelFinished {
                finish_reason: "stop".into(),
                usage: None,
            },
            TurnEvent::TurnCommitting,
            TurnEvent::TurnCommitted {
                session_id: "S".into(),
                commit_id: "C".into(),
                status: RunTurnStatus::Completed,
            },
        ];
        let (out, _err, _state) = fake_event_loop(events, false);
        // D-B4: "foo" does not end with \n, so fake_event_loop appends one
        assert_eq!(s(&out), "foo\n");
    }

    #[test]
    fn seq_token_then_status_inserts_stderr_newline() {
        let events = vec![
            TurnEvent::TokenDelta {
                content: "foo".into(),
            },
            TurnEvent::ToolCallStarted {
                id: "t1".into(),
                name: "read_file".into(),
            },
        ];
        let (out, err, _state) = fake_event_loop(events, false);
        // D-B4: "foo" has no trailing newline, so helper appends one to stdout
        assert_eq!(&out[..], b"foo\n");
        // D-B3: stderr starts with \n (mid-token newline trick)
        let err_str = s(&err);
        assert!(
            err_str.starts_with('\n'),
            "stderr should begin with newline after mid-token status line (D-B3); got: {err_str:?}"
        );
        // After the leading \n, stderr contains tool name
        assert!(err_str.contains("tool"));
        assert!(err_str.contains("read_file"));
    }

    #[test]
    fn seq_multiround_usage_sums_into_state() {
        let events = vec![
            TurnEvent::ModelFinished {
                finish_reason: "stop".into(),
                usage: Some(TurnUsageSummary {
                    prompt_tokens: 10,
                    completion_tokens: 5,
                    total_tokens: 15,
                }),
            },
            TurnEvent::ModelFinished {
                finish_reason: "stop".into(),
                usage: Some(TurnUsageSummary {
                    prompt_tokens: 10,
                    completion_tokens: 5,
                    total_tokens: 15,
                }),
            },
        ];
        let (_out, err, state) = fake_event_loop(events, true);
        // D-A3: usage_total is summed across two ModelFinished events
        assert_eq!(
            state.usage_total,
            Some(TurnUsageSummary {
                prompt_tokens: 20,
                completion_tokens: 10,
                total_tokens: 30,
            })
        );
        // D-C3: verbose epilogue uses state.usage_total, NOT TurnCommitted
        let mut epilogue_err = err;
        render_verbose_epilogue(&state, &mut epilogue_err).unwrap();
        let epilogue_str = s(&epilogue_err);
        assert!(epilogue_str.contains("30 tok"));
        assert!(epilogue_str.contains("prompt: 20"));
        assert!(epilogue_str.contains("completion: 10"));
    }

    // ─── Failure / cancellation / protocol-violation sequence tests (Plan 02, Task 3) ──

    #[test]
    fn seq_tool_failure_renders_summary() {
        let events = vec![
            TurnEvent::TurnStarted {
                session_id: "S".into(),
                parent_commit_id: None,
                branch: None,
            },
            TurnEvent::ToolCallStarted {
                id: "t".into(),
                name: "read_file".into(),
            },
            TurnEvent::ToolCallFinished {
                id: "t".into(),
                name: "read_file".into(),
                ok: false,
                output_len: 120,
                error_summary: Some("permission denied".into()),
            },
            TurnEvent::TurnCommitting,
            TurnEvent::TurnCommitted {
                session_id: "S".into(),
                commit_id: "C".into(),
                status: RunTurnStatus::Completed,
            },
        ];
        let (_out, err, state) = fake_event_loop(events, false);
        let err_str = s(&err);
        // Tool failure renders byte count and error summary with failure glyph
        assert!(err_str.contains("120B"));
        assert!(err_str.contains("permission denied"));
        assert!(err_str.contains("✗"));
        // D-A4: tool failure inside a successful turn does NOT change exit code
        // — terminal status comes from TurnCommitted per WR-01
        assert_eq!(
            format!("{:?}", exit_code_from_state(&state)),
            format!("{:?}", std::process::ExitCode::SUCCESS)
        );
    }

    #[test]
    fn seq_turn_failed_then_commit_drains_stream() {
        let events = vec![
            TurnEvent::TurnStarted {
                session_id: "S".into(),
                parent_commit_id: None,
                branch: None,
            },
            TurnEvent::ModelStarted {
                provider: None,
                model: "x".into(),
            },
            TurnEvent::TurnFailed {
                reason: "provider error".into(),
            },
            TurnEvent::TurnCommitting,
            TurnEvent::TurnCommitted {
                session_id: "S".into(),
                commit_id: "C".into(),
                status: RunTurnStatus::Error,
            },
        ];
        let (_out, err, state) = fake_event_loop(events, false);
        let err_str = s(&err);
        // D-A6 stream-drain proof: stderr contains the TurnFailed message
        assert!(err_str.contains("failed"));
        assert!(err_str.contains("provider error"));
        // And ALSO the post-terminal commit events (D-A6: must drain past terminal)
        assert!(err_str.contains("committing"));
        assert!(err_str.contains("C"));
        assert!(err_str.contains("error")); // D-B2: lowercase status
                                            // State accumulated correctly
        assert!(state.last_terminal.is_some());
        assert_eq!(state.last_status, Some(RunTurnStatus::Error));
        assert!(state.last_commit.is_some());
        // D-A4 row 2: TurnCommitted{Error} -> exit 1
        assert_ne!(
            format!("{:?}", exit_code_from_state(&state)),
            format!("{:?}", std::process::ExitCode::SUCCESS)
        );
    }

    #[test]
    fn seq_commit_failed_renders_reason() {
        let events = vec![
            TurnEvent::TurnFailed {
                reason: "boom".into(),
            },
            TurnEvent::TurnCommitting,
            TurnEvent::CommitFailed {
                reason: "parent mismatch".into(),
            },
        ];
        let (_out, err, state) = fake_event_loop(events, false);
        let err_str = s(&err);
        // CommitFailed renders the failure glyph and reason
        assert!(err_str.contains("commit failed"));
        assert!(err_str.contains("parent mismatch"));
        // D-A4 row 4: CommitFailed -> exit 1
        assert_ne!(
            format!("{:?}", exit_code_from_state(&state)),
            format!("{:?}", std::process::ExitCode::SUCCESS)
        );
    }

    #[test]
    fn seq_stream_ends_without_terminal_is_protocol_violation() {
        let events = vec![
            TurnEvent::TurnStarted {
                session_id: "S".into(),
                parent_commit_id: None,
                branch: None,
            },
            TurnEvent::ModelStarted {
                provider: None,
                model: "x".into(),
            },
        ];
        let (_out, _err, state) = fake_event_loop(events, false);
        // No terminal event arrived
        assert!(state.last_terminal.is_none());
        assert!(state.last_commit.is_none());
        // D-A4 row 7: stream ended without terminal or commit -> exit 1
        assert_ne!(
            format!("{:?}", exit_code_from_state(&state)),
            format!("{:?}", std::process::ExitCode::SUCCESS)
        );
    }

    #[test]
    fn seq_jsonl_round_trips_every_variant() {
        // Build a Vec<TurnEvent> covering all 12 variants with representative field values
        let events: Vec<TurnEvent> = vec![
            TurnEvent::TurnStarted {
                session_id: "s1".into(),
                parent_commit_id: None,
                branch: Some("main".into()),
            },
            TurnEvent::ModelStarted {
                provider: Some("anthropic".into()),
                model: "claude-3".into(),
            },
            TurnEvent::ReasoningDelta {
                content: "thinking deeply".into(),
            },
            TurnEvent::TokenDelta {
                content: "hello world".into(),
            },
            TurnEvent::ToolCallStarted {
                id: "tc1".into(),
                name: "read_file".into(),
            },
            TurnEvent::ToolCallFinished {
                id: "tc1".into(),
                name: "read_file".into(),
                ok: true,
                output_len: 412,
                error_summary: None,
            },
            TurnEvent::ModelFinished {
                finish_reason: "stop".into(),
                usage: Some(TurnUsageSummary {
                    prompt_tokens: 10,
                    completion_tokens: 5,
                    total_tokens: 15,
                }),
            },
            TurnEvent::TurnFailed {
                reason: "provider error".into(),
            },
            TurnEvent::TurnCancelled {
                reason: Some("caller cancelled".into()),
            },
            TurnEvent::TurnCommitting,
            TurnEvent::TurnCommitted {
                session_id: "s1".into(),
                commit_id: "c1".into(),
                status: RunTurnStatus::Completed,
            },
            TurnEvent::CommitFailed {
                reason: "parent mismatch".into(),
            },
        ];
        assert_eq!(events.len(), 12, "must cover all 12 TurnEvent variants");

        let (out, _state) = fake_jsonl_loop(events.clone());

        // Split stdout buffer by \n; exclude trailing empty element after final newline
        let out_str = s(&out);
        let lines: Vec<&str> = out_str.split('\n').filter(|l| !l.is_empty()).collect();
        assert_eq!(lines.len(), 12, "JSONL must produce exactly 12 lines");

        // Each line round-trips through serde_json::from_str back to the original event
        for (i, line) in lines.iter().enumerate() {
            let parsed: TurnEvent = serde_json::from_str(line).unwrap();
            assert_eq!(
                parsed, events[i],
                "JSONL roundtrip failed for variant at index {i}: parsed={:?}, expected={:?}",
                parsed, events[i]
            );
        }

        // D-D3 / D-B6: stdout contains zero ESC (0x1B) bytes
        assert!(
            !out.contains(&0x1B),
            "JSONL output must not contain ANSI ESC bytes (D-D3 / D-B6)"
        );

        // No human chrome substrings in JSONL output
        assert!(
            !out_str.contains("·") && !out_str.contains("✓") && !out_str.contains("✗"),
            "JSONL must not contain human chrome glyphs (D-D3)"
        );
        assert!(
            !out_str.contains("reasoning:"),
            "JSONL must not contain human-format 'reasoning:' prefix"
        );

        // D-D3 (clarification): JSONL IS the raw public TurnEvent stream,
        // so ReasoningDelta IS emitted as a normal JSONL line even though
        // it's hidden from human stderr
        assert!(
            out_str.contains("\"type\":\"reasoning_delta\""),
            "JSONL must contain reasoning_delta as a normal JSONL line (D-D3 raw-stream contract)"
        );
    }

    #[test]
    fn seq_reasoning_hidden_default_in_human() {
        let events = vec![
            TurnEvent::TokenDelta {
                content: "a".into(),
            },
            TurnEvent::ReasoningDelta {
                content: "thinking".into(),
            },
            TurnEvent::TokenDelta {
                content: "b".into(),
            },
            TurnEvent::TurnCommitted {
                session_id: "S".into(),
                commit_id: "C".into(),
                status: RunTurnStatus::Completed,
            },
        ];
        let (out, err, _state) = fake_event_loop(events, false);
        // stdout contains TokenDelta bytes only
        let out_str = s(&out);
        assert!(
            out_str.starts_with("ab"),
            "stdout should start with 'ab' (TokenDelta concatenation); got: {out_str:?}"
        );
        // D-C2: stderr does not contain reasoning or thinking text
        let err_str = s(&err);
        assert!(
            !err_str.contains("reasoning"),
            "stderr must not contain 'reasoning' in non-verbose mode (D-C2)"
        );
        assert!(
            !err_str.contains("thinking"),
            "stderr must not contain 'thinking' in non-verbose mode (D-C2)"
        );
    }

    #[test]
    fn seq_reasoning_visible_in_verbose() {
        let events = vec![
            TurnEvent::TokenDelta {
                content: "a".into(),
            },
            TurnEvent::ReasoningDelta {
                content: "thinking".into(),
            },
            TurnEvent::TokenDelta {
                content: "b".into(),
            },
            TurnEvent::TurnCommitted {
                session_id: "S".into(),
                commit_id: "C".into(),
                status: RunTurnStatus::Completed,
            },
        ];
        let (out, err, _state) = fake_event_loop(events, true);
        // stdout contains TokenDelta bytes only (unchanged from non-verbose)
        let out_str = s(&out);
        assert!(
            out_str.starts_with("ab"),
            "stdout should start with 'ab'; got: {out_str:?}"
        );
        // D-C3: verbose stderr contains reasoning text
        let err_str = s(&err);
        assert!(
            err_str.contains("thinking"),
            "verbose stderr must contain 'thinking' (D-C3)"
        );
        assert!(
            err_str.contains("reasoning"),
            "verbose stderr must contain 'reasoning' (D-C3)"
        );
    }
}
