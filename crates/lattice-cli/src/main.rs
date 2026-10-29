// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use anyhow::Result;
use clap::{CommandFactory, Parser, Subcommand};

mod commands;
#[cfg(test)]
mod test_support;
use commands::{audit, lancet as lancet_cmd, models, session as session_cmd};

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
        help = "Emit JSONL (submit/cancel/head/entry/text/connection/error records) without human status lines."
    )]
    json: bool,

    #[arg(short, long, global = true, help = "Verbose logging")]
    verbose: bool,
}

#[derive(Subcommand)]
enum Commands {
    /// Submit a request; same as session send.
    Run(session_cmd::SendArgs),
    /// Manage the local daemon for this user and state root.
    Daemon {
        #[command(subcommand)]
        action: commands::daemon::DaemonAction,
    },
    /// Manage provider API keys and credential status.
    Auth {
        #[command(subcommand)]
        action: commands::auth::AuthAction,
    },
    /// List available models or refresh model metadata.
    Models {
        /// Only show authenticated models.
        #[arg(long)]
        auth: bool,
        /// Only list models from this configured provider.
        #[arg(long)]
        provider: Option<String>,
        #[command(subcommand)]
        action: Option<models::ModelsAction>,
    },
    /// Manage sessions.
    #[command(disable_help_subcommand = true)]
    Session {
        #[command(subcommand)]
        action: session_cmd::SessionAction,
    },
    /// Inspect CEP invocation records (registered, started and terminal).
    Audit(Box<audit::AuditCmd>),
    /// LANCET inspection and self-check subcommands.
    Lancet {
        #[command(subcommand)]
        action: Option<lancet_cmd::LancetAction>,
    },
    /// Open the terminal UI.
    Tui {
        /// Existing session; when omitted, a session is created once the first submission is accepted.
        session_id: Option<String>,
        #[arg(long, required_unless_present = "session_id")]
        agent: Option<String>,
    },
}

fn parse_cli(
    args: impl IntoIterator<Item = impl Into<std::ffi::OsString> + Clone>,
) -> Result<Cli, clap::Error> {
    let cli = Cli::try_parse_from(args)?;
    if let Some(Commands::Models {
        auth,
        provider,
        action: Some(_),
    }) = &cli.command
        && (*auth || provider.is_some())
    {
        return Err(Cli::command().error(
            clap::error::ErrorKind::ArgumentConflict,
            "--auth and --provider cannot be used with models refresh",
        ));
    }
    Ok(cli)
}

fn main() -> std::process::ExitCode {
    let cli = match parse_cli(std::env::args_os()) {
        Ok(cli) => cli,
        Err(error) => {
            let help = matches!(
                error.kind(),
                clap::error::ErrorKind::DisplayHelp | clap::error::ErrorKind::DisplayVersion
            );
            if !help && std::env::args_os().any(|arg| arg == "--json" || arg == "-j") {
                println!(
                    "{}",
                    serde_json::json!({"type":"error", "kind":"bad_request", "message":"invalid command; use --help"})
                );
            } else if !help
                && std::env::args_os()
                    .skip(1)
                    .find(|arg| !arg.as_encoded_bytes().starts_with(b"-") && arg != "help")
                    .is_some_and(|arg| arg == "auth")
            {
                // 包括 help auth 路径在内，clap 错误都可能回显误放在参数中的 key。
                eprintln!("error: invalid auth command; use lattice auth --help");
            } else {
                let _ = error.print();
            }
            return std::process::ExitCode::from(if help { 0 } else { 2 });
        }
    };
    async_main(cli)
}

#[tokio::main]
async fn async_main(cli: Cli) -> std::process::ExitCode {
    let json = cli.json;
    let _ = lattice_api::init_logging(cli.verbose);
    match run_commands(cli).await {
        Ok(exit_code) => exit_code,
        Err(error) => commands::run::report_error(&error, json),
    }
}

async fn run_commands(cli: Cli) -> Result<std::process::ExitCode> {
    match cli.command {
        Some(Commands::Run(args)) => {
            return session_cmd::run(
                session_cmd::SessionAction::Send(args),
                cli.json,
                cli.verbose,
            )
            .await;
        }
        Some(Commands::Daemon { action }) => {
            return commands::daemon::run(action, cli.json).await;
        }
        Some(Commands::Auth { action }) => {
            commands::auth::run(action, cli.json).await?;
        }
        Some(Commands::Models {
            auth,
            provider,
            action,
        }) => {
            models::run(provider, auth, action, cli.json).await?;
        }
        Some(Commands::Session { action }) => {
            return session_cmd::run(action, cli.json, cli.verbose).await;
        }
        Some(Commands::Audit(cmd)) => {
            audit::run(*cmd, cli.json).await?;
        }
        Some(Commands::Lancet { action }) => {
            lancet_cmd::run(action, cli.json).await?;
        }
        Some(Commands::Tui { session_id, agent }) => {
            return lattice_tui::run(lattice_tui::TuiOptions {
                session_id,
                agent: agent.unwrap_or_default(),
                session_dir: session_cmd::default_session_dir(),
                project_root: std::env::current_dir()?,
            })
            .await;
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
    use super::{Cli, Commands, parse_cli};
    use clap::{CommandFactory, Parser};

    #[test]
    fn models_accepts_provider_and_auth_filters_and_documents_them() {
        let cli = Cli::try_parse_from([
            "lattice",
            "--json",
            "models",
            "--provider",
            "local",
            "--auth",
        ])
        .unwrap();
        assert!(cli.json);
        assert!(
            matches!(cli.command, Some(Commands::Models { auth: true, provider: Some(provider), action: None }) if provider == "local")
        );
        let mut command = Cli::command();
        let help = command
            .find_subcommand_mut("models")
            .unwrap()
            .render_help()
            .to_string();
        assert!(help.contains("--provider"));
        assert!(help.contains("--auth"));
    }

    #[test]
    fn models_refresh_is_separate_from_list_filters() {
        assert!(matches!(
            parse_cli(["lattice", "models", "refresh"]).unwrap().command,
            Some(Commands::Models {
                action: Some(crate::commands::models::ModelsAction::Refresh),
                ..
            })
        ));
        for args in [
            vec!["lattice", "models", "--auth", "refresh"],
            vec!["lattice", "models", "--provider", "anthropic", "refresh"],
            vec!["lattice", "models", "refresh", "--auth"],
        ] {
            assert!(parse_cli(args).is_err());
        }
    }

    #[test]
    fn models_refresh_accepts_global_flags_before_and_after_the_subcommand() {
        for flag in ["--json", "--verbose"] {
            for args in [
                vec!["lattice", flag, "models", "refresh"],
                vec!["lattice", "models", flag, "refresh"],
                vec!["lattice", "models", "refresh", flag],
            ] {
                let cli = parse_cli(args).unwrap();
                assert_eq!(cli.json, flag == "--json");
                assert_eq!(cli.verbose, flag == "--verbose");
                assert!(matches!(
                    cli.command,
                    Some(Commands::Models {
                        action: Some(crate::commands::models::ModelsAction::Refresh),
                        ..
                    })
                ));
            }
        }
    }

    #[test]
    fn auth_surface_accepts_only_provider_arguments_and_lists_commands() {
        for args in [
            vec!["auth", "set", "openai"],
            vec!["auth", "remove", "openai"],
            vec!["auth", "status"],
        ] {
            assert!(Cli::try_parse_from(std::iter::once("lattice").chain(args)).is_ok());
        }
        for args in [
            vec!["auth", "set"],
            vec!["auth", "set", "openai", "key"],
            vec!["auth", "login", "chatgpt"],
            vec!["auth", "logout", "chatgpt"],
        ] {
            assert!(Cli::try_parse_from(std::iter::once("lattice").chain(args)).is_err());
        }
        use clap::CommandFactory;
        let mut command = Cli::command();
        let auth = command
            .find_subcommand_mut("auth")
            .unwrap()
            .render_long_help()
            .to_string();
        for word in ["set", "remove", "status"] {
            assert!(auth.contains(word));
        }
    }

    #[test]
    fn r2_send_run_and_node_surface() {
        for command in [vec!["session", "send"], vec!["run"]] {
            for tail in [
                vec!["hello", "--agent", "coder"],
                vec!["hello", "--session", "abcd"],
            ] {
                let mut args = vec!["lattice"];
                args.extend(command.clone());
                args.extend(tail);
                assert!(Cli::try_parse_from(&args).is_ok(), "{args:?}");
            }
        }
        for node in ["0", "18446744073709551615"] {
            assert!(Cli::try_parse_from(["lattice", "session", "rewind", "abcd", node]).is_ok());
        }
        for node in ["-1", "+1", "ff00", "18446744073709551616"] {
            assert!(Cli::try_parse_from(["lattice", "session", "rewind", "abcd", node]).is_err());
        }
        assert!(
            Cli::try_parse_from([
                "lattice",
                "session",
                "turn",
                "10000000-0000-4000-8000-000000000001"
            ])
            .is_ok()
        );
    }

    // -- Help text boundary test --

    #[test]
    fn cli_surface_tui_requires_agent_only_without_session() {
        assert!(Cli::try_parse_from(["lattice", "tui"]).is_err());
        assert!(Cli::try_parse_from(["lattice", "tui", "--agent", "coder"]).is_ok());
        let cli = Cli::try_parse_from(["lattice", "tui", "abcd"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Commands::Tui {
                session_id: Some(_),
                agent: None
            })
        ));
    }
}
