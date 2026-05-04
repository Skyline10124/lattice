use anyhow::Result;
use clap::{Parser, Subcommand};
use colored::Colorize;

mod commands;
mod config;
mod credentials;
mod display;
mod session;

use commands::{
    bus_status, config_cmd, debug, doctor, list_agents, models, new_agent, print, resolve, run,
    sessions, stats, validate,
};
use config::Config;
use credentials::CredentialStore;

#[derive(Parser)]
#[command(name = "lattice")]
#[command(about = "󰚩 LATTICE — model-centric LLM engine")]
#[command(version = "0.1.0")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    #[arg(short, long, help = "Single-turn print mode (pipe-friendly)")]
    print: Option<String>,

    #[arg(
        short,
        long,
        help = "Default model alias (can be overridden by subcommand --model)"
    )]
    model: Option<String>,

    #[arg(long, help = "Provider override")]
    provider: Option<String>,

    #[arg(short, long, global = true, help = "Continue last session")]
    continue_session: bool,

    #[arg(long, global = true, help = "Do not save session")]
    no_save: bool,

    #[arg(
        short,
        long,
        global = true,
        help = "Verbose output (show resolve trace)"
    )]
    verbose: bool,

    #[arg(short, long, global = true, help = "JSON output")]
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
    #[command(about = "Show bus status: agents, subscriptions, and bus configuration")]
    Bus {
        #[arg(long, help = "JSON output")]
        json: bool,
        #[arg(long, help = "Project directory (default: current dir)")]
        dir: Option<String>,
    },
    #[command(about = "Run a prompt through the model or pipeline")]
    Run {
        prompt: String,
        #[arg(short, long, help = "Model alias or canonical ID")]
        model: Option<String>,
        #[arg(long, help = "Provider override")]
        provider: Option<String>,
        #[arg(long, help = "Run as pipeline (start agent name)")]
        pipeline: Option<String>,
        #[arg(long, help = "Agent directory path (default: ~/.lattice/agents/)")]
        agents_dir: Option<String>,
        #[arg(
            long,
            help = "Plugin directory for DAG execution (default: builtin plugins)"
        )]
        plugins_dir: Option<String>,
        #[arg(long, help = "Tool definitions path (default: agent default tools)")]
        tools_dir: Option<String>,
    },
    #[command(about = "Debug mode: trace-level logging with colored output")]
    Debug {
        #[arg(help = "Model alias or canonical ID (positional)")]
        model: Option<String>,
        #[arg(long, help = "Prompt to send (for chat debugging)")]
        prompt: Option<String>,
        #[arg(long, help = "Provider override")]
        provider: Option<String>,
        #[arg(long, help = "Only resolve, don't chat")]
        resolve_only: bool,
    },
    #[command(about = "Validate agent profiles in ~/.lattice/agents/")]
    Validate {
        #[arg(help = "Optional path to agents directory")]
        dir: Option<String>,
    },
    #[command(about = "Create a new agent profile")]
    New {
        #[command(subcommand)]
        action: NewAction,
    },
    #[command(about = "List agents, models, etc.")]
    List {
        #[command(subcommand)]
        action: ListCommands,
    },
    #[command(about = "Launch the terminal UI (TUI)")]
    Tui {
        #[arg(long, help = "Optional prompt to pre-fill")]
        prompt: Option<String>,
        #[arg(long, help = "Model alias or canonical ID")]
        model: Option<String>,
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

#[derive(Subcommand)]
enum ListCommands {
    #[command(about = "List loaded agent profiles")]
    Agents,
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    let cli = Cli::parse();

    // Initialize logging based on verbose or debug mode.
    if cli.verbose {
        let _ = lattice_core::init_logging(true);
    } else if matches!(cli.command, Some(Commands::Debug { .. })) {
        let log_dir = dirs::home_dir()
            .map(|h| h.join(".lattice"))
            .unwrap_or_else(|| std::path::PathBuf::from("/tmp"));
        let log_path = log_dir.join("debug.log");
        let _ =
            lattice_core::init_debug_logging(log_path.to_str().unwrap_or("/tmp/lattice-debug.log"));
    } else {
        let _ = lattice_core::init_logging(false);
    }

    let config = Config::load(cli.config.as_deref())?;
    let creds = CredentialStore::from_config(&config)?;
    // Credentials loaded explicitly; passed to ModelRouter::with_credentials()

    // Print mode (single-turn streaming)
    if let Some(prompt) = cli.print {
        let model = cli.model.unwrap_or_else(|| config.default_model());
        let previous_session = if cli.continue_session {
            let mgr = crate::session::SessionManager::new();
            mgr.latest().ok().flatten()
        } else {
            None
        };
        return print::run(
            &model,
            &prompt,
            cli.provider.as_deref(),
            cli.verbose,
            cli.json,
            &creds,
            !cli.no_save,
            previous_session,
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
            resolve::run(&model, provider.as_deref(), trace, cli.json, &creds)?;
        }
        Some(Commands::Models { auth }) => {
            models::run(auth, &creds)?;
        }
        Some(Commands::Doctor) => {
            doctor::run(&config, &creds)?;
        }
        Some(Commands::Bus { json, dir }) => {
            bus_status::run(json, dir)?;
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
        Some(Commands::Run {
            prompt,
            model,
            provider,
            pipeline,
            agents_dir,
            plugins_dir,
            tools_dir,
        }) => {
            if let Some(start_agent) = pipeline {
                run::run_pipeline(
                    &prompt,
                    &start_agent,
                    agents_dir.as_deref(),
                    plugins_dir.as_deref(),
                    tools_dir.as_deref(),
                    cli.verbose,
                    cli.json,
                    creds.to_hashmap(),
                )
                .await?;
            } else {
                let model = model
                    .or_else(|| cli.model.clone())
                    .unwrap_or_else(|| config.default_model());
                let provider = provider.or_else(|| cli.provider.clone());
                let previous_session = if cli.continue_session {
                    let mgr = crate::session::SessionManager::new();
                    mgr.latest().ok().flatten()
                } else {
                    None
                };
                run::run(
                    prompt,
                    model,
                    provider.as_deref(),
                    cli.verbose,
                    cli.json,
                    &creds,
                    !cli.no_save,
                    previous_session,
                )
                .await?;
            }
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
        Some(Commands::Tui { prompt, model }) => {
            let mut cmd = std::process::Command::new("lattice-tui");
            if let Some(p) = &prompt {
                cmd.arg("--prompt").arg(p);
            }
            if let Some(m) = &model {
                cmd.arg("--model").arg(m);
            }
            let status = cmd.status().map_err(|e| {
                anyhow::anyhow!(
                    "Failed to launch lattice-tui: {} (is it built and on $PATH?)",
                    e
                )
            })?;
            if !status.success() {
                let code = status.code().unwrap_or(-1);
                anyhow::bail!("lattice-tui exited with code {}", code);
            }
        }
        Some(Commands::New { action }) => match action {
            NewAction::Agent { name } => new_agent::run(name)?,
        },
        Some(Commands::List { action }) => match action {
            ListCommands::Agents => list_agents::run()?,
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
