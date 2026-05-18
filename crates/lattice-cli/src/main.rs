use anyhow::Result;
use clap::{CommandFactory, Parser, Subcommand};
use colored::Colorize;

mod commands;
use commands::{doctor, models, resolve, session as session_cmd};

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
        let _ = lattice_llm::init_logging(true);
    } else {
        let _ = lattice_llm::init_logging(false);
    }

    // Run command uses the v1.1 path with ExitCode semantics.
    if let Some(Commands::Run {
        prompt,
        model,
        profile,
        session,
    }) = cli.command
    {
        return commands::run::run(
            prompt,
            model,
            profile.as_deref(),
            session,
            cli.verbose,
            cli.json,
        )
        .await;
    }

    // All other commands use the Result<()> path.
    if let Err(e) = run_other_commands(cli) {
        eprintln!("{}: {}", "error".red(), e);
        std::process::ExitCode::from(1)
    } else {
        std::process::ExitCode::SUCCESS
    }
}

fn run_other_commands(cli: Cli) -> Result<()> {
    match cli.command {
        Some(Commands::Run { .. }) => unreachable!("handled above"),
        Some(Commands::Models { auth }) => {
            models::run(auth, cli.json)?;
        }
        Some(Commands::Resolve { model }) => {
            resolve::run(&model, cli.verbose, cli.json)?;
        }
        Some(Commands::Session { action }) => {
            session_cmd::run(action, cli.json)?;
        }
        Some(Commands::Doctor) => {
            doctor::run(cli.json)?;
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

    #[test]
    fn cli_surface_rejects_unknown_command() {
        assert!(Cli::try_parse_from(["lattice", "unsupported-command"]).is_err());
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
    fn cli_surface_help_lists_exact_supported_command_count() {
        let subcommands: Vec<_> = Cli::command()
            .get_subcommands()
            .map(|command| command.get_name().to_string())
            .collect();
        assert_eq!(
            subcommands,
            vec!["run", "models", "resolve", "session", "doctor"]
        );
    }
}
