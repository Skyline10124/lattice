// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Session turn execution renderer built on `lattice_api::stream_turn`.
//!
//! Consumes a live `TurnEventStream` and renders events incrementally:
//! `TokenDelta` content goes to stdout, all status/lifecycle/error lines
//! go to stderr in human mode, and `--json` emits one `TurnEvent` JSON
//! object per line on stdout (JSONL).
//!
//! # JSONL mode (`--json`)
//!
//! `--json` emits one `TurnEvent` JSON object per line on stdout,
//! terminated by `\n`. Human status lines and verbose epilogue are suppressed
//! in JSON mode. JSONL emits public `TurnEvent` values as received, including
//! `ReasoningDelta` when present.
//!
//! # Exit codes
//!
//! - 0: Turn committed successfully (`RunTurnStatus::Completed`)
//! - 1:  Provider/API/tool failure, commit failure, terminal without commit, or protocol violation
//! - 2: Input/configuration error (missing agent profile, missing credentials, missing session)

use anyhow::Result;
use colored::Colorize;
use std::io::Write;
use std::path::{Path, PathBuf};

use std::time::Duration;
use tokio::time;

use lattice_api::{LoadedAgentProfile, RunTurnRequest, RunTurnStatus, TurnEvent, TurnUsageSummary};

/// Stream idle timeout in seconds.
pub(crate) const STREAM_TIMEOUT_SECS: u64 = 300;
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
/// the CLI exit code after the stream drains.
///
/// `usage_total` is summed across multiple `ModelFinished` events;
/// `TurnCommitted` is NOT a usage source.
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

/// Update `RunRenderState` from each `TurnEvent` variant.
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
            ..
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

/// Derive exit code from accumulated stream signals (rows 1-7).
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

/// Render a `TurnEvent` as one JSONL line on `out`.
///
/// Each call emits exactly one `serde_json::to_string` line followed
/// by `\n` via `writeln!`. No ANSI escapes are produced.
fn render_json_event<W: std::io::Write>(event: &TurnEvent, out: &mut W) -> std::io::Result<()> {
    let line = serde_json::to_string(event).map_err(std::io::Error::other)?;
    writeln!(out, "{}", line)
}

/// Render a `TurnEvent` for human consumption.
///
/// `TokenDelta` content goes to `out`; all status/lifecycle lines go to `err`.
/// Before writing any status line to `err`, if `state.tokens_emitted_without_newline`
/// is true, the mid-token newline trick is applied: flush `out`, emit `\n` on `err`.
/// This function does not mutate `state` — `apply_event_to_state` owns that.
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
        TurnEvent::LlmRequestError {
            attempt,
            max_attempts,
            error_kind,
            will_retry,
            delay_ms,
            message,
            ..
        } => {
            if verbose {
                flush_before_status(out, err, state)?;
                if *will_retry {
                    let delay = delay_ms.unwrap_or(0);
                    writeln!(
                        err,
                        "{} llm retry {attempt}/{max_attempts} after {delay}ms ({error_kind}): {message}",
                        "·".dimmed()
                    )?;
                } else {
                    writeln!(
                        err,
                        "{} llm failed {attempt}/{max_attempts} ({error_kind}): {message}",
                        "✗".red()
                    )?;
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
                writeln!(err, "{} tool {name} {output_len} chars", "✓".green())?;
            } else {
                let summary = error_summary.as_deref().unwrap_or("error");
                writeln!(
                    err,
                    "{} tool {name} {output_len} chars: {summary}",
                    "✗".red()
                )?;
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
        TurnEvent::CepCallStarted { method, .. } => {
            if verbose {
                flush_before_status(out, err, state)?;
                writeln!(err, "{} cep {method}", "·".dimmed())?;
            }
        }
        TurnEvent::CepCallFinished { method, ok, .. } => {
            if verbose {
                flush_before_status(out, err, state)?;
                if *ok {
                    writeln!(err, "{} cep {method}", "✓".green())?;
                } else {
                    writeln!(err, "{} cep {method}", "✗".red())?;
                }
            }
        }
    }
    Ok(())
}

/// Apply mid-token newline trick: if stdout has emitted tokens
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

/// Render the verbose epilogue after the stream drains.
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

/// Execute one prompt in an existing session with a resolved active agent.
pub async fn run_session_prompt(
    prompt: String,
    session_id: String,
    loaded_profile: LoadedAgentProfile,
    verbose: bool,
    json: bool,
) -> std::process::ExitCode {
    match run_inner(prompt, session_id, loaded_profile, verbose, json).await {
        Ok(exit_code) => exit_code,
        Err(error) => {
            // `{:#}` prints the full anyhow context chain (outer: inner: root cause).
            eprintln!("{}: {:#}", "error".red(), error);
            std::process::ExitCode::from(2)
        }
    }
}

async fn run_inner(
    prompt: String,
    session_id: String,
    loaded_profile: LoadedAgentProfile,
    verbose: bool,
    json: bool,
) -> Result<std::process::ExitCode> {
    if verbose {
        eprintln!(
            "{}",
            format!("profile: {}", loaded_profile.path.display()).dimmed()
        );
    }

    // Build SessionApi backed by Git session graph.
    let session_dir = crate::commands::session::default_session_dir();
    let git_repo_path = session_dir.join("sessions.git");
    let session_api = lattice_api::SessionApi::git(git_repo_path);

    // Cooperative cancellation: install a scoped SIGINT listener that flips
    // a shared flag. The agent loop races stalled stream.next() against it
    // (#24 slice 5); the harness SIGKILL path never counts as coverage.
    let cancel_flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let sigint_flag = cancel_flag.clone();
    let sigint_guard = tokio::spawn(async move {
        if let Ok(mut sigint) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())
        {
            // Each SIGINT flips the flag; the listener stays active until
            // aborted below so it cannot affect later commands.
            while sigint.recv().await.is_some() {
                sigint_flag.store(true, std::sync::atomic::Ordering::Relaxed);
            }
        }
    });

    let request = RunTurnRequest {
        input: prompt,
        session_id,
        loaded_profile: Some(loaded_profile),
        cancel_flag: Some(cancel_flag),
        test_llm: None,
        test_project_root: None,
        test_persist_audit: None,
    };

    // drive stream_turn; lattice-api owns Cep runtime construction
    let mut stream = match lattice_api::stream_turn(session_api, request).await {
        Ok(s) => s,
        Err(lattice_api::ApiError::NotFound(id)) => {
            eprintln!("{}: session '{}' not found", "error".red(), id);
            return Ok(std::process::ExitCode::from(2));
        }
        Err(lattice_api::ApiError::BadRequest(msg)) => {
            eprintln!("{}: {}", "error".red(), msg);
            return Ok(std::process::ExitCode::from(2));
        }
        Err(lattice_api::ApiError::Conflict(msg)) => {
            eprintln!("{}: {}", "error".red(), msg);
            return Ok(std::process::ExitCode::from(1));
        }
        Err(lattice_api::ApiError::Internal(msg)) => {
            eprintln!("{}: {}", "error".red(), msg);
            return Ok(std::process::ExitCode::from(1));
        }
    };

    // drain the stream to completion; never break on a terminal event
    let mut state = RunRenderState::default();
    let stdout = std::io::stdout();
    let stderr = std::io::stderr();
    let mut out = stdout.lock();
    let mut err = stderr.lock();

    use futures::StreamExt;

    loop {
        let event =
            match time::timeout(Duration::from_secs(STREAM_TIMEOUT_SECS), stream.next()).await {
                Ok(Some(event)) => event,
                Ok(None) => break,
                Err(_) => {
                    out.flush()?;
                    writeln!(
                        err,
                        "{}: stream timed out after {}s",
                        "error".red(),
                        STREAM_TIMEOUT_SECS
                    )?;
                    err.flush()?;
                    return Ok(std::process::ExitCode::from(1));
                }
            };

        if json {
            render_json_event(&event, &mut out)?;
        } else {
            render_human_event(&event, &mut state, &mut out, &mut err, verbose)?;
        }

        apply_event_to_state(&event, &mut state);
    }

    // Abort the scoped SIGINT listener once the stream has drained so it
    // cannot observe a later command's SIGINT.
    sigint_guard.abort();

    // final stdout newline (human mode only; JSONL already terminates each line)
    if !json && state.tokens_emitted_without_newline {
        writeln!(out)?;
    }
    out.flush()?;

    // verbose epilogue after stream drains
    if !json && verbose {
        render_verbose_epilogue(&state, &mut err)?;
    }

    Ok(exit_code_from_state(&state))
}

pub(crate) fn load_named_profile(
    agent_name: &str,
    project_root: &Path,
) -> std::result::Result<LoadedAgentProfile, anyhow::Error> {
    let global_agents_dir = safe_agents_dir(None).map_err(|e| anyhow::anyhow!("{}", e))?;
    lattice_api::resolve_named_agent_profile(agent_name, project_root, &global_agents_dir)
        .map_err(Into::into)
}

pub(crate) fn safe_agents_dir(override_path: Option<&str>) -> Result<PathBuf, String> {
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

pub(crate) fn validate_agents_dir(path: &Path) -> Result<(), String> {
    let canonical = canonicalize_agents_dir_prefix(path)?;

    let dangerous_prefixes = [
        Path::new("/etc"),
        Path::new("/usr"),
        Path::new("/var"),
        Path::new("/sys"),
        Path::new("/proc"),
        Path::new("/dev"),
        Path::new("/boot"),
    ];
    for prefix in &dangerous_prefixes {
        if canonical.starts_with(prefix) {
            return Err(format!(
                "Agents directory '{}' is inside a system path ({}). \
                 Set LATTICE_AGENTS_DIR to a safe location (e.g., under $HOME or your project).",
                path.display(),
                prefix.display()
            ));
        }
    }

    let tmp_root = Path::new("/tmp")
        .canonicalize()
        .unwrap_or_else(|_| PathBuf::from("/tmp"));
    if canonical.starts_with(&tmp_root) {
        return Err(format!(
            "Agents directory '{}' is inside /tmp which is world-writable and unsafe. \
             Set LATTICE_AGENTS_DIR to a safe location.",
            path.display()
        ));
    }

    Ok(())
}

pub(crate) fn canonicalize_agents_dir_prefix(path: &Path) -> Result<PathBuf, String> {
    if path.as_os_str().is_empty() {
        return Ok(PathBuf::new());
    }
    if path
        .components()
        .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err(format!(
            "Agents directory '{}' contains path traversal.",
            path.display()
        ));
    }

    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|error| format!("cannot resolve current directory: {error}"))?
            .join(path)
    };

    let mut cursor = absolute.as_path();
    let mut missing = Vec::new();
    loop {
        match cursor.canonicalize() {
            Ok(mut canonical) => {
                for component in missing.iter().rev() {
                    canonical.push(component);
                }
                return Ok(canonical);
            }
            Err(error) => {
                let Some(file_name) = cursor.file_name() else {
                    return Err(format!(
                        "cannot resolve agents directory '{}': {}",
                        path.display(),
                        error
                    ));
                };
                missing.push(file_name.to_os_string());
                let Some(parent) = cursor.parent() else {
                    return Err(format!(
                        "cannot resolve agents directory '{}': {}",
                        path.display(),
                        error
                    ));
                };
                cursor = parent;
            }
        }
    }
}

#[cfg(test)]
#[path = "run_tests.rs"]
mod tests;
