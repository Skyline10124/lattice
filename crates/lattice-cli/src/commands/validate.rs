use std::collections::HashSet;
use std::path::Path;

use anyhow::{bail, Result};
use lattice_runtime::Runtime;

use super::agent_inventory::load_agent_inventory;

/// Validate all agent profiles in the default agents directory or a given path.
pub fn run(dir: Option<String>) -> Result<()> {
    let path = match dir {
        Some(ref d) => Path::new(d).to_path_buf(),
        None => super::safe_agents_dir(None).map_err(anyhow::Error::msg)?,
    };

    if !path.exists() {
        println!(
            "Directory '{}' does not exist. Nothing to validate.",
            path.display()
        );
        return Ok(());
    }

    let inventory = load_agent_inventory(&path).map_err(|e| anyhow::anyhow!("{}", e))?;
    let profile_count = inventory.profile_count();
    let mut errors = 0u32;

    for failed in inventory.failures() {
        println!(
            "ERROR: failed to load '{}': {}",
            failed.path.display(),
            failed.error
        );
        errors += 1;
    }

    if profile_count == 0 {
        println!("No agent profiles found in '{}'", path.display());
        if errors > 0 {
            bail!("{} validation error(s) found", errors);
        }
        return Ok(());
    }

    println!("Found {} agent(s) in '{}'\n", profile_count, path.display());

    let runtime = Runtime::builder().build();
    let mut agent_names = HashSet::new();

    for loaded in inventory.v1_profiles() {
        let name = &loaded.profile.agent.name;
        println!("  [check] {}", name);

        if !agent_names.insert(name.clone()) {
            println!("    ERROR: duplicate agent name '{}'", name);
            errors += 1;
        }

        match runtime.resolve(&loaded.profile.agent.model) {
            Ok(_) => println!("    model: {} OK", loaded.profile.agent.model),
            Err(e) => {
                println!("    model: {} — WARNING: {}", loaded.profile.agent.model, e);
            }
        }
    }

    if errors > 0 {
        bail!("{} validation error(s) found", errors);
    }

    println!("\nAll v1 agents valid.");
    Ok(())
}
