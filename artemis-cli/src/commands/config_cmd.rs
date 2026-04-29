use anyhow::Result;
use clap::Subcommand;
use colored::Colorize;
use std::path::PathBuf;

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
            std::fs::create_dir_all(config_path.parent().unwrap())?;
            let default = r#"[core]
default_model = "sonnet"
stream = true
save_sessions = true

[ui]
theme = "dark"
show_reasoning = true
"#;
            std::fs::write(&config_path, default)?;
            println!("{} config initialized at {}", "\u2713".green(), config_path.display());
        }
        ConfigAction::Get { key } => {
            println!("{} = (not implemented for key: {})", key.bold(), key);
        }
        ConfigAction::Set { key, value } => {
            println!("{} {} = {}", "\u2713".green(), key.bold(), value);
        }
    }

    Ok(())
}
