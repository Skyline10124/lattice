use crate::session::{session_manager, SessionStore};
use anyhow::Result;
use clap::Subcommand;
use colored::Colorize;

#[derive(Subcommand)]
pub enum SessionAction {
    List,
    Show {
        id: String,
    },
    Current,
    Checkout {
        target: String,
    },
    Branch {
        name: String,
        #[arg(long, help = "Branch from a session id or existing branch")]
        from: Option<String>,
    },
    Branches,
    Log {
        #[arg(help = "Session id or prefix. Defaults to current session.")]
        id: Option<String>,
    },
    Reflog {
        #[arg(help = "HEAD, branch, tag, or ref name. Defaults to HEAD.")]
        target: Option<String>,
    },
    Reset {
        target: String,
    },
    Tag {
        name: String,
        #[arg(help = "Session id, branch, tag, or commit id. Defaults to current session.")]
        target: Option<String>,
    },
    Tags,
    Diff {
        from: String,
        to: String,
    },
    CherryPick {
        source: String,
        #[arg(long = "turn", required = true, help = "Turn number to pick")]
        turns: Vec<usize>,
    },
    Merge {
        target: String,
    },
    Rebase {
        target: String,
    },
    Stash {
        #[command(subcommand)]
        action: StashAction,
    },
    Artifacts {
        #[command(subcommand)]
        action: ArtifactAction,
    },
    Manifests {
        #[command(subcommand)]
        action: ManifestAction,
    },
    Gc {
        #[arg(long, help = "Actually prune unreachable commit objects")]
        prune: bool,
    },
    Migrate,
    Rm {
        id: String,
    },
}

#[derive(Subcommand)]
pub enum StashAction {
    Push {
        #[arg(short, long)]
        message: Option<String>,
    },
    List,
    Apply {
        id: String,
    },
}

#[derive(Subcommand)]
pub enum ArtifactAction {
    List,
    Show { id: String },
}

#[derive(Subcommand)]
pub enum ManifestAction {
    List {
        #[arg(help = "Session id or prefix. Defaults to current session.")]
        session: Option<String>,
    },
    Show {
        id: String,
        #[arg(long, help = "Emit raw manifest JSON")]
        json: bool,
    },
}

pub fn run(action: SessionAction) -> Result<()> {
    let manager = session_manager();

    match action {
        SessionAction::List => {
            let sessions = manager.list()?;
            if sessions.is_empty() {
                println!("{}", "No sessions found.".yellow());
                return Ok(());
            }
            println!("{}", "Sessions:".bold());
            println!();
            for s in &sessions {
                let current = if s.is_current { "*" } else { " " };
                let branch_label = if s.branches.is_empty() {
                    s.branch.as_deref().unwrap_or("").to_string()
                } else {
                    s.branches.join(",")
                };
                let parent = s
                    .parent_id
                    .as_ref()
                    .map(|id| format!(" parent:{}", display_id(id)))
                    .unwrap_or_default();
                println!(
                    "{} {}  {}  {}  {}  {} messages{}",
                    current.green(),
                    display_id(&s.id).bold(),
                    s.model.cyan(),
                    s.provider.dimmed(),
                    s.updated_at.dimmed(),
                    s.message_count,
                    parent.dimmed()
                );
                if !branch_label.is_empty() {
                    println!("      {}", format!("branch: {branch_label}").yellow());
                }
                if let Some(title) = &s.title {
                    println!("      {}", title.dimmed());
                }
            }
        }
        SessionAction::Show { id } => match manager.load(&id)? {
            None => println!("{} Session '{}' not found.", "\u{2717}".red(), id),
            Some(session) => {
                println!(
                    "{}",
                    format!(
                        "Session: {} ({})",
                        display_id(&session.id).bold(),
                        session.model
                    )
                    .cyan()
                );
                println!("{}", format!("Provider: {}", session.provider).dimmed());
                println!("{}", format!("Created: {}", session.created_at).dimmed());
                println!("{}", format!("Updated: {}", session.updated_at).dimmed());
                if let Some(parent) = &session.parent_id {
                    println!("{}", format!("Parent: {}", display_id(parent)).dimmed());
                }
                if let Some(branch) = &session.branch {
                    println!("{}", format!("Branch: {}", branch).dimmed());
                }
                if let Some(title) = &session.title {
                    println!("{}", format!("Title: {}", title).dimmed());
                }
                println!();
                for msg in &session.messages {
                    let role_label = match msg.role.to_lowercase().as_str() {
                        "user" => "You".green(),
                        "assistant" => "AI".cyan(),
                        _ => msg.role.dimmed(),
                    };
                    println!("{}: {}", role_label, redacted_content_summary(&msg.content));
                    println!();
                }
            }
        },
        SessionAction::Current => match manager.current()? {
            None => println!(
                "{}",
                "No current session. Run a prompt or checkout a session.".yellow()
            ),
            Some(session) => {
                println!(
                    "{} {}",
                    "Current session:".bold(),
                    display_id(&session.id).cyan()
                );
                if let Some(branch) = manager
                    .branches()?
                    .into_iter()
                    .find(|b| b.is_current)
                    .map(|b| b.name)
                {
                    println!("{}", format!("Branch: {branch}").yellow());
                }
                println!(
                    "{}",
                    format!("Model: {} @ {}", session.model, session.provider).dimmed()
                );
                println!("{}", format!("Updated: {}", session.updated_at).dimmed());
                if let Some(title) = &session.title {
                    println!("{}", title.dimmed());
                }
            }
        },
        SessionAction::Checkout { target } => {
            let session = manager.checkout(&target)?;
            println!(
                "{} Checked out {}.",
                "\u{2713}".green(),
                display_id(&session.id).bold()
            );
            if let Some(title) = &session.title {
                println!("  {}", title.dimmed());
            }
        }
        SessionAction::Branch { name, from } => {
            let session = manager.create_branch(&name, from.as_deref())?;
            println!(
                "{} Created branch '{}' at session {}.",
                "\u{2713}".green(),
                name.bold(),
                display_id(&session.id).cyan()
            );
        }
        SessionAction::Branches => {
            let branches = manager.branches()?;
            if branches.is_empty() {
                println!("{}", "No branches found.".yellow());
                return Ok(());
            }
            for branch in branches {
                let current = if branch.is_current { "*" } else { " " };
                println!(
                    "{} {}  {}  {}",
                    current.green(),
                    branch.name.bold(),
                    display_id(&branch.session_id).cyan(),
                    branch.updated_at.unwrap_or_default().dimmed(),
                );
                if let Some(title) = branch.title {
                    println!("      {}", title.dimmed());
                }
            }
        }
        SessionAction::Log { id } => {
            let turns = manager.log(id.as_deref())?;
            if turns.is_empty() {
                println!("{}", "No turns found.".yellow());
                return Ok(());
            }
            for turn in turns {
                println!("{}", format!("turn {}", turn.turn).bold());
                println!("  {} {}", "user:".green(), summarize(&turn.user, 120));
                println!("  {} {}", "ai:".cyan(), summarize(&turn.assistant, 120));
                println!();
            }
        }
        SessionAction::Reflog { target } => {
            let entries = manager.reflog(target.as_deref())?;
            if entries.is_empty() {
                println!("{}", "No reflog entries found.".yellow());
                return Ok(());
            }
            for entry in entries {
                println!(
                    "{} {} -> {}  {}  {}",
                    entry.timestamp.dimmed(),
                    display_optional_id(entry.old_session_id.as_deref()).dimmed(),
                    display_optional_id(entry.new_session_id.as_deref()).cyan(),
                    entry.ref_name.yellow(),
                    entry.action
                );
            }
        }
        SessionAction::Reset { target } => {
            let session = manager.reset(&target)?;
            println!(
                "{} Reset HEAD to {}.",
                "\u{2713}".green(),
                display_id(&session.id).cyan()
            );
        }
        SessionAction::Tag { name, target } => {
            let tag = manager.create_tag(&name, target.as_deref())?;
            println!(
                "{} Created tag '{}' at {}.",
                "\u{2713}".green(),
                tag.name.bold(),
                display_id(&tag.session_id).cyan()
            );
        }
        SessionAction::Tags => {
            let tags = manager.tags()?;
            if tags.is_empty() {
                println!("{}", "No tags found.".yellow());
                return Ok(());
            }
            for tag in tags {
                println!(
                    "{}  {}  {}",
                    tag.name.bold(),
                    display_id(&tag.session_id).cyan(),
                    tag.updated_at.unwrap_or_default().dimmed()
                );
                if let Some(title) = tag.title {
                    println!("      {}", title.dimmed());
                }
            }
        }
        SessionAction::Diff { from, to } => {
            let diff = manager.diff(&from, &to)?;
            println!(
                "{} -> {}",
                display_id(&diff.from).cyan(),
                display_id(&diff.to).cyan()
            );
            println!("messages: [redacted]");
            for msg in diff.removed_messages.iter().take(8) {
                println!(
                    "{} {}: {}",
                    "-".red(),
                    msg.role.dimmed(),
                    redacted_content_summary(&msg.content)
                );
            }
            for msg in diff.added_messages.iter().take(8) {
                println!(
                    "{} {}: {}",
                    "+".green(),
                    msg.role.dimmed(),
                    redacted_content_summary(&msg.content)
                );
            }
        }
        SessionAction::CherryPick { source, turns } => {
            let session = manager.cherry_pick(&source, &turns)?;
            println!(
                "{} Cherry-picked selected turn(s) into {}.",
                "\u{2713}".green(),
                display_id(&session.id).cyan()
            );
        }
        SessionAction::Merge { target } => {
            let result = manager.merge(&target)?;
            println!(
                "{} Merged selected message(s) into {}.",
                "\u{2713}".green(),
                display_id(&result.session.id).cyan()
            );
            if !result.conflicts.is_empty() {
                println!(
                    "{}",
                    format!("{} conflict(s)", result.conflicts.len()).yellow()
                );
            }
        }
        SessionAction::Rebase { target } => {
            let session = manager.rebase_onto(&target)?;
            println!(
                "{} Rebased onto {} as {}.",
                "\u{2713}".green(),
                target,
                display_id(&session.id).cyan()
            );
        }
        SessionAction::Stash { action } => match action {
            StashAction::Push { message } => {
                let entry = manager.stash_push(message.as_deref())?;
                println!(
                    "{} Stashed {} as {}.",
                    "\u{2713}".green(),
                    display_id(&entry.session_id).cyan(),
                    entry.id.bold()
                );
            }
            StashAction::List => {
                let entries = manager.stash_list()?;
                if entries.is_empty() {
                    println!("{}", "No stashes found.".yellow());
                    return Ok(());
                }
                for entry in entries {
                    println!(
                        "{}  {}  {} messages  {}",
                        entry.id.bold(),
                        display_id(&entry.session_id).cyan(),
                        entry.message_count,
                        entry.created_at.dimmed()
                    );
                    if let Some(message) = entry.message {
                        println!("      {}", message.dimmed());
                    }
                }
            }
            StashAction::Apply { id } => {
                let session = manager.stash_apply(&id)?;
                println!(
                    "{} Applied stash at {}.",
                    "\u{2713}".green(),
                    display_id(&session.id).cyan()
                );
            }
        },
        SessionAction::Artifacts { action } => match action {
            ArtifactAction::List => {
                let artifacts = manager.list_artifacts()?;
                if artifacts.is_empty() {
                    println!("{}", "No artifacts found.".yellow());
                    return Ok(());
                }
                for artifact in artifacts {
                    let path = artifact.path.as_deref().unwrap_or("-");
                    let mime = artifact.mime.as_deref().unwrap_or("-");
                    println!(
                        "{}  {}  {} bytes  {}  {}  {}",
                        artifact.id.bold(),
                        artifact.kind.as_str().cyan(),
                        artifact.size,
                        path,
                        mime.dimmed(),
                        artifact.created_at.dimmed()
                    );
                }
            }
            ArtifactAction::Show { id } => match manager.get_artifact(&id)? {
                None => println!("{} Artifact '{}' not found.", "\u{2717}".red(), id),
                Some(artifact) => {
                    println!("{}", format!("Artifact: {}", artifact.id.bold()).cyan());
                    println!("{}", format!("Kind: {}", artifact.kind.as_str()).dimmed());
                    println!(
                        "{}",
                        format!("Content hash: {}", artifact.content_hash).dimmed()
                    );
                    println!("{}", format!("Size: {} bytes", artifact.size).dimmed());
                    println!("{}", format!("Created: {}", artifact.created_at).dimmed());
                    if let Some(path) = artifact.path.as_deref() {
                        println!("{}", format!("Path: {path}").dimmed());
                    }
                    if let Some(mime) = artifact.mime.as_deref() {
                        println!("{}", format!("MIME: {mime}").dimmed());
                    }
                    if !artifact.metadata.is_null() {
                        println!("{}", "Metadata:".bold());
                        println!("{}", serde_json::to_string_pretty(&artifact.metadata)?);
                    }
                    println!();
                    println!("{}", redacted_content_summary(&artifact.content));
                }
            },
        },
        SessionAction::Manifests { action } => match action {
            ManifestAction::List { session } => {
                let session_id = match session {
                    Some(target) => manager.resolve_session_id(&target)?,
                    None => match manager.current_session_id()? {
                        Some(id) => id,
                        None => {
                            println!("{}", "No current session.".yellow());
                            return Ok(());
                        }
                    },
                };
                let manifests = manager.list_turn_manifests(&session_id)?;
                if manifests.is_empty() {
                    println!("{}", "No turn manifests found.".yellow());
                    return Ok(());
                }
                for manifest in manifests {
                    println!(
                        "{}  turn {}  {}  {}",
                        manifest.id.bold(),
                        manifest.turn_index,
                        display_id(&manifest.request_id).cyan(),
                        manifest.created_at.dimmed()
                    );
                }
            }
            ManifestAction::Show { id, json } => match manager.get_turn_manifest(&id)? {
                None => println!("{} Manifest '{}' not found.", "\u{2717}".red(), id),
                Some(manifest) => {
                    if json {
                        println!("{}", serde_json::to_string_pretty(&manifest)?);
                    } else {
                        print_manifest(&manifest);
                    }
                }
            },
        },
        SessionAction::Gc { prune } => {
            let report = manager.gc(prune)?;
            println!(
                "reachable sessions: {}  unreachable sessions: {}  reachable commits: {}  reachable messages: {}  reachable artifacts: {}  pruned objects: {}{}",
                count_label(report.reachable_sessions),
                count_label(report.unreachable_sessions),
                count_label(report.reachable_commits),
                count_label(report.reachable_messages),
                count_label(report.reachable_artifacts),
                count_label(report.pruned_objects),
                if report.dry_run { "  (dry-run)" } else { "" }
            );
        }
        SessionAction::Migrate => {
            let report = manager.migrate_legacy_sessions()?;
            println!(
                "{} Migrated {} / {} session(s).",
                "\u{2713}".green(),
                count_label(report.migrated_sessions),
                count_label(report.scanned_sessions)
            );
        }
        SessionAction::Rm { id } => match manager.delete(&id)? {
            true => println!("{} Session '{}' deleted.", "\u{2713}".green(), id),
            false => println!("{} Session '{}' not found.", "\u{2717}".red(), id),
        },
    }
    Ok(())
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
        out.push('…');
        out
    }
}

fn display_id(_id: &str) -> String {
    "[redacted]".to_string()
}

fn display_optional_id(id: Option<&str>) -> String {
    id.map(display_id).unwrap_or_else(|| "-".to_string())
}

fn redacted_content_summary(text: &str) -> String {
    format!("[redacted; {} chars]", text.chars().count())
}

fn count_label(_count: usize) -> String {
    "[redacted]".to_string()
}

fn print_manifest(manifest: &lattice_session::TurnManifestObject) {
    println!(
        "{}",
        format!("Manifest: {}", display_id(&manifest.id).bold()).cyan()
    );
    println!(
        "{}",
        format!("Session: {}", display_id(&manifest.session_id)).dimmed()
    );
    if let Some(commit_id) = manifest.commit_id.as_deref() {
        println!("{}", format!("Commit: {}", display_id(commit_id)).dimmed());
    }
    println!("{}", format!("Turn: {}", manifest.turn_index).dimmed());
    println!(
        "{}",
        format!("Request: {}", display_id(&manifest.request_id)).dimmed()
    );
    println!(
        "{}",
        format!("Model: {} @ {}", manifest.model, manifest.provider).dimmed()
    );
    println!("{}", format!("Created: {}", manifest.created_at).dimmed());
    println!(
        "{}",
        format!(
            "Prompt: {} tokens / {} budget",
            manifest.prompt.total_tokens, manifest.prompt.effective_context_budget
        )
        .dimmed()
    );

    if !manifest.prompt.rendered_fragments.is_empty() {
        println!();
        println!("{}", "Rendered fragments: [redacted]".bold());
    }

    if !manifest.prompt.withheld_fragments.is_empty() {
        println!();
        println!("{}", "Withheld fragments: [redacted]".bold());
    }

    if !manifest.hooks.is_empty() {
        println!();
        println!("{}", "Hook decisions: [redacted]".bold());
    }

    if !manifest.tools.is_empty() {
        println!();
        println!("{}", "Tool records:".bold());
        for tool in &manifest.tools {
            println!(
                "  {}  {}  command={}  result={}",
                tool.tool_name.bold(),
                tool.decision_kind.cyan(),
                tool.command_hash.as_deref().unwrap_or("-").dimmed(),
                tool.result_hash.as_deref().unwrap_or("-").dimmed()
            );
        }
    }
}
