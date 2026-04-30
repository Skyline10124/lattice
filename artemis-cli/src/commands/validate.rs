use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{bail, Result};
use artemis_harness::{AgentRegistry, Pipeline};

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

    // Detect circular handoff chains via Pipeline::dry_run
    for profile in registry.list() {
        let pipeline = Pipeline::new("validate", Arc::new(registry.clone()), None, None);
        let report = pipeline.dry_run(&profile.agent.name);
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

fn default_agents_dir() -> std::path::PathBuf {
    if let Ok(dir) = std::env::var("ARTEMIS_AGENTS_DIR") {
        Path::new(&dir).to_path_buf()
    } else if let Ok(home) = std::env::var("HOME") {
        Path::new(&home).join(".artemis").join("agents")
    } else {
        PathBuf::from(".artemis/agents")
    }
}
