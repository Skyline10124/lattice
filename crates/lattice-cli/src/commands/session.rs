// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Session 命令通过 LocalClient 操作后台；本层仅处理配置、参数与呈现。
//! 找回、查询和恢复先于当前 profile/provider 校验；引用写操作不在前端执行。
use std::path::PathBuf;

use anyhow::Result;
use clap::Subcommand;
use colored::Colorize;
use lattice_api::{
    CreateSessionRequest, GraphHead,
    local::{LocalClient, LocalRewindResult, SessionWrite, SessionWriteResult},
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// Lattice config root (`$XDG_CONFIG_HOME` or the Linux config directory).
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

/// Directory holding CLI/TUI *configuration-adjacent* session files (the REPL
/// history file). Durable session state does not live here: since #143 the
/// session store is `$XDG_STATE_HOME/lattice/sessions.git`.
pub(crate) fn default_session_dir() -> PathBuf {
    lattice_config_dir().join("lattice").join("sessions")
}

/// The private-state layout every durable store is addressed through
/// (ADR 0005 §3).
pub(crate) fn state_layout() -> Result<lattice_session::StateLayout> {
    Ok(lattice_session::StateLayout::resolve()?)
}

/// The global session store path (`$XDG_STATE_HOME/lattice/sessions.git`).
pub(crate) fn session_store_path() -> Result<PathBuf> {
    Ok(state_layout()?
        .lattice_root()
        .join(lattice_session::git_graph::SESSIONS_REPO_NAME))
}

pub(crate) fn parse_turn_id(value: &str) -> Result<lattice_api::local::TurnId, String> {
    lattice_api::local::TurnId::parse(value).map_err(|_| "expected canonical TurnId UUID".into())
}

pub(crate) async fn local_client() -> Result<lattice_api::local::LocalClient> {
    let client = lattice_api::local::LocalClient::new(state_layout()?);
    client.ensure_running().await?;
    Ok(client)
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
        /// 提交前保留的规范 TurnId；不按 prompt 文本去重。
        #[arg(long, value_parser = parse_turn_id)]
        turn_id: Option<lattice_api::local::TurnId>,
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
    /// 按规范 TurnId 查询持久结果，可继续观察后续事件。
    Turn {
        session_id: String,
        #[arg(value_parser = parse_turn_id)]
        turn_id: lattice_api::local::TurnId,
        #[arg(long)]
        follow: bool,
    },
    /// 明确取消指定 Turn；确认接管不等于已持久 Cancelled。
    /// 未接管只描述后台的判定点，不表示没有后台工作；请另行查询原 Turn。
    Cancel {
        session_id: String,
        #[arg(value_parser = parse_turn_id)]
        turn_id: lattice_api::local::TurnId,
    },
    /// 独立关闭旧 pending，不请求模型或重新分发工具。
    Recover { session_id: String },
}

/// 全部 Session 命令经本地后台；前端只解析、冻结新请求配置和渲染。
pub async fn run(
    action: SessionAction,
    json: bool,
    verbose: bool,
) -> Result<std::process::ExitCode> {
    let api = local_client().await?;
    match action {
        SessionAction::Send {
            session_id,
            prompt,
            turn_id,
        } => {
            return crate::commands::run::run_session_prompt(
                &api, prompt, session_id, turn_id, verbose, json,
            )
            .await;
        }
        SessionAction::Turn {
            session_id,
            turn_id,
            follow,
        } => {
            let session = api.open_session(&session_id).await?;
            return crate::commands::run::observe_turn(
                &api,
                &session.session_id,
                turn_id,
                follow,
                verbose,
                json,
            )
            .await;
        }
        SessionAction::Cancel {
            session_id,
            turn_id,
        } => {
            let session = api.open_session(&session_id).await?;
            let owned = api.cancel(&session.session_id, turn_id).await?;
            println!(
                "{}",
                serde_json::json!({"type": if owned { "cancel_acknowledged" } else { "cancel_not_owned" }, "session_id":session.session_id, "turn_id":turn_id, "owned":owned})
            );
            if !json {
                eprintln!(
                    "{}",
                    if owned {
                        "后台已接管当前操作的取消；持久终态仍待查询"
                    } else {
                        "后台在判定点未接管本次取消；不代表没有后台工作，请查询原 Turn"
                    }
                );
            }
        }
        SessionAction::Recover { session_id } => {
            let session = api.open_session(&session_id).await?;
            let view = api.recover(&session.session_id).await?;
            crate::commands::run::render_view(&api, &view, json).await?;
        }
        SessionAction::New {
            agent,
            project_branch,
            label,
        } => cmd_new_api(&api, &agent, &project_branch, label.as_deref(), json).await?,
        SessionAction::Agent { session_id, agent } => {
            cmd_agent_api(&api, &session_id, &agent, json).await?
        }
        SessionAction::Status { session_id } => {
            cmd_status_api(&api, session_id.as_deref(), json).await?
        }
        SessionAction::List => cmd_list_api(&api, json).await?,
        SessionAction::Log { session_id } => cmd_log_api(&api, &session_id, json).await?,
        SessionAction::Checkout { session_id, target } => {
            cmd_checkout_api(&api, &session_id, &target, json).await?
        }
        SessionAction::Branch {
            session_id,
            name,
            from,
        } => cmd_branch_api(&api, &session_id, name.as_deref(), from.as_deref(), json).await?,
        SessionAction::Diff {
            session_id,
            from,
            to,
        } => cmd_diff_api(&api, &session_id, &from, &to, json).await?,
        SessionAction::Reset { session_id, target } => {
            cmd_reset_api(&api, &session_id, &target, json).await?
        }
        SessionAction::Rewind { session_id, target } => {
            return cmd_rewind_api(&api, &session_id, &target, json).await;
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

async fn cmd_new_api(
    api: &LocalClient,
    agent: &str,
    project_branch: &str,
    label: Option<&str>,
    json: bool,
) -> Result<()> {
    let name = agent.to_owned();
    let key = tokio::task::spawn_blocking(move || -> Result<_> {
        load_agent_checked(&name)?;
        Ok(lattice_api::resolve_local_workspace_root()?.store_key_hex())
    })
    .await??;
    let session = api
        .create_session(CreateSessionRequest {
            label: label.map(str::to_string),
            active_agent: agent.to_string(),
            project_branch: project_branch.to_string(),
            project_store_key: key,
        })
        .await?;
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

async fn cmd_agent_api(api: &LocalClient, session_id: &str, agent: &str, json: bool) -> Result<()> {
    let name = agent.to_owned();
    tokio::task::spawn_blocking(move || load_agent_checked(&name)).await??;
    let id = api.open_session(session_id).await?.session_id;
    let SessionWriteResult::SetAgent(session) = api
        .write_session(&id, SessionWrite::SetAgent { name: agent.into() })
        .await?
    else {
        anyhow::bail!("unexpected session write response")
    };
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

async fn cmd_status_api(api: &LocalClient, session_id: Option<&str>, json: bool) -> Result<()> {
    match session_id {
        Some(id) => {
            let session = api.open_session(id).await?;
            let view = api.query(&session.session_id, None).await?;
            if json {
                println!(
                    "{}",
                    serde_json::json!({
                        "session_id": view.session.session_id, "head": view.head,
                        "current_commit_id": view.head_commit_id, "active_agent": view.session.active_agent_name,
                        "branches": view.branches, "commit_count": view.commit_count,
                        "project_target_branch": view.session.project_target_branch,
                        "active_turn": view.active_turn, "state_oid": view.state_oid,
                    })
                );
            } else {
                println!(
                    "Session: {}\n  Agent: {}\n  HEAD: {:?}\n  Commits: {}\n  Commit: {}\n  Active Turn: {}",
                    view.session.session_id,
                    view.session.active_agent_name,
                    view.head,
                    view.commit_count,
                    view.head_commit_id.as_deref().unwrap_or("empty"),
                    view.active_turn
                        .map(|turn| turn.turn_id.to_string())
                        .unwrap_or_else(|| "none".into())
                );
            }
        }
        None => {
            let sessions = api.list_sessions().await?;
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

async fn cmd_list_api(api: &LocalClient, json: bool) -> Result<()> {
    let sessions = api.list_sessions().await?;
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

async fn cmd_log_api(api: &LocalClient, session_id: &str, json: bool) -> Result<()> {
    let entries = api.log(session_id).await?;
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
        format!(
            "  {} {}",
            "outcome:".green(),
            serde_json::to_string(&entry.outcome).expect("serialize outcome")
        ),
        format!("  {} {}", "origin:".green(), entry.origin.as_str()),
        format!(
            "  {} {}",
            "workspace history:".green(),
            entry
                .workspace_history_checkpoint
                .as_oid()
                .unwrap_or("empty")
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

async fn cmd_checkout_api(
    api: &LocalClient,
    session_id: &str,
    target: &str,
    json: bool,
) -> Result<()> {
    let id = api.open_session(session_id).await?.session_id;
    let SessionWriteResult::Checkout(head) = api
        .write_session(
            &id,
            SessionWrite::Checkout {
                target: target.into(),
            },
        )
        .await?
    else {
        anyhow::bail!("unexpected session write response")
    };
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

async fn cmd_branch_api(
    api: &LocalClient,
    session_id: &str,
    name: Option<&str>,
    from: Option<&str>,
    json: bool,
) -> Result<()> {
    match name {
        Some(branch_name) => {
            let id = api.open_session(session_id).await?.session_id;
            let SessionWriteResult::Branch(branch_ref) = api
                .write_session(
                    &id,
                    SessionWrite::Branch {
                        name: branch_name.into(),
                        from: from.map(str::to_owned),
                    },
                )
                .await?
            else {
                anyhow::bail!("unexpected session write response")
            };
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
            let status = api.session_status(session_id).await?;
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

async fn cmd_diff_api(
    api: &LocalClient,
    session_id: &str,
    from: &str,
    to: &str,
    json: bool,
) -> Result<()> {
    let diff = api.diff(session_id, from, to).await?;
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

async fn cmd_reset_api(
    api: &LocalClient,
    session_id: &str,
    target: &str,
    json: bool,
) -> Result<()> {
    let id = api.open_session(session_id).await?.session_id;
    let SessionWriteResult::Reset(result) = api
        .write_session(
            &id,
            SessionWrite::Reset {
                target: target.into(),
            },
        )
        .await?
    else {
        anyhow::bail!("unexpected session write response")
    };
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

/// Rewind 的文件效果、引用与审计诊断原样保留，不把错误当成未发生变更。
async fn cmd_rewind_api(
    api: &LocalClient,
    session_id: &str,
    target: &str,
    json: bool,
) -> Result<std::process::ExitCode> {
    let id = api.open_session(session_id).await?.session_id;
    let root = tokio::task::spawn_blocking(lattice_api::resolve_local_workspace_root).await??;
    match api
        .rewind(&id, target, root.canonical_path().into())
        .await?
    {
        LocalRewindResult::AuditPublicationFailed(partial) => {
            if json {
                println!("{}", serde_json::to_string(&partial)?);
            } else {
                eprint!("{}", render_rewind_partial_text(&partial));
            }
            Ok(std::process::ExitCode::from(1))
        }
        LocalRewindResult::Success(success) => {
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
        LocalRewindResult::Conflict(conflict) => {
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

fn render_rewind_ref_observation(observation: &lattice_api::RewindRefObservation) -> String {
    match observation {
        lattice_api::RewindRefObservation::Present { value } => format!("present ({value})"),
        lattice_api::RewindRefObservation::Empty => "empty".to_string(),
        lattice_api::RewindRefObservation::Unavailable => "unavailable".to_string(),
    }
}

fn render_rewind_audit_observation(
    observation: lattice_api::RewindAuditRecordObservation,
) -> &'static str {
    match observation {
        lattice_api::RewindAuditRecordObservation::VisibleDurabilityUnconfirmed => {
            "visible (append durability unconfirmed)"
        }
        lattice_api::RewindAuditRecordObservation::NotVisible => "not visible",
        lattice_api::RewindAuditRecordObservation::Uncertain => "uncertain (outside bounded page)",
        lattice_api::RewindAuditRecordObservation::Unavailable => "unavailable",
    }
}

fn render_rewind_partial_text(partial: &lattice_api::RewindPartialState) -> String {
    format!(
        "partial state:\n  session_id: {}\n  target_turn_id: {}\n  target_commit_id: {}\n  \\
         outcome: {}\n  started_from_history_oid: {}\n  attempted_target_history_oid: {}\n  \\
         actual_workspace_history_oid: {}\n  actual_session_head_commit_id: {}\n  \\
         audit_record_observation: {}\n  steps: {}\n  preflight: {}\n  restored: {}\n  \\
         conflicts: {}\n",
        partial.session_id,
        partial.target_turn_id,
        partial.target_commit_id,
        partial.outcome,
        partial
            .started_from_history_oid
            .as_deref()
            .unwrap_or("<empty>"),
        partial
            .attempted_target_history_oid
            .as_deref()
            .unwrap_or("<empty>"),
        render_rewind_ref_observation(&partial.actual_workspace_history_oid),
        render_rewind_ref_observation(&partial.actual_session_head_commit_id),
        render_rewind_audit_observation(partial.audit_record_observation),
        serde_json::to_string(&partial.steps).unwrap_or_else(|_| "<unavailable>".to_string()),
        serde_json::to_string(&partial.preflight).unwrap_or_else(|_| "<unavailable>".to_string()),
        serde_json::to_string(&partial.restored).unwrap_or_else(|_| "<unavailable>".to_string()),
        serde_json::to_string(&partial.conflicts).unwrap_or_else(|_| "<unavailable>".to_string()),
    )
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
    fn cancel_help_limits_not_taken_to_the_decision_point() {
        use clap::CommandFactory;
        let mut command = TestCli::command();
        let cancel = command.find_subcommand_mut("cancel").unwrap();
        let help = cancel.render_long_help().to_string();
        assert!(help.contains("不表示没有后台工作"));
        assert!(help.contains("不等于已持久 Cancelled"));
        let id = "10000000-0000-4000-8000-000000000001";
        assert!(matches!(try_parse(&["cancel", "session-id", id]).unwrap(),
            SessionAction::Cancel { session_id, turn_id }
                if session_id == "session-id" && turn_id.to_string() == id));
        assert!(try_parse(&["cancel", "session-id", "invalid"]).is_err());
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
    fn human_turn_log_renders_typed_terminal_facts() {
        let entry = lattice_session::TurnLogEntry {
            commit_id: "abc123".to_string(),
            turn_id: lattice_session::TurnId::parse("aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa")
                .expect("valid test TurnId"),
            parent_ids: vec![],
            outcome: lattice_session::TurnOutcome::Completed {
                finish_reason: lattice_session::TurnFinishReason::Stop,
            },
            origin: lattice_session::ClosureOrigin::Live,
            workspace_history_checkpoint: lattice_session::WorkspaceHistoryCheckpoint::Empty,
            agent_name: "coder".to_string(),
            profile_snapshot_hash: "profile".to_string(),
            model: "test-model".to_string(),
            provider: "test-provider".to_string(),
            commit_message: "pending cleanup".to_string(),
            message_count: 2,
            tool_message_count: 0,
            usage: None,
            created_at: "2026-06-22T00:00:00Z".to_string(),
        };

        let rendered = render_turn_log_entry(0, &entry);
        assert!(rendered.contains("outcome:"));
        assert!(rendered.contains("completed"));
        assert!(rendered.contains("origin:"));
        assert!(rendered.contains("live"));
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
    fn rewind_partial_text_renders_all_typed_observations_and_ids() {
        let partial: lattice_api::RewindPartialState = serde_json::from_value(serde_json::json!({
            "session_id": "0123456789abcdef0123456789abcdef",
            "target_turn_id": "10000000-0000-4000-8000-000000000001",
            "target_commit_id": "target-commit",
            "outcome": "history_ref_failure",
            "started_from_history_oid": "started-history",
            "attempted_target_history_oid": "target-history",
            "actual_workspace_history_oid": {
                "state": "present",
                "value": "actual-history"
            },
            "actual_session_head_commit_id": { "state": "empty" },
            "audit_record_observation": { "state": "uncertain" },
            "steps": [],
            "preflight": [],
            "restored": [],
            "conflicts": []
        }))
        .expect("typed partial state");

        let rendered = render_rewind_partial_text(&partial);
        for expected in [
            "session_id: 0123456789abcdef0123456789abcdef",
            "target_turn_id: 10000000-0000-4000-8000-000000000001",
            "target_commit_id: target-commit",
            "outcome: history_ref_failure",
            "started_from_history_oid: started-history",
            "attempted_target_history_oid: target-history",
            "actual_workspace_history_oid: present (actual-history)",
            "actual_session_head_commit_id: empty",
            "audit_record_observation: uncertain (outside bounded page)",
            "steps: []",
            "preflight: []",
            "restored: []",
            "conflicts: []",
        ] {
            assert!(
                rendered.contains(expected),
                "missing {expected:?} in {rendered:?}"
            );
        }
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
    use lattice_session::StateLayout;

    fn test_store_key() -> String {
        lattice_session::ProjectStoreKey::from_canonical_path_bytes(b"/lattice-cli-tests/project")
            .to_string()
    }

    fn temp_api(name: &str) -> SessionApi {
        let dir = std::env::temp_dir()
            .join("lattice-cli-project-branch-tests")
            .join(name);
        let _ = std::fs::remove_dir_all(&dir);
        let layout = StateLayout::in_state_root(&dir.join("state"));
        SessionApi::git_create_with_layout(dir.join("sessions.git"), &layout).expect("create store")
    }

    #[test]
    fn project_branch_status_output_includes_target_branch() {
        let api = temp_api("status-output");
        let created = api
            .create_session(lattice_api::CreateSessionRequest {
                label: Some("branch test".to_string()),
                active_agent: "coder".to_string(),
                project_branch: "develop".to_string(),
                project_store_key: test_store_key(),
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
            project_store_key: test_store_key(),
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
