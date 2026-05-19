//! Singular `session` command over the session graph store.
//!
//! Supports exactly: status, list, log, checkout, branch, diff, reset.
//! Does not import or call old session manager surfaces.

use std::path::PathBuf;

use anyhow::Result;
use clap::Subcommand;
use colored::Colorize;
use lattice_api::SessionApi;
use lattice_session::GraphHead;

pub(crate) fn default_session_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("lattice")
        .join("sessions")
}

fn session_api() -> SessionApi {
    SessionApi::sqlite(default_session_dir())
}

#[derive(Subcommand)]
pub enum SessionAction {
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
}

/// Run the `session` command.
pub fn run(action: SessionAction, json: bool) -> Result<()> {
    let api = session_api();
    run_with_api(&api, action, json)
}

/// Run the `session` command with an explicit [`SessionApi`] instance.
///
/// This is the testable seam: production `run()` constructs the default API
/// and delegates here. Tests pass a temp-backed `SessionApi` directly.
pub(crate) fn run_with_api(api: &SessionApi, action: SessionAction, json: bool) -> Result<()> {
    match action {
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
    }
    Ok(())
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
                    "branches": status.branches,
                    "commit_count": status.commit_count,
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
                    println!(
                        "  {} {} msgs  {} [{}]",
                        display_id(&s.session_id).cyan(),
                        s.commit_count,
                        label.dimmed(),
                        branch
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
            println!(
                "  {}  {} msgs  {} [{}] head={}",
                display_id(&s.session_id).cyan(),
                s.commit_count,
                label.dimmed(),
                branch,
                display_id(head)
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
            println!("{}", format!("turn {}", i).bold());
            println!("  {} {}", "commit:".green(), display_id(&entry.commit_id));
            println!("  {} {}", "status:".green(), entry.status.as_str());
            println!("  {} {}", "model:".green(), entry.model);
            println!("  {} {}", "provider:".green(), entry.provider);
            println!("  {} {}", "messages:".green(), entry.message_count);
            if entry.tool_message_count > 0 {
                println!("  {} {}", "tools:".green(), entry.tool_message_count);
            }
            println!(
                "  {} {}",
                "message:".green(),
                summarize(&entry.commit_message, 120)
            );
            println!();
        }
    }
    Ok(())
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
                        "{} {}  {}",
                        current.green(),
                        branch.name.bold(),
                        commit_label.cyan()
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

#[cfg(test)]
fn not_found(kind: &str, id: &str) -> anyhow::Error {
    anyhow::anyhow!("{} '{}' not found", kind, id)
}

fn display_id(id: &str) -> String {
    if id.len() > 12 {
        let end = id
            .char_indices()
            .nth(12)
            .map(|(i, _)| i)
            .unwrap_or(id.len());
        format!("{}..", &id[..end])
    } else {
        id.to_string()
    }
}

fn summarize(text: &str, max: usize) -> String {
    let one_line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if one_line.chars().count() <= max {
        one_line
    } else {
        let mut out = one_line
            .chars()
            .take(max.saturating_sub(1))
            .collect::<String>();
        out.push('\u{2026}');
        out
    }
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
            .create_session(session_id, label, None, Some("sonnet"), Some("anthropic"))
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
                profile_snapshot_id: Some("test-profile".to_string()),
                model: "sonnet".to_string(),
                provider: "anthropic".to_string(),
                status: TurnCommitStatus::Completed,
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
