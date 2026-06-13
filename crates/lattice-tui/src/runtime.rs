// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Inline CLI REPL: `> ` prompt, line read, stream tokens to stdout,
//! tool/status lines to stderr. No alternate screen, no raw mode.

use anyhow::{Context, Result};
use colored::Colorize;
use futures::StreamExt;
use lattice_api::{
    CreateSessionRequest, OpenSessionRequest, RunTurnRequest, SessionApi, TurnEvent,
};
use std::io::{Write, stdout};
use std::path::PathBuf;

#[derive(Clone, Debug)]
pub struct TuiOptions {
    pub session_id: Option<String>,
    pub agent: String,
    pub session_dir: PathBuf,
    pub agents_dir: PathBuf,
    pub project_root: PathBuf,
}

pub async fn run(options: TuiOptions) -> Result<std::process::ExitCode> {
    let session_api = SessionApi::sqlite(options.session_dir.clone());
    let session = match &options.session_id {
        Some(session_id) => session_api
            .open_session(OpenSessionRequest {
                id: session_id.clone(),
            })
            .with_context(|| format!("open session {session_id}"))?,
        None => session_api
            .create_session(CreateSessionRequest {
                label: None,
                active_agent: options.agent.clone(),
            })
            .context("create chat session")?,
    };

    let session_id = session.session_id;
    let active_agent = session.active_agent_name;

    // ---------- banner ----------
    println!();
    println!("  LATTICE");
    println!("  session  {}", session_id);
    println!("  agent    {}", active_agent);
    println!();
    println!("  /help    show commands");
    println!("  /quit    exit");
    println!();

    // ---------- REPL ----------
    loop {
        // prompt
        print!("{} ", ">".cyan());
        stdout().flush()?;

        // read line (blocking on a tokio blocking thread so we don't stall the runtime)
        let (n, input) = tokio::task::spawn_blocking(|| {
            let mut buf = String::new();
            let n = std::io::stdin().read_line(&mut buf)?;
            Ok::<_, std::io::Error>((n, buf))
        })
        .await??;
        if n == 0 {
            // EOF
            println!();
            break;
        }

        let input = input.trim().to_string();
        if input.is_empty() {
            continue;
        }

        // built-in commands
        if input == "/quit" || input == "/exit" || input == "/q" {
            break;
        }
        if input == "/help" || input == "/?" {
            print_help();
            continue;
        }

        // execute turn — errors go to stderr, we keep the REPL alive
        if let Err(e) = execute_turn(&options, &session_id, &active_agent, &input).await {
            // `{:#}` prints the full anyhow context chain (outer: inner: root cause)
            // so the underlying cause is not swallowed by the outermost context.
            eprintln!("{} {e:#}", "error:".red());
        }

        // ensure a clean line after turn output
        println!();
    }

    Ok(std::process::ExitCode::SUCCESS)
}

fn print_help() {
    println!("  /help        this message");
    println!("  /quit, /q    exit");
    println!("  anything else is sent as a prompt to the agent");
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

    let session_api = SessionApi::sqlite(options.session_dir.clone());
    let request = RunTurnRequest {
        input: prompt.to_string(),
        session_id: session_id.to_string(),
        loaded_profile: Some(loaded_profile),
        cancel_flag: None,
        test_llm: None,
        test_project_root: None,
        test_persist_audit: None,
    };

    let mut stream = lattice_api::stream_turn(session_api, request).await?;
    let mut tokens_ended_without_newline = false;

    let mut out = stdout().lock();
    let mut err = std::io::stderr().lock();

    while let Some(event) = stream.next().await {
        match event {
            TurnEvent::TokenDelta { content } => {
                tokens_ended_without_newline = !content.ends_with('\n');
                write!(out, "{content}")?;
                out.flush()?;
            }
            TurnEvent::ToolCallStarted { name, .. } => {
                flush_mid_token(&mut out, &mut err, &mut tokens_ended_without_newline)?;
                writeln!(err, "  {} {name}", "·".dimmed())?;
            }
            TurnEvent::ToolCallFinished {
                name,
                ok,
                output_len,
                ..
            } => {
                flush_mid_token(&mut out, &mut err, &mut tokens_ended_without_newline)?;
                if ok {
                    writeln!(err, "  {} {name} ({output_len} chars)", "✓".green())?;
                } else {
                    writeln!(err, "  {} {name} ({output_len} chars)", "✗".red())?;
                }
            }
            TurnEvent::TurnCommitted { commit_id, .. } => {
                flush_mid_token(&mut out, &mut err, &mut tokens_ended_without_newline)?;
                writeln!(err, "  {} committed {commit_id}", "✓".green())?;
            }
            TurnEvent::TurnFailed { reason } => {
                flush_mid_token(&mut out, &mut err, &mut tokens_ended_without_newline)?;
                writeln!(err, "  {} {reason}", "✗".red())?;
            }
            TurnEvent::TurnCancelled { reason } => {
                flush_mid_token(&mut out, &mut err, &mut tokens_ended_without_newline)?;
                if let Some(r) = reason {
                    writeln!(err, "  {} cancelled: {r}", "✗".red())?;
                } else {
                    writeln!(err, "  {} cancelled", "✗".red())?;
                }
            }
            TurnEvent::CommitFailed { reason } => {
                flush_mid_token(&mut out, &mut err, &mut tokens_ended_without_newline)?;
                writeln!(err, "  {} commit failed: {reason}", "✗".red())?;
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
                        "  {} llm retry {attempt}/{max_attempts} after {delay}ms: {message}",
                        "·".dimmed()
                    )?;
                } else {
                    writeln!(
                        err,
                        "  {} llm failed {attempt}/{max_attempts}: {message}",
                        "✗".red()
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
            | TurnEvent::CepCallFinished { .. }
            | TurnEvent::CepEnvelopeDeclared { .. }
            | TurnEvent::CepEnvelopeSuperseded { .. }
            | TurnEvent::CepEnvelopeMissing { .. }
            | TurnEvent::CepVerifyFailed { .. }
            | TurnEvent::CepReplanRequired { .. } => {}
        }
    }

    // trailing newline if token stream didn't end with one
    if tokens_ended_without_newline {
        writeln!(out)?;
    }
    out.flush()?;
    err.flush()?;
    Ok(())
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
