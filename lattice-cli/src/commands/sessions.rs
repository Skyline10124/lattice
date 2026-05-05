use crate::session::SessionManager;
use anyhow::Result;
use clap::Subcommand;
use colored::Colorize;

#[derive(Subcommand)]
pub enum SessionAction {
    List,
    Show { id: String },
    Rm { id: String },
}

pub fn run(action: SessionAction) -> Result<()> {
    let manager = SessionManager::new();

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
                println!(
                    "  {}  {}  {}  {}  {} messages",
                    s.id.bold(),
                    s.model.cyan(),
                    s.provider.dimmed(),
                    s.updated_at.dimmed(),
                    s.message_count
                );
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
                    format!("Session: {} ({})", session.id.bold(), session.model).cyan()
                );
                println!("{}", format!("Provider: {}", session.provider).dimmed());
                println!("{}", format!("Created: {}", session.created_at).dimmed());
                println!("{}", format!("Updated: {}", session.updated_at).dimmed());
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
                    println!("{}: {}", role_label, msg.content);
                    println!();
                }
            }
        },
        SessionAction::Rm { id } => match manager.delete(&id)? {
            true => println!("{} Session '{}' deleted.", "\u{2713}".green(), id),
            false => println!("{} Session '{}' not found.", "\u{2717}".red(), id),
        },
    }
    Ok(())
}
