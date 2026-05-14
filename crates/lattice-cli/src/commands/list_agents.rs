use anyhow::Result;
use colored::Colorize;
use lattice_orchestrator::AgentRegistry;

pub fn run() -> Result<()> {
    let dir = super::safe_agents_dir(None).map_err(anyhow::Error::msg)?;

    if !dir.exists() {
        println!("{}", "No agents found.".yellow());
        return Ok(());
    }

    let registry = AgentRegistry::load_dir(&dir).map_err(|e| anyhow::anyhow!("{}", e))?;
    let mut agents: Vec<_> = registry.list().into_iter().collect();

    if agents.is_empty() {
        println!("{}", "No agents found.".yellow());
        return Ok(());
    }

    agents.sort_by(|a, b| a.agent.name.cmp(&b.agent.name));

    println!(
        "{} {}",
        agents.len().to_string().cyan().bold(),
        "agent(s) found:".bold()
    );
    println!();

    for profile in &agents {
        print!("  {}", profile.agent.name.green().bold());
        if !profile.agent.model.is_empty() {
            print!("  ({})", profile.agent.model.cyan());
        }
        if profile.plugins.is_some() {
            print!("  [plugins]");
        }
        println!();
        if !profile.agent.tags.is_empty() {
            println!(
                "    tags: {}",
                profile
                    .agent
                    .tags
                    .iter()
                    .map(|t| t.dimmed().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
    }

    if !registry.failed_loads().is_empty() {
        println!();
        println!(
            "{} {}",
            registry.failed_loads().len().to_string().yellow().bold(),
            "agent profile(s) failed to load:".yellow()
        );
        for failed in registry.failed_loads() {
            println!("  {}: {}", failed.path.display(), failed.error);
        }
    }

    Ok(())
}
