use anyhow::Result;
use clap::{CommandFactory, Parser, Subcommand};
use colored::Colorize;

mod commands;
mod config;
mod credentials;
mod display;
mod frontend;
mod model_inventory;
mod profile_runtime;
mod runtime;
mod runtime_turn;
mod security;
mod session;
mod session_seed;
#[cfg(test)]
mod test_support;

use commands::{doctor, models, resolve, session as session_cmd};
use config::Config;
use credentials::CredentialStore;

#[derive(Parser)]
#[command(name = "lattice")]
#[command(about = "LATTICE — model-centric LLM engine")]
#[command(version = "0.1.0")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    #[arg(short, long, global = true, help = "JSON output")]
    json: bool,

    #[arg(
        short,
        long,
        global = true,
        help = "Verbose output (show resolve trace)"
    )]
    verbose: bool,
}

#[derive(Subcommand)]
enum Commands {
    /// Run a prompt through an agent profile.
    Run {
        /// Prompt text.
        prompt: String,
        /// Model alias or canonical ID.
        #[arg(short = 'm', long = "model")]
        model: Option<String>,
        /// Agent profile name or path.
        #[arg(long)]
        profile: Option<String>,
        /// Session id to resume.
        #[arg(long)]
        session: Option<String>,
    },
    /// List available models.
    Models {
        /// Only show authenticated models.
        #[arg(long)]
        auth: bool,
    },
    /// Resolve a model alias to provider details.
    Resolve {
        /// Model alias to resolve.
        model: String,
    },
    /// Manage sessions.
    Session {
        #[command(subcommand)]
        action: session_cmd::SessionAction,
    },
    /// Run diagnostics.
    Doctor,
}

#[tokio::main]
async fn main() -> std::process::ExitCode {
    dotenvy::dotenv().ok();
    let cli = Cli::parse();

    // Initialize logging.
    if cli.verbose {
        let _ = lattice_core::init_logging(true);
    } else {
        let _ = lattice_core::init_logging(false);
    }

    // Run command uses the new v1.1 path with ExitCode semantics.
    if let Some(Commands::Run {
        prompt,
        model,
        profile,
        session,
    }) = cli.command
    {
        return commands::run::run(prompt, model, profile.as_deref(), session, cli.verbose, cli.json)
            .await;
    }

    // All other commands use the legacy Result<()> path.
    if let Err(e) = main_legacy(cli) {
        eprintln!("{}: {}", "error".red(), e);
        std::process::ExitCode::from(1)
    } else {
        std::process::ExitCode::SUCCESS
    }
}

fn main_legacy(cli: Cli) -> Result<()> {
    let config = Config::load(None)?;
    let creds = CredentialStore::from_config(&config)?;

    match cli.command {
        Some(Commands::Run { .. }) => unreachable!("handled above"),
        Some(Commands::Models { auth }) => {
            models::run(auth, cli.json, &creds)?;
        }
        Some(Commands::Resolve { model }) => {
            resolve::run(&model, cli.verbose, cli.json, &creds)?;
        }
        Some(Commands::Session { action }) => {
            session_cmd::run(action, cli.json)?;
        }
        Some(Commands::Doctor) => {
            doctor::run(&config, cli.json, &creds)?;
        }
        None => {
            let _ = Cli::command().print_help();
            println!();
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::Cli;
    use clap::{CommandFactory, Parser};

    // -- Positive parsing: Phase 10 supported commands --

    #[test]
    fn cli_surface_run_parses_prompt() {
        let cli = Cli::try_parse_from(["lattice", "run", "hello"]).unwrap();
        assert!(cli.command.is_some());
    }

    #[test]
    fn cli_surface_run_with_model_profile_session() {
        let cli = Cli::try_parse_from([
            "lattice",
            "run",
            "hello",
            "-m",
            "sonnet",
            "--profile",
            "default",
            "--session",
            "sess1",
        ])
        .unwrap();
        assert!(cli.command.is_some());
    }

    #[test]
    fn cli_surface_models_auth() {
        let cli = Cli::try_parse_from(["lattice", "models", "--auth"]).unwrap();
        assert!(cli.command.is_some());
    }

    #[test]
    fn cli_surface_resolve() {
        let cli = Cli::try_parse_from(["lattice", "resolve", "sonnet"]).unwrap();
        assert!(cli.command.is_some());
    }

    #[test]
    fn cli_surface_session_status() {
        let cli = Cli::try_parse_from(["lattice", "session", "status"]).unwrap();
        assert!(cli.command.is_some());
    }

    #[test]
    fn cli_surface_session_log() {
        let cli = Cli::try_parse_from(["lattice", "session", "log", "sess1"]).unwrap();
        assert!(cli.command.is_some());
    }

    #[test]
    fn cli_surface_session_checkout() {
        let cli = Cli::try_parse_from(["lattice", "session", "checkout", "sess1", "main"]).unwrap();
        assert!(cli.command.is_some());
    }

    #[test]
    fn cli_surface_session_branch_with_from() {
        let cli = Cli::try_parse_from([
            "lattice", "session", "branch", "sess1", "feature", "--from", "commit1",
        ])
        .unwrap();
        assert!(cli.command.is_some());
    }

    #[test]
    fn cli_surface_session_diff() {
        let cli = Cli::try_parse_from(["lattice", "session", "diff", "sess1", "a", "b"]).unwrap();
        assert!(cli.command.is_some());
    }

    #[test]
    fn cli_surface_session_reset() {
        let cli = Cli::try_parse_from(["lattice", "session", "reset", "sess1", "commit1"]).unwrap();
        assert!(cli.command.is_some());
    }

    #[test]
    fn cli_surface_doctor() {
        let cli = Cli::try_parse_from(["lattice", "doctor"]).unwrap();
        assert!(cli.command.is_some());
    }

    // -- Negative parsing: forbidden old commands (D-01, D-02, D-03, D-07, D-08) --

    #[test]
    fn cli_surface_rejects_tui() {
        assert!(Cli::try_parse_from(["lattice", "tui"]).is_err());
    }

    #[test]
    fn cli_surface_rejects_debug() {
        assert!(Cli::try_parse_from(["lattice", "debug"]).is_err());
    }

    #[test]
    fn cli_surface_rejects_config() {
        assert!(Cli::try_parse_from(["lattice", "config"]).is_err());
    }

    #[test]
    fn cli_surface_rejects_stats() {
        assert!(Cli::try_parse_from(["lattice", "stats"]).is_err());
    }

    #[test]
    fn cli_surface_rejects_validate() {
        assert!(Cli::try_parse_from(["lattice", "validate"]).is_err());
    }

    #[test]
    fn cli_surface_rejects_new() {
        assert!(Cli::try_parse_from(["lattice", "new"]).is_err());
    }

    #[test]
    fn cli_surface_rejects_list() {
        assert!(Cli::try_parse_from(["lattice", "list"]).is_err());
    }

    #[test]
    fn cli_surface_rejects_run_v11() {
        assert!(Cli::try_parse_from(["lattice", "run-v11", "hello"]).is_err());
    }

    #[test]
    fn cli_surface_rejects_quick_flag() {
        assert!(Cli::try_parse_from(["lattice", "-q", "hello"]).is_err());
    }

    #[test]
    fn cli_surface_rejects_sessions_plural() {
        assert!(Cli::try_parse_from(["lattice", "sessions"]).is_err());
    }

    // -- Negative parsing: forbidden old globals (D-05, D-06) --

    #[test]
    fn cli_surface_rejects_global_config() {
        assert!(Cli::try_parse_from(["lattice", "--config", "foo.toml", "doctor"]).is_err());
    }

    #[test]
    fn cli_surface_rejects_global_model() {
        assert!(Cli::try_parse_from(["lattice", "--model", "sonnet", "doctor"]).is_err());
    }

    #[test]
    fn cli_surface_rejects_global_provider() {
        assert!(Cli::try_parse_from(["lattice", "--provider", "openai", "doctor"]).is_err());
    }

    #[test]
    fn cli_surface_rejects_global_session() {
        assert!(Cli::try_parse_from(["lattice", "--session", "sess1", "doctor"]).is_err());
    }

    #[test]
    fn cli_surface_rejects_global_no_save() {
        assert!(Cli::try_parse_from(["lattice", "--no-save", "doctor"]).is_err());
    }

    #[test]
    fn cli_surface_rejects_global_continue() {
        assert!(Cli::try_parse_from(["lattice", "--continue", "doctor"]).is_err());
    }

    // -- Help text boundary test --

    #[test]
    fn cli_surface_help_contains_supported_commands() {
        let mut cmd = Cli::command();
        let help = cmd.render_help().to_string();
        for name in &["run", "models", "resolve", "session", "doctor"] {
            assert!(
                help.contains(name),
                "help should contain supported command '{name}'"
            );
        }
    }

    #[test]
    fn cli_surface_help_omits_removed_commands() {
        let mut cmd = Cli::command();
        let help = cmd.render_help().to_string();
        // Check that removed commands do not appear as top-level subcommand entries.
        // We check for the command name followed by whitespace (subcommand table entry)
        // to avoid false positives from words like "list" in descriptions.
        for name in &[
            "tui", "debug", "config", "stats", "validate", "new", "run-v11",
        ] {
            let pattern = format!("\n  {name} ");
            assert!(
                !help.contains(&pattern),
                "help should not contain removed top-level command '{name}'"
            );
        }
        // "sessions" (plural) must not appear as a subcommand.
        let sessions_pattern = "\n  sessions ";
        assert!(
            !help.contains(sessions_pattern),
            "help should not contain plural 'sessions' command"
        );
        // "list" must not appear as a top-level subcommand (it's a session subcommand).
        let list_pattern = "\n  list ";
        assert!(
            !help.contains(list_pattern),
            "help should not contain top-level 'list' command"
        );
    }
}
