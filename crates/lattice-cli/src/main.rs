use anyhow::Result;
use clap::{CommandFactory, Parser, Subcommand};
use colored::Colorize;

mod commands;
use commands::{audit, doctor, lancet as lancet_cmd, models, resolve, session as session_cmd};

#[derive(Parser)]
#[command(name = "lattice")]
#[command(about = "LATTICE — session-centric LLM engine")]
#[command(version = "0.1.0")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    #[arg(
        short,
        long,
        global = true,
        help = "Emit one TurnEvent JSON object per line on stdout (JSONL). Human status lines and verbose epilogue are suppressed. JSONL emits public TurnEvent values as received, including ReasoningDelta when present."
    )]
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
    /// Inspect CEP audit / provenance records.
    Audit(audit::AuditCmd),
    /// LANCET inspection and self-check subcommands.
    Lancet {
        #[command(subcommand)]
        action: Option<lancet_cmd::LancetAction>,
    },
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

    match run_commands(cli).await {
        Ok(exit_code) => exit_code,
        Err(e) => {
            eprintln!("{}: {}", "error".red(), e);
            std::process::ExitCode::from(1)
        }
    }
}

async fn run_commands(cli: Cli) -> Result<std::process::ExitCode> {
    match cli.command {
        Some(Commands::Models { auth }) => {
            models::run(auth, cli.json)?;
        }
        Some(Commands::Resolve { model }) => {
            resolve::run(&model, cli.verbose, cli.json)?;
        }
        Some(Commands::Session { action }) => {
            return session_cmd::run(action, cli.json, cli.verbose).await;
        }
        Some(Commands::Doctor) => {
            doctor::run(cli.json)?;
        }
        Some(Commands::Audit(cmd)) => {
            audit::run(cmd, cli.json)?;
        }
        Some(Commands::Lancet { action }) => {
            lancet_cmd::run(action, cli.json)?;
        }
        None => {
            let _ = Cli::command().print_help();
            println!();
        }
    }

    Ok(std::process::ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use super::{Cli, Commands};
    use clap::{CommandFactory, Parser};

    // -- Positive parsing: CLI supported commands --

    #[test]
    fn cli_surface_run_is_not_exposed() {
        assert!(Cli::try_parse_from(["lattice", "run", "hello"]).is_err());
    }

    #[test]
    fn cli_surface_session_new_agent() {
        let cli = Cli::try_parse_from(["lattice", "session", "new", "--agent", "coder"]).unwrap();
        assert!(cli.command.is_some());
    }

    #[test]
    fn cli_surface_session_send() {
        let cli = Cli::try_parse_from(["lattice", "session", "send", "sess1", "hello"]).unwrap();
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
        for name in &["models", "resolve", "session", "doctor", "audit", "lancet"] {
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
            vec!["models", "resolve", "session", "doctor", "audit", "lancet"]
        );
    }

    #[test]
    fn cli_surface_lancet_no_subcommand_parses() {
        let cli = Cli::try_parse_from(["lattice", "lancet"]).unwrap();
        assert!(cli.command.is_some());
        match cli.command.unwrap() {
            Commands::Lancet { action } => assert!(action.is_none()),
            _ => panic!("expected Lancet"),
        }
    }

    #[test]
    fn cli_surface_lancet_status_parses() {
        let cli = Cli::try_parse_from(["lattice", "lancet", "status"]).unwrap();
        assert!(matches!(cli.command, Some(Commands::Lancet { .. })));
    }

    #[test]
    fn cli_surface_lancet_doctor_parses() {
        let cli = Cli::try_parse_from(["lattice", "lancet", "doctor"]).unwrap();
        assert!(matches!(cli.command, Some(Commands::Lancet { .. })));
    }

    #[test]
    fn cli_surface_doctor_unchanged() {
        let cli = Cli::try_parse_from(["lattice", "doctor"]).unwrap();
        assert!(matches!(cli.command, Some(Commands::Doctor)));
    }

    #[test]
    fn cli_surface_audit_unchanged() {
        let cli = Cli::try_parse_from(["lattice", "audit"]).unwrap();
        assert!(matches!(cli.command, Some(Commands::Audit(_))));
    }
}
