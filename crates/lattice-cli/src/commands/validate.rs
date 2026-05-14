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

    let registry = inventory.legacy_registry();
    let runtime = Runtime::builder()
        .name("validate")
        .agent_registry(registry.clone())
        .build();
    let mut agent_names = HashSet::new();

    for profile in registry.list() {
        let name = &profile.agent.name;
        println!("  [check] {}", name);

        if !agent_names.insert(name.clone()) {
            println!("    ERROR: duplicate agent name '{}'", name);
            errors += 1;
        }

        match runtime.resolve(&profile.agent.model) {
            Ok(_) => println!("    model: {} OK", profile.agent.model),
            Err(e) => {
                println!("    model: {} — WARNING: {}", profile.agent.model, e);
            }
        }

        for (i, rule) in profile.handoff.handoff_rules.iter().enumerate() {
            if let Some(ref target) = rule.target {
                for name in target.agent_names() {
                    if registry.get(name).is_none() {
                        println!(
                            "    rule[{}]: target '{}' is not a registered agent",
                            i, name
                        );
                        errors += 1;
                    }
                }
            }
        }

        if let Some(ref fallback) = profile.handoff.fallback {
            for name in fallback.agent_names() {
                if registry.get(name).is_none() {
                    println!("    fallback: '{}' is not a registered agent", name);
                    errors += 1;
                }
            }
        }
    }

    for loaded in inventory.v1_profiles() {
        let name = &loaded.profile.agent.name;
        println!("  [check] {} [v1]", name);

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

    // Detect circular handoff chains via Runtime pipeline validation.
    for profile in registry.list() {
        let report = runtime.dry_run_pipeline(&profile.agent.name);
        if report.circular {
            println!(
                "    ERROR: circular handoff detected starting from '{}'",
                profile.agent.name
            );
            errors += 1;
        }
        for issue in &report.issues {
            if issue.contains("not found") || issue.contains("unregistered") {
                println!("    ERROR: {}", issue);
                errors += 1;
            }
        }
    }

    if errors > 0 {
        bail!("{} validation error(s) found", errors);
    }

    println!("\nAll agents valid.");
    Ok(())
}
