// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Inline CLI REPL: `> ` prompt, line editing via rustyline, streaming tokens
//! to stdout, tool/status lines to stderr. No alternate screen; rustyline
//! enters raw mode only for the duration of each readline and restores it
//! afterward, so the session stays inline.

use anyhow::{Context, Result};
use colored::Colorize;
use futures::StreamExt;
use lattice_api::{
    CreateSessionRequest, OpenSessionRequest, RunTurnRequest, SessionApi, TurnEvent,
};
use rustyline::DefaultEditor;
use rustyline::error::ReadlineError;
use std::io::{Write, stdout};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Clone, Debug)]
pub struct TuiOptions {
    pub session_id: Option<String>,
    pub agent: String,
    pub project_branch: Option<String>,
    pub session_dir: PathBuf,
    pub agents_dir: PathBuf,
    pub project_root: PathBuf,
}

fn required_project_branch(project_branch: Option<&str>) -> Result<&str> {
    match project_branch {
        Some(branch) if !branch.trim().is_empty() => Ok(branch),
        _ => Err(anyhow::anyhow!(
            "--project-branch is required when creating a new TUI session"
        )),
    }
}

pub async fn run(options: TuiOptions) -> Result<std::process::ExitCode> {
    let git_repo_path = options.session_dir.join("sessions.git");
    if let Err(msg) = lattice_api::check_migration_guard(&git_repo_path) {
        eprintln!("{msg}");
        std::process::exit(1);
    }
    let session_api = SessionApi::git(git_repo_path.clone());
    let session = match &options.session_id {
        Some(session_id) => session_api
            .open_session(OpenSessionRequest {
                id: session_id.clone(),
            })
            .with_context(|| format!("open session {session_id}"))?,
        None => {
            let project_branch =
                required_project_branch(options.project_branch.as_deref())?.to_string();
            session_api
                .create_session(CreateSessionRequest {
                    label: None,
                    active_agent: options.agent.clone(),
                    project_branch,
                })
                .context("create chat session")?
        }
    };

    let session_id = session.session_id;
    let active_agent = session.active_agent_name;
    let project_branch = session.project_target_branch.as_deref().unwrap_or("main");

    // ---------- rustyline editor + persisted history ----------
    let history_path = options.session_dir.join("repl_history.txt");
    let mut rl: DefaultEditor = DefaultEditor::with_config(rustyline::Config::builder().build())?;
    let _ = rl.load_history(&history_path);

    print_banner(&session_id, &active_agent, project_branch);

    // ---------- REPL ----------
    let prompt = format!("{} ", ">".cyan());
    loop {
        let line = rl.readline(&prompt);
        match line {
            Ok(raw) => {
                let input = raw.trim().to_string();
                if input.is_empty() {
                    continue;
                }
                // Persist every non-empty input (best effort).
                let _ = rl.add_history_entry(&input);
                let _ = rl.save_history(&history_path);

                let action = handle_builtin(&input, &session_id, &active_agent, project_branch);
                match action {
                    BuiltinAction::Exit => break,
                    BuiltinAction::Handled => {}
                    BuiltinAction::NotBuiltin => {
                        // Not a builtin — send as a prompt.
                        if let Err(e) =
                            execute_turn(&options, &session_id, &active_agent, &input).await
                        {
                            // `{:#}` prints the full anyhow context chain (outer: inner: root cause)
                            // so the underlying cause is not swallowed by the outermost context.
                            eprintln!("{} {e:#}", "error:".red());
                        }
                        // Visual separator between turns.
                        print_turn_separator();
                    }
                }
            }
            Err(ReadlineError::Interrupted) => {
                // Ctrl-C at an empty prompt: clear the line and re-prompt; do not exit.
                continue;
            }
            Err(ReadlineError::Eof) => {
                // Ctrl-D on an empty line: exit like the old EOF behavior.
                println!();
                break;
            }
            Err(err) => {
                eprintln!("{} input: {err}", "error:".red());
                break;
            }
        }
    }

    Ok(std::process::ExitCode::SUCCESS)
}

/// Print the startup banner with dimmed dividers and aligned metadata.
fn print_banner(session_id: &str, active_agent: &str, project_branch: &str) {
    let rule = "─".repeat(40).dimmed();
    println!();
    println!("{rule}");
    println!("  {}", "LATTICE".bold());
    println!("  {} {}", "session".dimmed(), session_id);
    println!("  {} {}", "agent".dimmed(), active_agent);
    println!("  {} {}", "project".dimmed(), project_branch);
    println!("{rule}");
    println!("  {}  show commands", "/help".cyan());
    println!("  {}  clear screen", "/clear".cyan());
    println!("  {}  exit  (Ctrl-D on empty line)", "/quit".cyan());
    println!("{rule}");
    println!();
}

/// Print a dimmed separator after each turn to visually delimit rounds.
fn print_turn_separator() {
    println!("{}", "─".repeat(40).dimmed());
}

/// Outcome of a builtin slash command.
enum BuiltinAction {
    /// Input was not a builtin; the caller should run it as a prompt.
    NotBuiltin,
    /// Input was a builtin and was fully handled.
    Handled,
    /// Input requested REPL exit.
    Exit,
}

/// Handle a builtin slash command.
fn handle_builtin(
    input: &str,
    session_id: &str,
    active_agent: &str,
    project_branch: &str,
) -> BuiltinAction {
    if input == "/quit" || input == "/exit" || input == "/q" {
        println!();
        return BuiltinAction::Exit;
    }
    if input == "/help" || input == "/?" {
        print_help();
        return BuiltinAction::Handled;
    }
    if input == "/clear" {
        // Clear the screen and reprint the banner so context is preserved.
        print!("\x1b[2J\x1b[H");
        stdout().flush().ok();
        print_banner(session_id, active_agent, project_branch);
        return BuiltinAction::Handled;
    }
    BuiltinAction::NotBuiltin
}

fn print_help() {
    let rule = "─".repeat(40).dimmed();
    println!("{rule}");
    println!("  {}  this message", "/help".cyan());
    println!("  {}  clear screen and reprint banner", "/clear".cyan());
    println!("  {}  exit (also Ctrl-D on empty line)", "/quit".cyan());
    println!("{rule}");
    println!("  {}  cancel the running turn", "Ctrl-C".dimmed());
    println!(
        "  {}  anything else is sent as a prompt to the agent",
        "·".dimmed()
    );
    println!("{rule}");
}

/// Execute one prompt: load profile, stream turn, render events to stdout/stderr.
async fn execute_turn(
    options: &TuiOptions,
    session_id: &str,
    active_agent: &str,
    prompt: &str,
) -> Result<()> {
    let loaded_profile = lattice_api::resolve_named_agent_profile(
        active_agent,
        &options.project_root,
        &options.agents_dir,
    )
    .with_context(|| format!("load agent profile {active_agent}"))?;

    if loaded_profile.profile.agent.name != active_agent {
        anyhow::bail!(
            "agent profile '{}' declares agent.name='{}'; these must match",
            active_agent,
            loaded_profile.profile.agent.name
        );
    }

    let git_repo_path = options.session_dir.join("sessions.git");
    let session_api = SessionApi::git(git_repo_path);

    // Cooperative cancellation flag: a Ctrl-C during the turn sets this and
    // the agent loop checkpoints out at its next cancel boundary.
    let cancel_flag = Arc::new(AtomicBool::new(false));
    let request = RunTurnRequest {
        input: prompt.to_string(),
        session_id: session_id.to_string(),
        loaded_profile: Some(loaded_profile),
        cancel_flag: Some(cancel_flag.clone()),
        test_llm: None,
        test_project_root: None,
        test_persist_audit: None,
    };

    let mut stream = lattice_api::stream_turn(session_api, request).await?;
    let mut tokens_ended_without_newline = false;

    let mut out = stdout().lock();
    let mut err = std::io::stderr().lock();

    // Spawn a Ctrl-C watcher that flips the cancel flag mid-turn. It is
    // aborted when the turn stream ends so the next prompt's Ctrl-C is handled
    // by rustyline instead.
    let cancel_task = tokio::spawn({
        let flag = cancel_flag.clone();
        async move {
            loop {
                if tokio::signal::ctrl_c().await.is_err() {
                    break;
                }
                if !flag.load(Ordering::Relaxed) {
                    flag.store(true, Ordering::Relaxed);
                    eprintln!("{} cancelling…", "✗".red());
                }
            }
        }
    });

    while let Some(event) = stream.next().await {
        match event {
            TurnEvent::TokenDelta { content } => {
                tokens_ended_without_newline = !content.ends_with('\n');
                write!(out, "{content}")?;
                out.flush()?;
            }
            TurnEvent::ToolCallStarted { name, .. } => {
                flush_mid_token(&mut out, &mut err, &mut tokens_ended_without_newline)?;
                writeln!(err, "{} {} {}", gutter(), "·".dimmed(), name)?;
            }
            TurnEvent::ToolCallFinished {
                name,
                ok,
                output_len,
                ..
            } => {
                flush_mid_token(&mut out, &mut err, &mut tokens_ended_without_newline)?;
                if ok {
                    writeln!(
                        err,
                        "{} {} {} ({} chars)",
                        gutter(),
                        "✓".green(),
                        name,
                        output_len
                    )?;
                } else {
                    writeln!(
                        err,
                        "{} {} {} ({} chars)",
                        gutter(),
                        "✗".red(),
                        name,
                        output_len
                    )?;
                }
            }
            TurnEvent::TurnCommitted { commit_id, .. } => {
                flush_mid_token(&mut out, &mut err, &mut tokens_ended_without_newline)?;
                writeln!(err, "{} {} committed {}", gutter(), "✓".green(), commit_id)?;
            }
            TurnEvent::TurnFailed { reason } => {
                flush_mid_token(&mut out, &mut err, &mut tokens_ended_without_newline)?;
                writeln!(err, "{} {} {}", gutter(), "✗".red(), reason)?;
            }
            TurnEvent::TurnCancelled { reason } => {
                flush_mid_token(&mut out, &mut err, &mut tokens_ended_without_newline)?;
                if let Some(r) = reason {
                    writeln!(err, "{} {} cancelled: {}", gutter(), "✗".red(), r)?;
                } else {
                    writeln!(err, "{} {} cancelled", gutter(), "✗".red())?;
                }
            }
            TurnEvent::CommitFailed { reason } => {
                flush_mid_token(&mut out, &mut err, &mut tokens_ended_without_newline)?;
                writeln!(err, "{} {} commit failed: {}", gutter(), "✗".red(), reason)?;
            }
            TurnEvent::LlmRequestError {
                attempt,
                max_attempts,
                will_retry,
                delay_ms,
                message,
                ..
            } => {
                flush_mid_token(&mut out, &mut err, &mut tokens_ended_without_newline)?;
                if will_retry {
                    let delay = delay_ms.unwrap_or(0);
                    writeln!(
                        err,
                        "{} {} llm retry {}/{} after {}ms: {}",
                        gutter(),
                        "·".dimmed(),
                        attempt,
                        max_attempts,
                        delay,
                        message
                    )?;
                } else {
                    writeln!(
                        err,
                        "{} {} llm failed {}/{}: {}",
                        gutter(),
                        "✗".red(),
                        attempt,
                        max_attempts,
                        message
                    )?;
                }
            }
            // silent events
            TurnEvent::ModelStarted { .. }
            | TurnEvent::ModelFinished { .. }
            | TurnEvent::ReasoningDelta { .. }
            | TurnEvent::TurnStarted { .. }
            | TurnEvent::TurnCommitting
            | TurnEvent::CepCallStarted { .. }
            | TurnEvent::CepCallFinished { .. } => {}
        }
    }

    // trailing newline if token stream didn't end with one
    if tokens_ended_without_newline {
        writeln!(out)?;
    }
    out.flush()?;
    err.flush()?;

    // Stop the Ctrl-C watcher so the next prompt's Ctrl-C is owned by rustyline.
    cancel_task.abort();

    Ok(())
}

/// Dimmed left gutter that visually separates stderr status lines from the
/// token stream on stdout.
fn gutter() -> String {
    "│".dimmed().to_string()
}

/// When writing a status line to stderr mid-stream (between token deltas that
/// haven't been terminated by a newline yet), emit a newline on stderr first
/// so the status line starts on its own row.
fn flush_mid_token<WOut: Write, WErr: Write>(
    out: &mut WOut,
    err: &mut WErr,
    tokens_ended_without_newline: &mut bool,
) -> std::io::Result<()> {
    if *tokens_ended_without_newline {
        out.flush()?;
        writeln!(err)?;
        *tokens_ended_without_newline = false;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_session_project_branch_is_required() {
        let error = required_project_branch(None).expect_err("missing branch must fail");
        assert!(
            error
                .to_string()
                .contains("--project-branch is required when creating a new TUI session")
        );
    }

    #[test]
    fn new_session_project_branch_rejects_blank_values() {
        let error = required_project_branch(Some("  ")).expect_err("blank branch must fail");
        assert!(
            error
                .to_string()
                .contains("--project-branch is required when creating a new TUI session")
        );
    }

    #[test]
    fn new_session_project_branch_preserves_explicit_value() {
        assert_eq!(
            required_project_branch(Some("feature/x")).expect("valid branch"),
            "feature/x"
        );
    }
}
