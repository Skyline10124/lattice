use anyhow::Result;
use clap::{Parser, Subcommand};

mod commands;
mod config;
mod credentials;
mod display;
mod frontend;
mod session;

use commands::{
    bus_status, config_cmd, debug, doctor, list_agents, models, new_agent, resolve, run,
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

    #[arg(short, long, help = "Quick single-turn query (no tool execution)")]
    quick: Option<String>,

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

    #[arg(long, global = true, help = "Continue a specific session id")]
    session: Option<String>,

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
        #[arg(help = "Prompt text, or use --file to read from a file")]
        prompt: Option<String>,
        #[arg(short = 'f', long = "file", help = "Read prompt text from a file")]
        file: Option<String>,
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
        #[arg(long, help = "Dry-run: compile prompt and print without calling LLM")]
        dry_run: bool,
        #[arg(long, help = "System prompt for prompt engine test")]
        system_prompt: Option<String>,
        #[arg(long, default_value_t = 10, help = "Maximum agent turns")]
        max_turns: u32,
        #[arg(
            long,
            overrides_with = "no_stream",
            help = "Stream tokens as they arrive"
        )]
        stream: bool,
        #[arg(
            long,
            overrides_with = "stream",
            help = "Print only after the run completes"
        )]
        no_stream: bool,
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
    let save_sessions = config.core.save_sessions && !cli.no_save;

    // Quick mode (single-turn, no tool execution, pipe-friendly)
    if let Some(prompt) = cli.quick {
        let model = cli.model.unwrap_or_else(|| config.default_model());
        let previous_session =
            load_requested_session(cli.session.as_deref(), cli.continue_session)?;
        return run::run(
            prompt,
            model,
            cli.provider.as_deref(),
            cli.verbose,
            cli.json,
            &creds,
            save_sessions,
            previous_session,
            false,
            None,
            1,
            !cli.json,
        )
        .await;
    }

    // Dispatch subcommand
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
            file,
            model,
            provider,
            pipeline,
            agents_dir,
            plugins_dir,
            tools_dir,
            dry_run,
            system_prompt,
            max_turns,
            stream,
            no_stream,
        }) => {
            let prompt = load_run_prompt(prompt, file.as_deref())?;
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
                let previous_session =
                    load_requested_session(cli.session.as_deref(), cli.continue_session)?;
                let stream_output = if cli.json {
                    false
                } else if stream {
                    true
                } else if no_stream {
                    false
                } else {
                    config.core.stream
                };
                run::run(
                    prompt,
                    model,
                    provider.as_deref(),
                    cli.verbose,
                    cli.json,
                    &creds,
                    save_sessions,
                    previous_session,
                    dry_run,
                    system_prompt.as_deref(),
                    max_turns,
                    stream_output,
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
            let previous_session =
                load_requested_session(cli.session.as_deref(), cli.continue_session)?;
            launch_tui(
                prompt,
                model,
                &cli.model,
                cli.provider.clone(),
                &config,
                &creds,
                save_sessions,
                previous_session,
            )
            .await?;
        }
        Some(Commands::New { action }) => match action {
            NewAction::Agent { name } => new_agent::run(name)?,
        },
        Some(Commands::List { action }) => match action {
            ListCommands::Agents => list_agents::run()?,
        },
        None => {
            // No subcommand — launch TUI like Claude Code
            let previous_session =
                load_requested_session(cli.session.as_deref(), cli.continue_session)?;
            launch_tui(
                None,
                None,
                &cli.model,
                cli.provider.clone(),
                &config,
                &creds,
                save_sessions,
                previous_session,
            )
            .await?;
        }
    }

    Ok(())
}

fn load_requested_session(
    id: Option<&str>,
    continue_latest: bool,
) -> Result<Option<crate::session::Session>> {
    let manager = crate::session::SessionManager::new();
    if let Some(id) = id {
        return manager
            .load(id)?
            .ok_or_else(|| anyhow::anyhow!("Session '{}' not found", id))
            .map(Some);
    }
    if continue_latest {
        return manager.latest();
    }
    Ok(None)
}

fn load_run_prompt(prompt: Option<String>, file: Option<&str>) -> Result<String> {
    match (prompt, file) {
        (Some(_), Some(_)) => anyhow::bail!("Use either PROMPT or --file, not both"),
        (Some(prompt), None) => Ok(prompt),
        (None, Some(path)) => std::fs::read_to_string(path)
            .map_err(|err| anyhow::anyhow!("Failed to read prompt file '{}': {}", path, err)),
        (None, None) => anyhow::bail!("Missing prompt text or --file"),
    }
}

#[allow(clippy::too_many_arguments)]
async fn launch_tui(
    prompt: Option<String>,
    model: Option<String>,
    cli_model: &Option<String>,
    provider_override: Option<String>,
    config: &Config,
    creds: &CredentialStore,
    save_sessions: bool,
    previous_session: Option<crate::session::Session>,
) -> Result<()> {
    let model = model.or_else(|| cli_model.clone());
    let options = frontend::tui::options_from_config(
        prompt,
        model,
        provider_override,
        config,
        creds,
        save_sessions,
        previous_session,
    );
    frontend::tui::run(options).await
}

#[cfg(test)]
mod tests {
    use super::{load_run_prompt, Cli, Commands};
    use clap::Parser;
    use std::fs;

    #[test]
    fn load_run_prompt_reads_file_verbatim() {
        let path =
            std::env::temp_dir().join(format!("lattice-run-prompt-{}.txt", std::process::id()));
        let content = "line 1\nquote: \"hello\"\nslash: \\\n";
        fs::write(&path, content).expect("write prompt fixture");

        let loaded = load_run_prompt(None, path.to_str());

        fs::remove_file(&path).ok();
        assert_eq!(loaded.expect("prompt file should load"), content);
    }

    #[test]
    fn load_run_prompt_rejects_prompt_and_file_together() {
        let err = load_run_prompt(Some("inline".into()), Some("prompt.txt"))
            .expect_err("prompt and file should conflict");

        assert!(err.to_string().contains("either PROMPT or --file"));
    }

    #[test]
    fn run_stream_flags_default_to_config_control() {
        let cli = Cli::parse_from(["lattice", "run", "hello"]);
        let Some(Commands::Run {
            stream, no_stream, ..
        }) = cli.command
        else {
            panic!("expected run command");
        };

        assert!(!stream);
        assert!(!no_stream);
    }

    #[test]
    fn run_no_stream_overrides_stream_flag() {
        let cli = Cli::parse_from(["lattice", "run", "hello", "--stream", "--no-stream"]);
        let Some(Commands::Run {
            stream, no_stream, ..
        }) = cli.command
        else {
            panic!("expected run command");
        };

        assert!(!stream);
        assert!(no_stream);
    }
}
