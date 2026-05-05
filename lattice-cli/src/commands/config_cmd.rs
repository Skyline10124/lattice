use anyhow::{anyhow, Result};
use clap::Subcommand;
use colored::Colorize;
use sha2::{Digest, Sha256};
use std::path::PathBuf;

use crate::config::Config;

#[derive(Subcommand)]
pub enum ConfigAction {
    Init,
    Get {
        key: String,
    },
    Set {
        key: String,
        value: String,
    },
    #[command(about = "Compute and write SHA-256 hash for an agent TOML config")]
    Hash {
        path: String,
    },
}

pub fn run(action: ConfigAction) -> Result<()> {
    let config_path = dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("lattice")
        .join("config.toml");

    match action {
        ConfigAction::Init => {
            if let Some(parent) = config_path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let default = r#"[core]
default_model = "sonnet"
stream = true
save_sessions = true

[ui]
theme = "dark"
show_reasoning = true
"#;
            std::fs::write(&config_path, default)?;
            println!(
                "{} config initialized at {}",
                "\u{2713}".green(),
                config_path.display()
            );
        }
        ConfigAction::Get { key } => {
            let config = Config::load(Some(config_path.to_str().unwrap()))?;
            match key.as_str() {
                "core.default_model" => println!("{} = {}", key, config.core.default_model),
                "core.stream" => println!("{} = {}", key, config.core.stream),
                "core.save_sessions" => println!("{} = {}", key, config.core.save_sessions),
                "ui.theme" => println!("{} = {}", key, config.ui.theme),
                "ui.show_reasoning" => println!("{} = {}", key, config.ui.show_reasoning),
                _ => println!("{}: unknown key", key.red()),
            }
        }
        ConfigAction::Set { key, value } => {
            // Ensure parent dir exists
            if let Some(parent) = config_path.parent() {
                std::fs::create_dir_all(parent)?;
            }

            // Read existing config or start fresh
            let content = if config_path.exists() {
                std::fs::read_to_string(&config_path)?
            } else {
                String::new()
            };

            // Parse with toml_edit for comment-preserving editing
            let mut doc = content
                .parse::<toml_edit::DocumentMut>()
                .map_err(|e| anyhow!("Failed to parse config: {}", e))?;

            let item = config_value_for_key(&key, &value)?;

            // Navigate key path (e.g., "core.default_model" → doc["core"]["default_model"])
            let parts: Vec<&str> = key.split('.').collect();
            if parts.len() == 2 {
                // Get or create the table
                if !doc.contains_table(parts[0]) {
                    doc[parts[0]] = toml_edit::table();
                }
                doc[parts[0]][parts[1]] = item;
            } else {
                doc[&key] = item;
            }

            // Write back
            std::fs::write(&config_path, doc.to_string())?;
            println!("{} {} = {}", "\u{2713}".green(), key.bold(), value);
        }
        ConfigAction::Hash { path } => {
            let file_path = std::path::Path::new(&path);
            let content = std::fs::read_to_string(file_path)?;
            let digest = Sha256::digest(content.as_bytes());
            let hash_str = format!("{:x}", digest);
            let hash_path = file_path.with_extension("toml.sha256");
            std::fs::write(&hash_path, &hash_str)?;
            println!(
                "{} SHA-256 hash written to {}",
                "\u{2713}".green(),
                hash_path.display()
            );
            println!("{}", hash_str);
        }
    }

    Ok(())
}

fn config_value_for_key(key: &str, value: &str) -> Result<toml_edit::Item> {
    match key {
        "core.stream" | "core.save_sessions" | "ui.show_reasoning" => {
            let parsed = value.parse::<bool>().map_err(|_| {
                anyhow!(
                    "{} expects a boolean value (true or false), got '{}'",
                    key,
                    value
                )
            })?;
            Ok(toml_edit::value(parsed))
        }
        _ => Ok(toml_edit::value(value)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boolean_config_values_are_typed() -> Result<()> {
        let item = config_value_for_key("core.save_sessions", "false")?;
        assert_eq!(item.as_bool(), Some(false));
        Ok(())
    }

    #[test]
    fn invalid_boolean_config_values_error() {
        let err = config_value_for_key("core.save_sessions", "nope").unwrap_err();
        assert!(err.to_string().contains("expects a boolean"));
    }

    #[test]
    fn string_config_values_remain_strings() -> Result<()> {
        let item = config_value_for_key("core.default_model", "sonnet")?;
        assert_eq!(item.as_str(), Some("sonnet"));
        Ok(())
    }
}
