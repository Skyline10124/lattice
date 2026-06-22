// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Singular `session` command over the session graph store.
//!
//! Supports session-first turn execution plus status, list, log, checkout,
//! branch, diff, and reset.
//! Does not import or call old session manager surfaces.

use std::path::PathBuf;

use anyhow::Result;
use clap::Subcommand;
use colored::Colorize;
use lattice_api::{CreateSessionRequest, GraphHead, OpenSessionRequest, SessionApi};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// Lattice config root (`$XDG_CONFIG_HOME` or platform config dir).
pub(crate) fn lattice_config_dir() -> PathBuf {
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
        let xdg = xdg.trim();
        if !xdg.is_empty() {
            return PathBuf::from(xdg);
        }
    }
    dirs::config_dir()
        .or_else(|| dirs::home_dir().map(|home| home.join(".config")))
        .unwrap_or_else(|| PathBuf::from("."))
}

pub(crate) fn default_session_dir() -> PathBuf {
    lattice_config_dir().join("lattice").join("sessions")
}

fn session_api() -> SessionApi {
    SessionApi::sqlite(default_session_dir())
}

#[derive(Subcommand)]
pub enum SessionAction {
    /// Create a new session with an initial active agent.
    New {
        /// Agent profile name to use as the initial active agent.
        #[arg(long)]
        agent: String,
        /// Project target branch for the session (required).
        #[arg(long)]
        project_branch: String,
        /// Optional session label.
        #[arg(long)]
        label: Option<String>,
    },
    /// Change the active agent for the checked-out branch.
    Agent {
        /// Session id (required).
        session_id: String,
        /// Agent profile name.
        agent: String,
    },
    /// Send a prompt to the active agent in an existing session.
    Send {
        /// Session id (required).
        session_id: String,
        /// Prompt text.
        prompt: String,
    },
    /// Show session store status or a specific session's status.
    Status {
        /// Optional session id to inspect.
        session_id: Option<String>,
    },
    /// List all sessions.
    List,
    /// Show the turn log for a session.
    Log {
        /// Session id (required).
        session_id: String,
    },
    /// Move a session's HEAD/current branch pointer.
    Checkout {
        /// Session id (required).
        session_id: String,
        /// Branch name or commit id to check out.
        target: String,
    },
    /// List branches for a session, or create a new branch.
    Branch {
        /// Session id (required).
        session_id: String,
        /// Branch name to create. Omit to list branches.
        name: Option<String>,
        /// Create branch from a specific commit.
        #[arg(long)]
        from: Option<String>,
    },
    /// Show diff between two refs in a session.
    Diff {
        /// Session id (required).
        session_id: String,
        /// From ref.
        from: String,
        /// To ref.
        to: String,
    },
    /// Reset a session's HEAD to a specific commit.
    Reset {
        /// Session id (required).
        session_id: String,
        /// Commit id to reset to.
        target: String,
    },
    /// Resume a pending turn from a cold snapshot (D-16, cold resume).
    Resume {
        /// Session id containing the pending turn to resume.
        session_id: String,
    },
    /// Prune retained snapshots scoped to a session (D-17, manual prune).
    PruneSnapshots {
        /// Session id whose snapshots should be pruned.
        session_id: String,
    },
}

/// Run the `session` command.
pub async fn run(
    action: SessionAction,
    json: bool,
    verbose: bool,
) -> Result<std::process::ExitCode> {
    let api = session_api();
    match action {
        SessionAction::New {
            agent,
            project_branch,
            label,
        } => {
            cmd_new_api(&api, &agent, &project_branch, label.as_deref(), json)?;
            Ok(std::process::ExitCode::SUCCESS)
        }
        SessionAction::Agent { session_id, agent } => {
            cmd_agent_api(&api, &session_id, &agent, json)?;
            Ok(std::process::ExitCode::SUCCESS)
        }
        SessionAction::Send { session_id, prompt } => {
            let exit = cmd_send_api(&api, &session_id, prompt, json, verbose).await;
            Ok(exit)
        }
        SessionAction::Resume { session_id } => {
            let exit = cmd_resume_api(&api, &session_id, json).await;
            Ok(exit)
        }
        SessionAction::PruneSnapshots { session_id } => {
            let exit = cmd_prune_snapshots_api(&api, &session_id, json).await;
            Ok(exit)
        }
        other => {
            run_with_api(&api, other, json)?;
            Ok(std::process::ExitCode::SUCCESS)
        }
    }
}

/// Run the `session` command with an explicit [`SessionApi`] instance.
///
/// This is the testable seam: production `run()` constructs the default API
/// and delegates here. Tests pass a temp-backed `SessionApi` directly.
pub(crate) fn run_with_api(api: &SessionApi, action: SessionAction, json: bool) -> Result<()> {
    match action {
        SessionAction::New {
            agent,
            project_branch,
            label,
        } => cmd_new_api(api, &agent, &project_branch, label.as_deref(), json)?,
        SessionAction::Agent { session_id, agent } => {
            cmd_agent_api(api, &session_id, &agent, json)?
        }
        SessionAction::Send { .. } => {
            return Err(anyhow::anyhow!(
                "session send requires the async production runner"
            ));
        }
        SessionAction::Status { session_id } => cmd_status_api(api, session_id.as_deref(), json)?,
        SessionAction::List => cmd_list_api(api, json)?,
        SessionAction::Log { session_id } => cmd_log_api(api, &session_id, json)?,
        SessionAction::Checkout { session_id, target } => {
            cmd_checkout_api(api, &session_id, &target, json)?
        }
        SessionAction::Branch {
            session_id,
            name,
            from,
        } => cmd_branch_api(api, &session_id, name.as_deref(), from.as_deref(), json)?,
        SessionAction::Diff {
            session_id,
            from,
            to,
        } => cmd_diff_api(api, &session_id, &from, &to, json)?,
        SessionAction::Reset { session_id, target } => {
            cmd_reset_api(api, &session_id, &target, json)?
        }
        SessionAction::Resume { .. } | SessionAction::PruneSnapshots { .. } => {
            return Err(anyhow::anyhow!(
                "resume and prune-snapshots require the async dispatcher"
            ));
        }
    }
    Ok(())
}

fn load_agent_checked(agent: &str) -> Result<lattice_api::LoadedAgentProfile> {
    let project_root = std::env::current_dir()?;
    let loaded = crate::commands::run::load_named_profile(agent, &project_root)?;
    if loaded.profile.agent.name != agent {
        return Err(anyhow::anyhow!(
            "agent profile '{}' declares agent.name='{}'; these must match",
            agent,
            loaded.profile.agent.name
        ));
    }
    Ok(loaded)
}

fn cmd_new_api(
    api: &SessionApi,
    agent: &str,
    project_branch: &str,
    label: Option<&str>,
    json: bool,
) -> Result<()> {
    let _loaded = load_agent_checked(agent)?;
    let session = api.create_session(CreateSessionRequest {
        label: label.map(str::to_string),
        active_agent: agent.to_string(),
        project_branch: project_branch.to_string(),
    })?;
    if json {
        let value = serde_json::json!({
            "session_id": session.session_id,
            "label": session.label,
            "active_agent": session.active_agent_name,
            "project_branch": session.project_target_branch,
        });
        println!("{}", serde_json::to_string_pretty(&value)?);
    } else {
        let branch = session
            .project_target_branch
            .as_deref()
            .unwrap_or(project_branch);
        println!(
            "{} Created session {} with agent {} on branch {}.",
            "\u{2713}".green(),
            display_id(&session.session_id).cyan(),
            session.active_agent_name.bold(),
            branch.bold()
        );
    }
    Ok(())
}

fn cmd_agent_api(api: &SessionApi, session_id: &str, agent: &str, json: bool) -> Result<()> {
    let _loaded = load_agent_checked(agent)?;
    let session = api.set_active_agent(session_id, agent)?;
    if json {
        let value = serde_json::json!({
            "session_id": session.session_id,
            "active_agent": session.active_agent_name,
        });
        println!("{}", serde_json::to_string_pretty(&value)?);
    } else {
        println!(
            "{} Active agent for session {} is now {}.",
            "\u{2713}".green(),
            display_id(&session.session_id).cyan(),
            session.active_agent_name.bold()
        );
    }
    Ok(())
}

async fn cmd_send_api(
    api: &SessionApi,
    session_id: &str,
    prompt: String,
    json: bool,
    verbose: bool,
) -> std::process::ExitCode {
    let session = match api.open_session(OpenSessionRequest {
        id: session_id.to_string(),
    }) {
        Ok(session) => session,
        Err(error) => {
            eprintln!("{}: {:#}", "error".red(), error);
            return std::process::ExitCode::from(2);
        }
    };

    let loaded = match load_agent_checked(&session.active_agent_name) {
        Ok(loaded) => loaded,
        Err(error) => {
            eprintln!("{}: {:#}", "error".red(), error);
            return std::process::ExitCode::from(2);
        }
    };

    crate::commands::run::run_session_prompt(prompt, session_id.to_string(), loaded, verbose, json)
        .await
}

fn cmd_status_api(api: &SessionApi, session_id: Option<&str>, json: bool) -> Result<()> {
    match session_id {
        Some(id) => {
            let status = api.status(id)?;
            if json {
                let value = serde_json::json!({
                    "session_id": status.session_id,
                    "head": status.head,
                    "current_commit_id": status.current_commit_id,
                    "active_agent": status.active_agent_name,
                    "branches": status.branches,
                    "commit_count": status.commit_count,
                    "project_target_branch": status.project_target_branch,
                });
                println!("{}", serde_json::to_string_pretty(&value)?);
            } else {
                println!(
                    "{} {}",
                    "Session:".bold(),
                    display_id(&status.session_id).cyan()
                );
                match &status.head {
                    GraphHead::Branch { name } => {
                        println!("  HEAD:     branch {}", name);
                    }
                    GraphHead::Detached { commit_id } => {
                        println!("  HEAD:     detached at {}", display_id(commit_id));
                    }
                }
                if let Some(cid) = &status.current_commit_id {
                    println!("  Commit:   {}", display_id(cid));
                }
                println!("  Agent:    {}", status.active_agent_name);
                if let Some(branch) = &status.project_target_branch {
                    println!("  Project:  {}", branch);
                }
                println!("  Branches: {}", status.branches.len());
                println!("  Commits:  {}", status.commit_count);
            }
        }
        None => {
            let sessions = api.list_sessions()?;
            if json {
                let value = serde_json::json!({
                    "session_count": sessions.len(),
                    "sessions": sessions,
                });
                println!("{}", serde_json::to_string_pretty(&value)?);
            } else if sessions.is_empty() {
                println!("{}", "No sessions found.".yellow());
            } else {
                println!("{} {} session(s)", "Status:".bold(), sessions.len());
                for s in &sessions {
                    let label = s.label.as_deref().unwrap_or("(untitled)");
                    let branch = s.current_branch.as_deref().unwrap_or("main");
                    let project = s.project_target_branch.as_deref().unwrap_or("main");
                    println!(
                        "  {} {} msgs  {} [{}] agent={} project={}",
                        display_id(&s.session_id).cyan(),
                        s.commit_count,
                        label.dimmed(),
                        branch,
                        s.active_agent_name,
                        project
                    );
                }
            }
        }
    }
    Ok(())
}

fn cmd_list_api(api: &SessionApi, json: bool) -> Result<()> {
    let sessions = api.list_sessions()?;
    if json {
        let value = serde_json::json!({
            "sessions": sessions,
        });
        println!("{}", serde_json::to_string_pretty(&value)?);
    } else if sessions.is_empty() {
        println!("{}", "No sessions found.".yellow());
    } else {
        for s in &sessions {
            let label = s.label.as_deref().unwrap_or("");
            let branch = s.current_branch.as_deref().unwrap_or("main");
            let head = s.head_commit_id.as_deref().unwrap_or("");
            let project = s.project_target_branch.as_deref().unwrap_or("main");
            println!(
                "  {}  {} msgs  {} [{}] agent={} head={} project={}",
                display_id(&s.session_id).cyan(),
                s.commit_count,
                label.dimmed(),
                branch,
                s.active_agent_name,
                display_id(head),
                project
            );
        }
    }
    Ok(())
}

fn cmd_log_api(api: &SessionApi, session_id: &str, json: bool) -> Result<()> {
    // Verify session exists first.
    let _status = api.status(session_id)?;
    let entries = api.log(session_id)?;
    if json {
        let value = serde_json::json!({
            "session_id": session_id,
            "turns": entries,
        });
        println!("{}", serde_json::to_string_pretty(&value)?);
    } else if entries.is_empty() {
        println!("{}", "No turns found.".yellow());
    } else {
        for (i, entry) in entries.iter().enumerate() {
            print!("{}", render_turn_log_entry(i, entry));
        }
    }
    Ok(())
}

fn render_turn_log_entry(index: usize, entry: &lattice_api::TurnLogEntry) -> String {
    let mut lines = vec![
        format!("{}", format!("turn {index}").bold()),
        format!("  {} {}", "commit:".green(), display_id(&entry.commit_id)),
        format!("  {} {}", "status:".green(), entry.status.as_str()),
        format!(
            "  {} {}",
            "disposition:".green(),
            entry.disposition.as_str()
        ),
        format!("  {} {}", "agent:".green(), entry.agent_name),
        format!("  {} {}", "model:".green(), entry.model),
        format!("  {} {}", "provider:".green(), entry.provider),
        format!("  {} {}", "messages:".green(), entry.message_count),
    ];
    if entry.tool_message_count > 0 {
        lines.push(format!(
            "  {} {}",
            "tools:".green(),
            entry.tool_message_count
        ));
    }
    lines.push(format!(
        "  {} {}",
        "message:".green(),
        summarize(&entry.commit_message, 120)
    ));
    lines.push(String::new());
    let mut rendered = lines.join("\n");
    rendered.push('\n');
    rendered
}

fn cmd_checkout_api(api: &SessionApi, session_id: &str, target: &str, json: bool) -> Result<()> {
    let head = api.checkout(session_id, target)?;
    if json {
        let value = serde_json::json!({
            "session_id": session_id,
            "head": head,
        });
        println!("{}", serde_json::to_string_pretty(&value)?);
    } else {
        match &head {
            GraphHead::Branch { name } => {
                println!(
                    "{} Checked out branch {} in session {}.",
                    "\u{2713}".green(),
                    name.bold(),
                    display_id(session_id).cyan()
                );
            }
            GraphHead::Detached { commit_id } => {
                println!(
                    "{} Detached HEAD at {} in session {}.",
                    "\u{2713}".green(),
                    display_id(commit_id).bold(),
                    display_id(session_id).cyan()
                );
            }
        }
    }
    Ok(())
}

fn cmd_branch_api(
    api: &SessionApi,
    session_id: &str,
    name: Option<&str>,
    from: Option<&str>,
    json: bool,
) -> Result<()> {
    match name {
        Some(branch_name) => {
            let branch_ref = api.branch(session_id, branch_name, from)?;
            if json {
                let value = serde_json::json!({
                    "session_id": branch_ref.session_id,
                    "name": branch_ref.name,
                    "kind": branch_ref.kind,
                    "commit_id": branch_ref.commit_id,
                    "active_agent": branch_ref.active_agent_name,
                    "updated_at": branch_ref.updated_at,
                });
                println!("{}", serde_json::to_string_pretty(&value)?);
            } else {
                println!(
                    "{} Created branch {} in session {}.",
                    "\u{2713}".green(),
                    branch_name.bold(),
                    display_id(session_id).cyan()
                );
            }
        }
        None => {
            // Branch listing reads SessionStatus.branches for the
            // addressed session, not store-global branch state.
            let status = api.status(session_id)?;
            if json {
                let value = serde_json::json!({
                    "session_id": session_id,
                    "branches": status.branches,
                });
                println!("{}", serde_json::to_string_pretty(&value)?);
            } else if status.branches.is_empty() {
                println!("{}", "No branches found.".yellow());
            } else {
                for branch in &status.branches {
                    let current = match &status.head {
                        GraphHead::Branch { name } if name == &branch.name => "*",
                        _ => " ",
                    };
                    let commit_label = branch
                        .commit_id
                        .as_deref()
                        .map(display_id)
                        .unwrap_or_default();
                    println!(
                        "{} {}  {}  agent={}",
                        current.green(),
                        branch.name.bold(),
                        commit_label.cyan(),
                        branch.active_agent_name
                    );
                }
            }
        }
    }
    Ok(())
}

fn cmd_diff_api(
    api: &SessionApi,
    session_id: &str,
    from: &str,
    to: &str,
    json: bool,
) -> Result<()> {
    let diff = api.diff(session_id, from, to)?;
    if json {
        let value = serde_json::json!({
            "session_id": session_id,
            "from_commit_id": diff.from_commit_id,
            "to_commit_id": diff.to_commit_id,
            "common_ancestor_id": diff.common_ancestor_id,
            "only_in_from": diff.only_in_from,
            "only_in_to": diff.only_in_to,
        });
        println!("{}", serde_json::to_string_pretty(&value)?);
    } else {
        println!(
            "{} -> {}",
            display_id(&diff.from_commit_id).cyan(),
            display_id(&diff.to_commit_id).cyan()
        );
        if let Some(ancestor) = &diff.common_ancestor_id {
            println!("  common ancestor: {}", display_id(ancestor));
        }
        println!("  only in from: {} commit(s)", diff.only_in_from.len());
        println!("  only in to:   {} commit(s)", diff.only_in_to.len());
    }
    Ok(())
}

fn cmd_reset_api(api: &SessionApi, session_id: &str, target: &str, json: bool) -> Result<()> {
    let result = api.reset(session_id, target)?;
    if json {
        let value = serde_json::json!({
            "session_id": result.session_id,
            "branch": result.branch,
            "old_commit_id": result.old_commit_id,
            "new_commit_id": result.new_commit_id,
        });
        println!("{}", serde_json::to_string_pretty(&value)?);
    } else {
        println!(
            "{} Reset session {} HEAD to {}.",
            "\u{2713}".green(),
            display_id(session_id).cyan(),
            display_id(&result.new_commit_id).bold()
        );
    }
    Ok(())
}

/// Resume a pending turn from a cold snapshot (D-16).
///
/// Calls the typed API (`lattice_api::resume_turn`) and renders the resulting
/// [`TurnCommit`] in JSON or human mode. Does not implement runtime logic locally.
async fn cmd_resume_api(api: &SessionApi, session_id: &str, json: bool) -> std::process::ExitCode {
    let request = lattice_api::ResumeTurnRequest {
        session_id: session_id.to_string(),
        loaded_profile: None,
        cancel_flag: None,
        workspace_override: None,
    };

    match lattice_api::resume_turn(api, request).await {
        Ok(commit) => {
            if json {
                let value = serde_json::json!({
                    "type": "turn_committed",
                    "session_id": commit.session_id,
                    "commit_id": commit.commit_id,
                    "status": format!("{:?}", commit.status),
                    "disposition": format!("{:?}", commit.disposition),
                    "agent": commit.agent_name,
                    "source_commit_id": commit.parent_ids.first().cloned().unwrap_or_default(),
                });
                println!(
                    "{}",
                    serde_json::to_string_pretty(&value).unwrap_or_default()
                );
            } else {
                println!(
                    "{} Resumed session {} \u{2192} commit {} ({} / {})",
                    "\u{2713}".green(),
                    display_id(session_id).cyan(),
                    display_id(&commit.commit_id).bold(),
                    format!("{:?}", commit.status).to_lowercase(),
                    format!("{:?}", commit.disposition).to_lowercase(),
                );
            }
            std::process::ExitCode::SUCCESS
        }
        Err(e) => {
            if json {
                let value = serde_json::json!({
                    "type": "error",
                    "session_id": session_id,
                    "error": format!("{e:#}"),
                });
                eprintln!(
                    "{}",
                    serde_json::to_string_pretty(&value).unwrap_or_default()
                );
            } else {
                eprintln!("{}: {:#}", "error".red(), e);
            }
            std::process::ExitCode::from(1)
        }
    }
}

/// Prune retained snapshots scoped to a session (D-17).
///
/// Calls the typed API (`lattice_api::prune_session_snapshots`) and renders
/// the exact released/skipped/missing counts. Requires a workspace override
/// in production; without it the API returns an error.
async fn cmd_prune_snapshots_api(
    api: &SessionApi,
    session_id: &str,
    json: bool,
) -> std::process::ExitCode {
    let request = lattice_api::PruneSnapshotsRequest {
        session_id: session_id.to_string(),
        workspace_override: None,
    };

    match lattice_api::prune_session_snapshots(api, request).await {
        Ok(result) => {
            let released_count = result.released.len();
            let skipped_count = result.skipped.len();
            let missing_count = result.already_missing.len();
            if json {
                let value = serde_json::json!({
                    "type": "prune_complete",
                    "session_id": session_id,
                    "released": released_count,
                    "skipped": skipped_count,
                    "already_missing": missing_count,
                });
                println!(
                    "{}",
                    serde_json::to_string_pretty(&value).unwrap_or_default()
                );
            } else {
                println!(
                    "{} Pruned session {} snapshots: {} released, {} skipped, {} already missing",
                    "\u{2713}".green(),
                    display_id(session_id).cyan(),
                    released_count,
                    skipped_count,
                    missing_count,
                );
            }
            std::process::ExitCode::SUCCESS
        }
        Err(e) => {
            if json {
                let value = serde_json::json!({
                    "type": "error",
                    "session_id": session_id,
                    "error": format!("{e:#}"),
                });
                eprintln!(
                    "{}",
                    serde_json::to_string_pretty(&value).unwrap_or_default()
                );
            } else {
                eprintln!("{}: {:#}", "error".red(), e);
            }
            std::process::ExitCode::from(1)
        }
    }
}

#[cfg(test)]
fn not_found(kind: &str, id: &str) -> anyhow::Error {
    anyhow::anyhow!("{} '{}' not found", kind, id)
}

fn display_id(id: &str) -> String {
    if id.len() > 8 {
        let end = id.char_indices().nth(8).map(|(i, _)| i).unwrap_or(id.len());
        format!("{}..", &id[..end])
    } else {
        id.to_string()
    }
}

/// Truncate `text` to at most `max` display columns, appending `…` if cut.
///
/// NOTE: `lattice-agent/src/agent_loop/mod.rs::truncate_to_width` contains a
/// similar column-width truncation loop. Keep both in sync if this logic changes.
fn summarize(text: &str, max: usize) -> String {
    let one_line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if one_line.width() <= max {
        return one_line;
    }
    let mut width = 0_usize;
    let mut end = 0_usize;
    let ellipsis = '\u{2026}';
    let ellipsis_width = ellipsis.width().unwrap_or(1);
    let limit = max.saturating_sub(ellipsis_width);
    for (i, c) in one_line.char_indices() {
        let cw = c.width().unwrap_or(0);
        if width + cw > limit {
            break;
        }
        width += cw;
        end = i + c.len_utf8();
    }
    let mut out = one_line[..end].to_string();
    out.push(ellipsis);
    out
}

#[cfg(test)]
mod session_cli {
    use super::*;
    use clap::Parser;

    /// Test wrapper to parse SessionAction as a subcommand.
    #[derive(Parser)]
    struct TestCli {
        #[command(subcommand)]
        action: SessionAction,
    }

    fn try_parse(args: &[&str]) -> std::result::Result<SessionAction, clap::Error> {
        let mut full_args = vec!["session"];
        full_args.extend_from_slice(args);
        TestCli::try_parse_from(full_args).map(|cli| cli.action)
    }

    #[test]
    fn parse_status_no_args() {
        let action = try_parse(&["status"]);
        assert!(action.is_ok());
        match action.unwrap() {
            SessionAction::Status { session_id } => assert!(session_id.is_none()),
            _ => panic!("expected Status"),
        }
    }

    #[test]
    fn parse_status_with_session_id() {
        let action = try_parse(&["status", "abc123"]);
        assert!(action.is_ok());
        match action.unwrap() {
            SessionAction::Status { session_id } => {
                assert_eq!(session_id.as_deref(), Some("abc123"));
            }
            _ => panic!("expected Status"),
        }
    }

    #[test]
    fn parse_list() {
        let action = try_parse(&["list"]);
        assert!(action.is_ok());
        assert!(matches!(action.unwrap(), SessionAction::List));
    }

    #[test]
    fn parse_log_with_session_id() {
        let action = try_parse(&["log", "sess-abc"]);
        assert!(action.is_ok());
        match action.unwrap() {
            SessionAction::Log { session_id } => {
                assert_eq!(session_id, "sess-abc");
            }
            _ => panic!("expected Log"),
        }
    }

    #[test]
    fn parse_log_without_session_id_fails() {
        let action = try_parse(&["log"]);
        assert!(action.is_err(), "log should require session_id");
    }

    #[test]
    fn human_turn_log_renders_disposition_next_to_status() {
        let entry = lattice_session::TurnLogEntry {
            commit_id: "abc123".to_string(),
            parent_ids: vec![],
            status: lattice_session::TurnCommitStatus::Completed,
            disposition: lattice_session::WorkspaceDisposition::PendingWorktree,
            agent_name: "coder".to_string(),
            profile_snapshot_hash: "profile".to_string(),
            model: "test-model".to_string(),
            provider: "test-provider".to_string(),
            commit_message: "pending cleanup".to_string(),
            message_count: 2,
            tool_message_count: 0,
            usage: None,
            terminal_reason: None,
            created_at: "2026-06-22T00:00:00Z".to_string(),
        };

        let rendered = render_turn_log_entry(0, &entry);
        assert!(rendered.contains("status:"));
        assert!(rendered.contains("completed"));
        assert!(rendered.contains("disposition:"));
        assert!(rendered.contains("pending_worktree"));
    }

    #[test]
    fn parse_checkout_with_session_id_and_target() {
        let action = try_parse(&["checkout", "sess-123", "feature-branch"]);
        assert!(action.is_ok());
        match action.unwrap() {
            SessionAction::Checkout { session_id, target } => {
                assert_eq!(session_id, "sess-123");
                assert_eq!(target, "feature-branch");
            }
            _ => panic!("expected Checkout"),
        }
    }

    #[test]
    fn parse_checkout_missing_target_fails() {
        let action = try_parse(&["checkout", "sess-123"]);
        assert!(action.is_err(), "checkout should require target");
    }

    #[test]
    fn parse_checkout_missing_session_id_fails() {
        let action = try_parse(&["checkout"]);
        assert!(action.is_err(), "checkout should require session_id");
    }

    #[test]
    fn parse_branch_list_mode() {
        let action = try_parse(&["branch", "sess-456"]);
        assert!(action.is_ok());
        match action.unwrap() {
            SessionAction::Branch {
                session_id,
                name,
                from,
            } => {
                assert_eq!(session_id, "sess-456");
                assert!(name.is_none());
                assert!(from.is_none());
            }
            _ => panic!("expected Branch"),
        }
    }

    #[test]
    fn parse_branch_create_mode() {
        let action = try_parse(&["branch", "sess-456", "dev", "--from", "abc123"]);
        assert!(action.is_ok());
        match action.unwrap() {
            SessionAction::Branch {
                session_id,
                name,
                from,
            } => {
                assert_eq!(session_id, "sess-456");
                assert_eq!(name.as_deref(), Some("dev"));
                assert_eq!(from.as_deref(), Some("abc123"));
            }
            _ => panic!("expected Branch"),
        }
    }

    #[test]
    fn parse_branch_without_session_id_fails() {
        let action = try_parse(&["branch"]);
        assert!(action.is_err(), "branch should require session_id");
    }

    #[test]
    fn parse_diff() {
        let action = try_parse(&["diff", "sess-789", "ref-a", "ref-b"]);
        assert!(action.is_ok());
        match action.unwrap() {
            SessionAction::Diff {
                session_id,
                from,
                to,
            } => {
                assert_eq!(session_id, "sess-789");
                assert_eq!(from, "ref-a");
                assert_eq!(to, "ref-b");
            }
            _ => panic!("expected Diff"),
        }
    }

    #[test]
    fn parse_diff_missing_args_fails() {
        let action = try_parse(&["diff", "sess-789"]);
        assert!(action.is_err(), "diff should require from and to");
    }

    #[test]
    fn parse_diff_missing_session_id_fails() {
        let action = try_parse(&["diff"]);
        assert!(action.is_err(), "diff should require session_id");
    }

    #[test]
    fn parse_reset() {
        let action = try_parse(&["reset", "sess-abc", "commit-xyz"]);
        assert!(action.is_ok());
        match action.unwrap() {
            SessionAction::Reset { session_id, target } => {
                assert_eq!(session_id, "sess-abc");
                assert_eq!(target, "commit-xyz");
            }
            _ => panic!("expected Reset"),
        }
    }

    #[test]
    fn parse_reset_missing_args_fails() {
        let action = try_parse(&["reset", "sess-abc"]);
        assert!(action.is_err(), "reset should require target");
    }

    #[test]
    fn parse_reset_missing_session_id_fails() {
        let action = try_parse(&["reset"]);
        assert!(action.is_err(), "reset should require session_id");
    }

    #[test]
    fn reject_old_subcommand_current() {
        assert!(try_parse(&["current"]).is_err());
    }

    #[test]
    fn reject_old_subcommand_show() {
        assert!(try_parse(&["show"]).is_err());
    }

    #[test]
    fn reject_old_subcommand_branches() {
        assert!(try_parse(&["branches"]).is_err());
    }

    #[test]
    fn reject_old_subcommand_reflog() {
        assert!(try_parse(&["reflog"]).is_err());
    }

    #[test]
    fn reject_old_subcommand_tag() {
        assert!(try_parse(&["tag"]).is_err());
    }

    #[test]
    fn reject_old_subcommand_tags() {
        assert!(try_parse(&["tags"]).is_err());
    }

    #[test]
    fn reject_old_subcommand_cherry_pick() {
        assert!(try_parse(&["cherry-pick"]).is_err());
    }

    #[test]
    fn reject_old_subcommand_merge() {
        assert!(try_parse(&["merge"]).is_err());
    }

    #[test]
    fn reject_old_subcommand_rebase() {
        assert!(try_parse(&["rebase"]).is_err());
    }

    #[test]
    fn reject_old_subcommand_stash() {
        assert!(try_parse(&["stash"]).is_err());
    }

    #[test]
    fn reject_old_subcommand_artifacts() {
        assert!(try_parse(&["artifacts"]).is_err());
    }

    #[test]
    fn reject_old_subcommand_manifests() {
        assert!(try_parse(&["manifests"]).is_err());
    }

    #[test]
    fn reject_old_subcommand_gc() {
        assert!(try_parse(&["gc"]).is_err());
    }

    #[test]
    fn reject_old_subcommand_migrate() {
        assert!(try_parse(&["migrate"]).is_err());
    }

    #[test]
    fn reject_old_subcommand_rm() {
        assert!(try_parse(&["rm"]).is_err());
    }

    #[test]
    fn not_found_error_includes_object_id() {
        let err = not_found("session", "missing-id-123");
        let msg = format!("{}", err);
        assert!(msg.contains("session"), "should mention object type");
        assert!(msg.contains("missing-id-123"), "should include the id");
    }

    #[test]
    fn summarize_short_text_unchanged() {
        assert_eq!(summarize("hello world", 120), "hello world");
    }

    #[test]
    fn summarize_long_text_truncated() {
        let long = "a".repeat(200);
        let result = summarize(&long, 50);
        assert!(result.chars().count() <= 50);
        assert!(result.ends_with('\u{2026}'));
    }

    #[test]
    fn summarize_cjk_short_unchanged() {
        // 5 CJK chars = 10 columns, well under 120 max
        assert_eq!(summarize("你好世界测试", 120), "你好世界测试");
    }

    #[test]
    fn summarize_cjk_truncated_at_column_boundary() {
        // 10 CJK chars = 20 columns, max 7 columns
        let cjk = "你好世界测试中文显示宽度";
        let result = summarize(cjk, 7);
        assert!(result.width() <= 7, "width {} > 7", result.width());
        assert!(result.ends_with('\u{2026}'));
        assert!(result.chars().count() < cjk.chars().count());
    }

    #[test]
    fn summarize_mixed_ascii_cjk_truncated() {
        // "hello世界" = 5 ascii + 2 cjk = 5 + 4 = 9 columns, max 5
        let result = summarize("hello世界", 5);
        assert!(result.width() <= 5, "width {} > 5", result.width());
        assert!(result.ends_with('\u{2026}'));
    }

    #[test]
    fn display_id_short_unchanged() {
        assert_eq!(display_id("abc"), "abc");
    }

    #[test]
    fn display_id_long_truncated() {
        let id = "abcdefghijklmnop";
        let result = display_id(id);
        assert!(result.len() < id.len());
        assert!(result.ends_with(".."));
    }

    // ─── resume and prune-snapshots source tests (36-15 Task 2 RED) ──────

    /// Helper: return only the implementation portion of session.rs
    /// (lines before #[cfg(test)] mod session_cli), which excludes test
    /// assertions and avoids self-matching in source scans.
    fn impl_source() -> String {
        let full = include_str!("session.rs");
        // Take only lines before the first `mod session_cli` test module
        // to avoid matching test assertion strings in source scans.
        let mut lines = Vec::new();
        for line in full.lines() {
            if line.starts_with("mod session_cli {") {
                break;
            }
            lines.push(line);
        }
        lines.join("\n")
    }

    #[test]
    fn session_resume_variant_exists_in_enum() {
        let src = impl_source();
        assert!(
            src.contains("Resume {") || src.contains("Resume{"),
            "SessionAction enum must define a Resume variant"
        );
    }

    #[test]
    fn session_prune_snapshots_variant_exists_in_enum() {
        let src = impl_source();
        assert!(
            src.contains("PruneSnapshots {") || src.contains("PruneSnapshots{"),
            "SessionAction enum must define a PruneSnapshots variant"
        );
    }

    #[test]
    fn session_resume_handler_function_exists() {
        let src = impl_source();
        assert!(
            src.contains("fn cmd_resume_api"),
            "session.rs must define a cmd_resume_api handler function"
        );
    }

    #[test]
    fn session_prune_handler_function_exists() {
        let src = impl_source();
        assert!(
            src.contains("fn cmd_prune_snapshots_api"),
            "session.rs must define a cmd_prune_snapshots_api handler function"
        );
    }

    #[test]
    fn session_resume_calls_typed_api() {
        let src = impl_source();
        assert!(
            src.contains("resume_turn("),
            "cmd_resume_api must call the typed API resume_turn()"
        );
    }

    #[test]
    fn session_prune_calls_typed_api() {
        let src = impl_source();
        assert!(
            src.contains("prune_session_snapshots("),
            "cmd_prune_snapshots_api must call the typed API prune_session_snapshots()"
        );
    }

    // ─── project_branch parser tests (36-15 Task 1 RED) ───────────────────

    #[test]
    fn project_branch_new_without_required_flag_fails() {
        // After Phase 36-15, session new must require --project-branch.
        // Currently this PASSES parsing (no required flag), making this test RED.
        let action = try_parse(&["new", "--agent", "coder"]);
        assert!(
            action.is_err(),
            "session new without --project-branch should fail parser validation"
        );
    }

    #[test]
    fn project_branch_new_with_required_flag_is_accepted() {
        let action = try_parse(&["new", "--agent", "coder", "--project-branch", "feature/x"]);
        assert!(
            action.is_ok(),
            "session new with --project-branch should parse"
        );
        match action.unwrap() {
            SessionAction::New {
                agent,
                project_branch,
                label,
            } => {
                assert_eq!(agent, "coder");
                assert_eq!(project_branch, "feature/x");
                assert!(label.is_none());
            }
            _ => panic!("expected New"),
        }
    }

    #[test]
    fn project_branch_send_has_no_branch_override() {
        // session send should NOT accept --project-branch.
        let action = try_parse(&["send", "sess1", "hello", "--project-branch", "main"]);
        assert!(
            action.is_err(),
            "session send must not accept --project-branch override"
        );
    }

    #[test]
    fn project_branch_send_source_has_no_branch_flag() {
        let source = include_str!("session.rs");
        // Find the Send variant definition and verify no project_branch field.
        let send_start = source.find("Send {").expect("Send variant should exist");
        let send_end = source[send_start..]
            .find("}")
            .expect("Send variant should close");
        let send_block = &source[send_start..send_start + send_end + 1];
        assert!(
            !send_block.contains("project_branch"),
            "Send variant must not contain project_branch field"
        );
    }

    #[test]
    fn project_branch_new_source_defines_field() {
        // Source scan: SessionAction::New variant must define project_branch field.
        let source = include_str!("session.rs");
        // Find the New variant in the enum definition
        let enum_start = source
            .find("pub enum SessionAction")
            .expect("enum should exist");
        let enum_body = &source[enum_start..];
        // Find the New variant block (between "New {" and the next variant marker "/// ")
        let new_start = enum_body.find("New {").expect("New variant should exist");
        // Scan to the closing brace of the variant (next line starting with spaces + "},")
        let new_block = &enum_body[new_start..new_start + 400.min(enum_body.len() - new_start)];
        assert!(
            new_block.contains("project_branch"),
            "New variant must define project_branch field"
        );
    }
}

// Scoped session regression tests: prove that checkout, branch, reset,
// diff, and branch listing are scoped to the addressed session_id.

#[cfg(test)]
mod d25_scoped_session {
    use super::*;
    use lattice_api::SessionApi;
    use lattice_session::{
        GraphMessage, SessionGraphStore, SqliteSessionGraphStore, TurnCommitInput, TurnCommitStatus,
    };

    /// Seed a session with N committed turns using the graph store directly.
    fn seed_session(
        store: &SqliteSessionGraphStore,
        session_id: &str,
        n: usize,
        label: Option<&str>,
    ) -> Vec<String> {
        store
            .create_session(session_id, label, "coder", "main")
            .expect("create_session");

        let mut commit_ids: Vec<String> = Vec::new();
        for i in 0..n {
            let parent_ids = if i == 0 {
                vec![]
            } else {
                vec![commit_ids.last().unwrap().clone()]
            };
            let messages = vec![
                GraphMessage {
                    role: "user".to_string(),
                    content: format!("{session_id} turn {i}"),
                    reasoning_content: None,
                    tool_calls: None,
                    tool_call_id: None,
                    name: None,
                },
                GraphMessage {
                    role: "assistant".to_string(),
                    content: format!("{session_id} response {i}"),
                    reasoning_content: None,
                    tool_calls: None,
                    tool_call_id: None,
                    name: None,
                },
            ];
            let input = TurnCommitInput {
                session_id: session_id.to_string(),
                parent_ids,
                messages,
                agent_name: "coder".to_string(),
                profile_snapshot_hash: "test-profile".to_string(),
                profile_snapshot_json: "{}".to_string(),
                model: "sonnet".to_string(),
                provider: "anthropic".to_string(),
                status: TurnCommitStatus::Completed,
                disposition: lattice_session::WorkspaceDisposition::NotApplicable,
                lifecycle: None,
                commit_message: format!("Turn {i}"),
                usage: None,
                terminal_reason: None,
            };
            let commit = store.commit_turn(input).expect("commit_turn");
            commit_ids.push(commit.commit_id);
        }
        commit_ids
    }

    /// Set up two sessions (sess-a, sess-b) each with 3 committed turns.
    /// Returns (tempdir guard, SessionApi, sess-a commit ids, sess-b commit ids).
    fn setup_two_sessions() -> (tempfile::TempDir, SessionApi, Vec<String>, Vec<String>) {
        let dir = tempfile::tempdir().unwrap();
        let store = SqliteSessionGraphStore::new(dir.path());
        let commits_a = seed_session(&store, "sess-a", 3, Some("Session A"));
        let commits_b = seed_session(&store, "sess-b", 3, Some("Session B"));
        drop(store);
        let api = SessionApi::sqlite(dir.path());
        (dir, api, commits_a, commits_b)
    }

    #[test]
    fn branch_listing_reads_only_addressed_session_refs() {
        let (_dir, api, commits_a, _commits_b) = setup_two_sessions();

        // Create same-named branch in both sessions, pointing to different commits.
        api.branch("sess-a", "feature", Some(&commits_a[0]))
            .unwrap();
        api.branch("sess-b", "feature", Some(&_commits_b[1]))
            .unwrap();

        // Branch listing for sess-a should only see sess-a's refs.
        let action = SessionAction::Branch {
            session_id: "sess-a".to_string(),
            name: None,
            from: None,
        };
        run_with_api(&api, action, false).unwrap();

        // Verify sess-a feature points to commits_a[0].
        let status_a = api.status("sess-a").unwrap();
        let feature_a = status_a
            .branches
            .iter()
            .find(|b| b.name == "feature")
            .expect("sess-a should have feature branch");
        assert_eq!(
            feature_a.commit_id,
            Some(commits_a[0].clone()),
            "sess-a feature should point to sess-a's first commit"
        );

        // Verify sess-b feature still points to commits_b[1].
        let status_b = api.status("sess-b").unwrap();
        let feature_b = status_b
            .branches
            .iter()
            .find(|b| b.name == "feature")
            .expect("sess-b should have feature branch");
        assert_eq!(
            feature_b.commit_id,
            Some(_commits_b[1].clone()),
            "sess-b feature should be unchanged"
        );
    }

    #[test]
    fn checkout_changes_only_addressed_session_head() {
        let (_dir, api, commits_a, _commits_b) = setup_two_sessions();

        // Create feature branch in sess-a.
        api.branch("sess-a", "feature", Some(&commits_a[0]))
            .unwrap();

        // Checkout feature in sess-a.
        let action = SessionAction::Checkout {
            session_id: "sess-a".to_string(),
            target: "feature".to_string(),
        };
        run_with_api(&api, action, false).unwrap();

        // Verify sess-a HEAD is on feature.
        let status_a = api.status("sess-a").unwrap();
        match &status_a.head {
            GraphHead::Branch { name } => assert_eq!(name, "feature"),
            other => panic!("expected Branch HEAD for sess-a, got {:?}", other),
        }

        // Verify sess-b HEAD is still on main (unchanged).
        let status_b = api.status("sess-b").unwrap();
        match &status_b.head {
            GraphHead::Branch { name } => assert_eq!(name, "main"),
            other => panic!("expected Branch HEAD for sess-b, got {:?}", other),
        }
    }

    #[test]
    fn branch_create_affects_only_addressed_session() {
        let (_dir, api, commits_a, _commits_b) = setup_two_sessions();

        // Create "work" branch in sess-a from first commit.
        let action = SessionAction::Branch {
            session_id: "sess-a".to_string(),
            name: Some("work".to_string()),
            from: Some(commits_a[0].clone()),
        };
        run_with_api(&api, action, false).unwrap();

        // Verify sess-a has "work" branch.
        let status_a = api.status("sess-a").unwrap();
        assert!(
            status_a.branches.iter().any(|b| b.name == "work"),
            "sess-a should have 'work' branch"
        );

        // Verify sess-b does NOT have "work" branch.
        let status_b = api.status("sess-b").unwrap();
        assert!(
            !status_b.branches.iter().any(|b| b.name == "work"),
            "sess-b should NOT have 'work' branch"
        );
    }

    #[test]
    fn reset_changes_only_addressed_session_branch_pointer() {
        let (_dir, api, commits_a, commits_b) = setup_two_sessions();

        // Record sess-b HEAD before reset.
        let status_b_before = api.status("sess-b").unwrap();
        let head_b_before = status_b_before.current_commit_id.clone();

        // Reset sess-a to first commit.
        let action = SessionAction::Reset {
            session_id: "sess-a".to_string(),
            target: commits_a[0].clone(),
        };
        run_with_api(&api, action, false).unwrap();

        // Verify sess-a HEAD changed to first commit.
        let status_a = api.status("sess-a").unwrap();
        assert_eq!(
            status_a.current_commit_id,
            Some(commits_a[0].clone()),
            "sess-a HEAD should be reset to first commit"
        );

        // Verify sess-b HEAD is unchanged.
        let status_b_after = api.status("sess-b").unwrap();
        assert_eq!(
            status_b_after.current_commit_id, head_b_before,
            "sess-b HEAD should be unchanged after sess-a reset"
        );

        // Verify sess-b current commit is still its last commit.
        assert_eq!(
            status_b_after.current_commit_id,
            Some(commits_b.last().unwrap().clone())
        );
    }

    #[test]
    fn diff_resolves_refs_inside_addressed_session_only() {
        let (_dir, api, commits_a, commits_b) = setup_two_sessions();

        // Create same-named "feature" branch in both sessions from different commits.
        api.branch("sess-a", "feature", Some(&commits_a[0]))
            .unwrap();
        api.branch("sess-b", "feature", Some(&commits_b[1]))
            .unwrap();

        // Diff sess-a: main vs feature.
        let action = SessionAction::Diff {
            session_id: "sess-a".to_string(),
            from: "main".to_string(),
            to: "feature".to_string(),
        };
        run_with_api(&api, action, false).unwrap();

        // Verify the diff resolved within sess-a.
        let diff = api.diff("sess-a", "main", "feature").unwrap();
        assert_eq!(
            diff.from_commit_id, commits_a[2],
            "from should be sess-a main HEAD"
        );
        assert_eq!(
            diff.to_commit_id, commits_a[0],
            "to should be sess-a feature commit"
        );

        // Verify sess-b's feature is a different commit (proving isolation).
        let diff_b = api.diff("sess-b", "main", "feature").unwrap();
        assert_eq!(
            diff_b.to_commit_id, commits_b[1],
            "sess-b feature should resolve to sess-b's commit"
        );
        assert_ne!(
            diff.to_commit_id, diff_b.to_commit_id,
            "same branch name should resolve to different commits in different sessions"
        );
    }

    #[test]
    fn boundary_no_legacy_session_store_patterns() {
        let source = include_str!("session.rs");

        // 1. No `use` import of SqliteCasSessionStore or SessionStore trait.
        for line in source.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with("use ") {
                assert!(
                    !trimmed.contains("SqliteCasSessionStore"),
                    "legacy import found: {}",
                    trimmed
                );
                if trimmed.contains("SessionStore")
                    && !trimmed.contains("SessionGraphStore")
                    && !trimmed.contains("SqliteSessionGraphStore")
                {
                    panic!("legacy SessionStore trait import found: {}", trimmed);
                }
            }
        }

        // 2. The production path uses SessionApi (positive proof of migration).
        assert!(
            source.contains("SessionApi::sqlite(default_session_dir())"),
            "session_api() should construct SessionApi"
        );
        assert!(
            source.contains("fn run_with_api(api: &SessionApi,"),
            "run_with_api should accept &SessionApi"
        );
        assert!(
            source.contains("run_with_api(&api, action, json)"),
            "run() should delegate to run_with_api"
        );
    }
}

// ─── project_branch persistence and output tests (36-15 Task 1 RED) ──────

#[cfg(test)]
mod project_branch_tests {
    use lattice_api::SessionApi;

    fn temp_api(name: &str) -> SessionApi {
        let dir = std::env::temp_dir()
            .join("lattice-cli-project-branch-tests")
            .join(name);
        let _ = std::fs::remove_dir_all(&dir);
        SessionApi::sqlite(dir)
    }

    #[test]
    fn project_branch_creation_json_output_includes_branch() {
        let api = temp_api("json-output");
        // Seed a session with a known project_branch via the store.
        // The cmd_new_api function will be updated to accept project_branch.
        // For now, this test verifies the expected behavior after implementation.
        let created = api
            .create_session(lattice_api::CreateSessionRequest {
                label: None,
                active_agent: "coder".to_string(),
                project_branch: "feature/test".to_string(),
            })
            .expect("create session");
        assert_eq!(
            created.project_target_branch.as_deref(),
            Some("feature/test"),
            "created session should carry project_branch"
        );
    }

    #[test]
    fn project_branch_status_output_includes_target_branch() {
        let api = temp_api("status-output");
        let created = api
            .create_session(lattice_api::CreateSessionRequest {
                label: Some("branch test".to_string()),
                active_agent: "coder".to_string(),
                project_branch: "develop".to_string(),
            })
            .expect("create session");

        let status = api.status(&created.session_id).expect("status");
        assert_eq!(
            status.project_target_branch.as_deref(),
            Some("develop"),
            "status should expose project_target_branch"
        );
    }

    #[test]
    fn project_branch_list_output_includes_target_branch() {
        let api = temp_api("list-output");
        api.create_session(lattice_api::CreateSessionRequest {
            label: Some("list test".to_string()),
            active_agent: "coder".to_string(),
            project_branch: "release/v1".to_string(),
        })
        .expect("create session");

        let sessions = api.list_sessions().expect("list");
        let found = sessions
            .iter()
            .find(|s| s.label.as_deref() == Some("list test"));
        assert!(found.is_some(), "should find the created session");
        assert_eq!(
            found.unwrap().project_target_branch.as_deref(),
            Some("release/v1"),
            "list output should expose project_target_branch"
        );
    }

    #[test]
    fn project_branch_persists_through_reopen() {
        let api = temp_api("reopen");
        let created = api
            .create_session(lattice_api::CreateSessionRequest {
                label: None,
                active_agent: "coder".to_string(),
                project_branch: "feature/persist".to_string(),
            })
            .expect("create session");

        // Reopen by ID
        let opened = api
            .open_session(lattice_api::OpenSessionRequest {
                id: created.session_id.clone(),
            })
            .expect("open session");
        assert_eq!(
            opened.project_target_branch.as_deref(),
            Some("feature/persist"),
            "reopened session should retain project_branch"
        );
    }
}
