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
            println!("(Session persistence not yet implemented in MVP)");
        }
        SessionAction::Rm { id } => {
            println!(
                "{} Session removal not yet implemented ({})",
                "\u{2717}".red(),
                id
            );
        }
    }
    Ok(())
}
