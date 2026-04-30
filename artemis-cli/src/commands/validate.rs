use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::{bail, Result};
use artemis_harness::AgentRegistry;

/// Validate all agent profiles in the default agents directory or a given path.
pub fn run(dir: Option<String>) -> Result<()> {
    let path = match dir {
        Some(ref d) => Path::new(d).to_path_buf(),
        None => default_agents_dir(),
    };

    if !path.exists() {
        println!("Directory '{}' does not exist. Nothing to validate.", path.display());
        return Ok(());
    }

    let registry = AgentRegistry::load_dir(&path)
        .map_err(|e| anyhow::anyhow!("{}", e))?;
    let profile_count = registry.list().len();

    if profile_count == 0 {
        println!("No agent profiles found in '{}'", path.display());
        return Ok(());
    }

    println!("Found {} agent(s) in '{}'\n", profile_count, path.display());

    let mut errors = 0u32;
    let mut agent_names = HashSet::new();

    for profile in registry.list() {
        let name = &profile.agent.name;
        println!("  [check] {}", name);

        if !agent_names.insert(name.clone()) {
            println!("    ERROR: duplicate agent name '{}'", name);
            errors += 1;
        }

        match artemis_core::resolve(&profile.agent.model) {
            Ok(_) => println!("    model: {} OK", profile.agent.model),
            Err(e) => {
                println!("    model: {} — WARNING: {}", profile.agent.model, e);
            }
        }

        for (i, rule) in profile.handoff.handoff_rules.iter().enumerate() {
            if let Some(ref target) = rule.target {
                if !registry.get(target).is_some() {
                    println!(
                        "    rule[{}]: target '{}' is not a registered agent",
                        i, target
                    );
                    errors += 1;
                }
            }
        }

        if let Some(ref fallback) = profile.handoff.fallback {
            if !registry.get(fallback).is_some() {
                println!("    fallback: '{}' is not a registered agent", fallback);
                errors += 1;
            }
        }
    }

    // Detect circular handoff chains
    for profile in registry.list() {
        let mut visited = HashSet::new();
        let mut current = profile.agent.name.clone();
        visited.insert(current.clone());

        loop {
            let next = match registry.get(&current) {
                Some(p) => {
                    if p.handoff.handoff_rules.is_empty() {
                        p.handoff.fallback.clone()
                    } else {
                        // Check first matching rule (simplified — assumes condition met)
                        p.handoff.handoff_rules.first().and_then(|r| r.target.clone())
                            .or_else(|| p.handoff.fallback.clone())
                    }
                }
                None => break,
            };

            match next {
                Some(ref n) => {
                    if n == &profile.agent.name || !visited.insert(n.clone()) {
                        println!(
                            "    ERROR: circular handoff detected — '{}' → ... → '{}'",
                            profile.agent.name, n
                        );
                        errors += 1;
                        break;
                    }
                    current = n.clone();
                }
                None => break,
            }
        }
    }

    if errors > 0 {
        bail!("{} validation error(s) found", errors);
    }

    println!("\nAll agents valid.");
    Ok(())
}

fn default_agents_dir() -> std::path::PathBuf {
    if let Ok(dir) = std::env::var("ARTEMIS_AGENTS_DIR") {
        Path::new(&dir).to_path_buf()
    } else if let Ok(home) = std::env::var("HOME") {
        Path::new(&home).join(".artemis").join("agents")
    } else {
        PathBuf::from(".artemis/agents")
    }
}
