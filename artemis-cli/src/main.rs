use anyhow::Result;
use clap::{Parser, Subcommand};
use colored::Colorize;

mod commands;
mod config;
mod credentials;
mod session;

use commands::{doctor, print, resolve, sessions, models, config_cmd, stats};
use config::Config;
use credentials::CredentialStore;

#[derive(Parser)]
#[command(name = "artemis")]
#[command(about = "󰚩 Artemis — model-centric LLM engine")]
#[command(version = "0.1.0")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    #[arg(short, long, help = "Single-turn print mode (pipe-friendly)")]
    print: Option<String>,

    #[arg(short, long, help = "Model alias or canonical ID")]
    model: Option<String>,

    #[arg(long, help = "Provider override")]
    provider: Option<String>,

    #[arg(short, long, help = "Continue last session")]
    continue_session: bool,

    #[arg(long, help = "Do not save session")]
    no_save: bool,

    #[arg(short, long, help = "Verbose output (show resolve trace)")]
    verbose: bool,

    #[arg(short, long, help = "JSON output")]
    json: bool,

    #[arg(long, help = "Configuration file path")]
    config: Option<String>,
}

#[derive(Subcommand)]
enum Commands {
    #[command(about = "Resolve a model alias to provider details")]
    Resolve {
        model: String,
        #[arg(long, help = "Show resolve trace")]
        trace: bool,
        #[arg(long, help = "Provider override")]
        provider: Option<String>,
    },
    #[command(about = "List available models")]
    Models {
        #[arg(long, help = "Only show authenticated models")]
        auth: bool,
    },
    #[command(about = "Run diagnostics")]
    Doctor,
    #[command(about = "Session statistics")]
    Stats,
    #[command(about = "Manage configuration")]
    Config {
        #[command(subcommand)]
        action: commands::config_cmd::ConfigAction,
    },
    #[command(about = "Manage sessions")]
    Sessions {
        #[command(subcommand)]
        action: commands::sessions::SessionAction,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let config = Config::load(cli.config.as_deref())?;
    let creds = CredentialStore::from_config(&config)?;
    creds.inject_env(); // Phase 1: explicit injection instead of hidden env reads

    // Print mode (single-turn streaming)
    if let Some(prompt) = cli.print {
        let model = cli.model.unwrap_or_else(|| config.default_model());
        return print::run(&model, &prompt, cli.provider.as_deref(), cli.verbose, cli.json).await;
    }

    // Default: enter TUI (if no subcommand)
    match cli.command {
        Some(Commands::Resolve { model, trace, provider }) => {
            resolve::run(&model, provider.as_deref(), trace, cli.json)?;
        }
        Some(Commands::Models { auth }) => {
            models::run(auth)?;
        }
        Some(Commands::Doctor) => {
            doctor::run(&config, &creds)?;
        }
        Some(Commands::Stats) => {
            stats::run()?;
        }
        Some(Commands::Config { action }) => {
            config_cmd::run(action)?;
        }
        Some(Commands::Sessions { action }) => {
            sessions::run(action)?;
        }
        None => {
            // No command and no -p: launch TUI
            eprintln!("{}", "Launching TUI (not yet implemented in MVP). Use -p for single-turn mode.".dimmed());
        }
    }

    Ok(())
}
