use anyhow::Result;
use clap::Subcommand;
use colored::Colorize;
use sha2::{Digest, Sha256};
use std::path::PathBuf;

use crate::config::Config;

#[derive(Subcommand)]
pub enum ConfigAction {
    Init,
    Get { key: String },
    Set { key: String, value: String },
    #[command(about = "Compute and write SHA-256 hash for an agent TOML config")]
    Hash { path: String },
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
            println!(
                "{} {} = {} (not yet persisted — edit {} directly)",
                "\u{2713}".green(),
                key.bold(),
                value,
                config_path.display()
            );
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
