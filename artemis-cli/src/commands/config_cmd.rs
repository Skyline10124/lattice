use anyhow::Result;
use clap::Subcommand;
use colored::Colorize;
use std::path::PathBuf;

use crate::config::Config;

#[derive(Subcommand)]
pub enum ConfigAction {
    Init,
    Get { key: String },
    Set { key: String, value: String },
}

pub fn run(action: ConfigAction) -> Result<()> {
    let config_path = dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("artemis")
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
    }

    Ok(())
}
