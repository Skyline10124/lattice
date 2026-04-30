use anyhow::Result;
use clap::{Parser, Subcommand};
use colored::Colorize;

mod commands;
mod config;
mod credentials;
mod session;

use commands::{config_cmd, debug, doctor, models, new_agent, print, resolve, run, sessions, stats, validate};
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
    #[command(about = "Run a prompt through the model")]
    Run {
        prompt: String,
        #[arg(short, long, help = "Model alias or canonical ID")]
        model: Option<String>,
    },
    #[command(about = "Debug mode: trace-level logging with colored output")]
    Debug {
        #[arg(help = "Prompt to send (optional, for chat debugging)")]
        prompt: Option<String>,
        #[arg(short, long, help = "Model alias or canonical ID")]
        model: Option<String>,
        #[arg(long, help = "Provider override")]
        provider: Option<String>,
        #[arg(long, help = "Only resolve, don't chat")]
        resolve_only: bool,
    },
    #[command(about = "Validate agent profiles in ~/.artemis/agents/")]
    Validate {
        #[arg(help = "Optional path to agents directory")]
        dir: Option<String>,
    },
    #[command(about = "Create a new agent profile")]
    New {
        #[command(subcommand)]
        action: NewAction,
    },
}

#[derive(Subcommand)]
enum NewAction {
    #[command(about = "Create a new agent profile from template")]
    Agent {
        #[arg(help = "Agent name")]
        name: String,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    let cli = Cli::parse();

    // Initialize logging based on verbose or debug mode.
    if cli.verbose {
        artemis_core::init_logging(true);
    } else if matches!(cli.command, Some(Commands::Debug { .. })) {
        let log_dir = dirs::home_dir()
            .map(|h| h.join(".artemis"))
            .unwrap_or_else(|| std::path::PathBuf::from("/tmp"));
        let log_path = log_dir.join("debug.log");
        let _ = artemis_core::init_debug_logging(
            log_path.to_str().unwrap_or("/tmp/artemis-debug.log"),
        );
    } else {
        let _ = artemis_core::init_logging(false);
    }

    let config = Config::load(cli.config.as_deref())?;
    let creds = CredentialStore::from_config(&config)?;
    // Credentials loaded explicitly; passed to ModelRouter::with_credentials()

    // Print mode (single-turn streaming)
    if let Some(prompt) = cli.print {
        let model = cli.model.unwrap_or_else(|| config.default_model());
        return print::run(
            &model,
            &prompt,
            cli.provider.as_deref(),
            cli.verbose,
            cli.json,
            &creds,
        )
        .await;
    }

    // Default: enter TUI (if no subcommand)
    match cli.command {
        Some(Commands::Resolve {
            model,
            trace,
            provider,
        }) => {
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
        Some(Commands::Run { prompt, model }) => {
            let model = model
                .or_else(|| cli.model.clone())
                .unwrap_or_else(|| config.default_model());
            run::run(
                prompt,
                model,
                cli.provider.as_deref(),
                cli.verbose,
                cli.json,
                &creds,
            )?;
        }
        Some(Commands::Debug {
            prompt,
            model,
            provider,
            resolve_only,
        }) => {
            let model = model
                .or_else(|| cli.model.clone())
                .unwrap_or_else(|| config.default_model());
            debug::run(prompt, model, provider, resolve_only, &creds).await?;
        }
        Some(Commands::Validate { dir }) => {
            validate::run(dir)?;
        }
        Some(Commands::New { action }) => match action {
            NewAction::Agent { name } => new_agent::run(name)?,
        },
        None => {
            // No command and no -p: launch TUI
            eprintln!(
                "{}",
                "Launching TUI (not yet implemented in MVP). Use -p for single-turn mode.".dimmed()
            );
        }
    }

    Ok(())
}
