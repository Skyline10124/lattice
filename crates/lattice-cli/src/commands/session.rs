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

fn session_api() -> Result<SessionApi> {
    let git_repo_path = default_session_dir().join("sessions.git");
    Ok(SessionApi::git(git_repo_path))
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
    /// Rewind to an earlier Turn, reversing only LATTICE's own mutations.
    ///
    /// External changes LATTICE never touched are preserved. If a path
    /// LATTICE changed has since diverged, the whole rewind reports a
    /// conflict and nothing moves.
    Rewind {
        /// Session id (required).
        session_id: String,
        /// Turn commit id to rewind to.
        target: String,
    },
}

/// Run the `session` command.
pub async fn run(
    action: SessionAction,
    json: bool,
    verbose: bool,
) -> Result<std::process::ExitCode> {
    let api = session_api()?;
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
        other => run_with_api(&api, other, json),
    }
}

/// Run the `session` command with an explicit [`SessionApi`] instance.
///
/// This is the testable seam: production `run()` constructs the default API
/// and delegates here. Tests pass a temp-backed `SessionApi` directly.
pub(crate) fn run_with_api(
    api: &SessionApi,
    action: SessionAction,
    json: bool,
) -> Result<std::process::ExitCode> {
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
        SessionAction::Rewind { session_id, target } => {
            return cmd_rewind_api(api, &session_id, &target, json);
        }
    }
    Ok(std::process::ExitCode::SUCCESS)
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

/// Run an explicit Rewind (#64).
///
/// Exit codes follow the pinned CLI contract: a target that does not exist or
/// is unreachable is an input error (2); a structured same-path conflict is a
/// runtime outcome (1), reported in full so the user can resolve it.
fn cmd_rewind_api(
    api: &SessionApi,
    session_id: &str,
    target: &str,
    json: bool,
) -> Result<std::process::ExitCode> {
    let project_root = std::env::current_dir()?;
    let outcome = match lattice_api::rewind_to_turn(api, session_id, target, &project_root) {
        Ok(outcome) => outcome,
        Err(err @ (lattice_api::ApiError::NotFound(_) | lattice_api::ApiError::BadRequest(_))) => {
            eprintln!("{}: {:#}", "error".red(), err);
            return Ok(std::process::ExitCode::from(2));
        }
        Err(err) => return Err(err.into()),
    };

    match outcome {
        lattice_api::RewindOutcome::Success(success) => {
            if json {
                println!("{}", serde_json::to_string_pretty(&success)?);
            } else {
                println!(
                    "{} Rewound session {} to {} ({} mutation(s) reversed, {} path(s) restored).",
                    "\u{2713}".green(),
                    display_id(&success.session_id).cyan(),
                    display_id(&success.target_commit_id).bold(),
                    success.steps.len(),
                    success.restored.len(),
                );
                for path in &success.restored {
                    let verb = if path.restored_revision.is_some() {
                        "restored"
                    } else {
                        "removed"
                    };
                    println!("  {verb} {}", path.path);
                }
            }
            Ok(std::process::ExitCode::SUCCESS)
        }
        lattice_api::RewindOutcome::Conflict(conflict) => {
            if json {
                println!("{}", serde_json::to_string_pretty(&conflict)?);
            } else {
                eprintln!(
                    "{}: rewind to {} conflicts; nothing was changed.",
                    "conflict".red(),
                    display_id(&conflict.target_commit_id).bold(),
                );
                for path in &conflict.conflicts {
                    eprintln!("  {} — {}", path.path, path.detail);
                }
            }
            Ok(std::process::ExitCode::from(1))
        }
    }
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
    fn parse_rewind() {
        let action = try_parse(&["rewind", "sess-abc", "commit-xyz"]);
        assert!(action.is_ok());
        match action.unwrap() {
            SessionAction::Rewind { session_id, target } => {
                assert_eq!(session_id, "sess-abc");
                assert_eq!(target, "commit-xyz");
            }
            _ => panic!("expected Rewind"),
        }
    }

    #[test]
    fn parse_rewind_requires_session_and_target() {
        assert!(try_parse(&["rewind"]).is_err());
        assert!(try_parse(&["rewind", "sess-abc"]).is_err());
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
    fn reject_old_subcommand_rm() {
        assert!(try_parse(&["rm"]).is_err());
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

    // ─── project_branch parser tests (36-15 Task 1 RED) ───────────────────

    #[test]
    fn project_branch_new_without_required_flag_fails() {
        // session new must require --project-branch.
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
        SessionApi::git(dir)
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
}
