use anyhow::Result;
use colored::Colorize;

use super::agent_inventory::load_agent_inventory;

pub fn run() -> Result<()> {
    let dir = super::safe_agents_dir(None).map_err(anyhow::Error::msg)?;

    if !dir.exists() {
        println!("{}", "No agents found.".yellow());
        return Ok(());
    }

    let inventory = load_agent_inventory(&dir).map_err(|e| anyhow::anyhow!("{}", e))?;
    let mut agents = inventory.summaries();

    if agents.is_empty() {
        println!("{}", "No agents found.".yellow());
    }

    agents.sort_by(|a, b| a.name.cmp(&b.name));

    if !agents.is_empty() {
        println!(
            "{} {}",
            agents.len().to_string().cyan().bold(),
            "agent(s) found:".bold()
        );
        println!();
    }

    for profile in &agents {
        print!("  {}", profile.name.green().bold());
        if !profile.model.is_empty() {
            print!("  ({})", profile.model.cyan());
        }
        print!("  [v1]");
        println!();
    }

    if !inventory.failures().is_empty() {
        println!();
        println!(
            "{} {}",
            inventory.failures().len().to_string().yellow().bold(),
            "agent profile(s) failed to load:".yellow()
        );
        for failed in inventory.failures() {
            println!("  {}: {}", failed.path.display(), failed.error);
        }
    }

    Ok(())
}
