use std::path::{Path, PathBuf};

use anyhow::Result;
use colored::Colorize;

pub fn run() -> Result<()> {
    let dir = agents_dir();

    if !dir.exists() {
        println!("{}", "No agents found.".yellow());
        return Ok(());
    }

    let entries = std::fs::read_dir(&dir)?;
    let mut agents: Vec<(String, String, Vec<String>, bool)> = Vec::new();

    for entry in entries {
        let entry = entry?;
        if !entry.path().is_dir() {
            continue;
        }

        let agent_toml = entry.path().join("agent.toml");
        if !agent_toml.exists() {
            continue;
        }

        let content = match std::fs::read_to_string(&agent_toml) {
            Ok(c) => c,
            Err(_) => continue,
        };

        // Parse just the [agent] section — simple line-by-line parsing avoids
        // pulling in the full serde dependency + AgentProfile type from harness.
        // Stops parsing name/model/tags once it hits a non-[agent] section header,
        // so that `name = "..."` lines inside [[plugins.slots]] are not picked up.
        let mut name = String::new();
        let mut model = String::new();
        let mut tags: Vec<String> = Vec::new();
        let mut has_plugins = false;
        let mut in_agent_section = true;

        for line in content.lines() {
            if line.starts_with('[') && !line.starts_with("[agent]") {
                in_agent_section = false;
                if line.starts_with("[plugins]") || line.starts_with("[[plugins") {
                    has_plugins = true;
                }
            }

            if in_agent_section {
                if let Some(val) = line.strip_prefix("name = \"") {
                    if name.is_empty() {
                        if let Some(end) = val.find('"') {
                            name = val[..end].to_string();
                        }
                    }
                } else if let Some(val) = line.strip_prefix("model = \"") {
                    if model.is_empty() {
                        if let Some(end) = val.find('"') {
                            model = val[..end].to_string();
                        }
                    }
                } else if let Some(val) = line.strip_prefix("tags = [") {
                    if tags.is_empty() {
                        let inner = val.trim_end_matches(']');
                        for tag in inner.split(',') {
                            let tag = tag.trim().trim_matches('"').trim();
                            if !tag.is_empty() {
                                tags.push(tag.to_string());
                            }
                        }
                    }
                }
            } else if !has_plugins
                && (line.starts_with("[plugins]") || line.starts_with("[[plugins"))
            {
                has_plugins = true;
            }
        }

        // Fallback to directory name when [agent] section has no name field
        if name.is_empty() {
            name = entry
                .path()
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("unknown")
                .to_string();
        }

        agents.push((name, model, tags, has_plugins));
    }

    if agents.is_empty() {
        println!("{}", "No agents found.".yellow());
        return Ok(());
    }

    agents.sort_by(|a, b| a.0.cmp(&b.0));

    println!(
        "{} {}",
        agents.len().to_string().cyan().bold(),
        "agent(s) found:".bold()
    );
    println!();

    for (name, model, tags, has_plugins) in &agents {
        print!("  {}", name.green().bold());
        if !model.is_empty() {
            print!("  ({})", model.cyan());
        }
        if *has_plugins {
            print!("  [plugins]");
        }
        println!();
        if !tags.is_empty() {
            println!(
                "    tags: {}",
                tags.iter()
                    .map(|t| t.dimmed().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
    }

    Ok(())
}

fn agents_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("LATTICE_AGENTS_DIR") {
        Path::new(&dir).to_path_buf()
    } else if let Ok(home) = std::env::var("HOME") {
        Path::new(&home).join(".lattice").join("agents")
    } else {
        PathBuf::from(".lattice/agents")
    }
}
