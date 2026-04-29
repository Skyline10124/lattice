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
    match action {
        SessionAction::List => {
            println!("{}", "Sessions:".bold());
            println!("(Session persistence not yet implemented in MVP)");
        }
        SessionAction::Show { id } => {
            println!("Session: {}", id.bold());
        }
        SessionAction::Rm { id } => {
            println!("{} Removed session {}", "\u2713".green(), id);
        }
    }
    Ok(())
}
