// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use anyhow::Result;
use clap::{CommandFactory, Parser, Subcommand};

mod commands;
mod env_boundary;
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
        help = "输出 JSONL：submission/acceptance/snapshot/history/event/connection/error，不混入人类状态行。"
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
    /// 提交请求；等同于 session send。
    Run(session_cmd::SendArgs),
    /// 管理本用户、同一状态根的本地后台。
    Daemon {
        #[command(subcommand)]
        action: commands::daemon::DaemonAction,
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
    #[command(disable_help_subcommand = true)]
    Session {
        #[command(subcommand)]
        action: session_cmd::SessionAction,
    },
    /// Run diagnostics.
    Doctor,
    /// Inspect CEP invocation records (registered, started and terminal).
    Audit(Box<audit::AuditCmd>),
    /// LANCET inspection and self-check subcommands.
    Lancet {
        #[command(subcommand)]
        action: Option<lancet_cmd::LancetAction>,
    },
    /// Open the terminal UI.
    Tui {
        /// 已有会话；省略时在首次提交接受后创建。
        session_id: Option<String>,
        #[arg(long, required_unless_present = "session_id")]
        agent: Option<String>,
    },
}

fn main() -> std::process::ExitCode {
    let cli = match Cli::try_parse() {
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
            } else {
                let _ = error.print();
            }
            return std::process::ExitCode::from(if help { 0 } else { 2 });
        }
    };
    if let Some(Commands::Daemon {
        action: commands::daemon::DaemonAction::Serve { state_root },
    }) = &cli.command
    {
        let root = match commands::daemon::state_root(state_root.as_deref()) {
            Ok(root) => root,
            Err(error) => return commands::run::report_error(&error, cli.json),
        };
        env_boundary::prepare_daemon_environment(&root);
    } else {
        // dotenv 仅在提交端、运行时创建前安装；后台不运行工作区环境加载器。
        env_boundary::load_workspace_dotenv();
    }
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
            audit::run(*cmd, cli.json).await?;
        }
        Some(Commands::Lancet { action }) => {
            lancet_cmd::run(action, cli.json).await?;
        }
        Some(Commands::Tui { session_id, agent }) => {
            let agents_dir = commands::run::agents_dir(None);
            return lattice_tui::run(lattice_tui::TuiOptions {
                session_id,
                agent: agent.unwrap_or_default(),
                session_dir: session_cmd::default_session_dir(),
                agents_dir,
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
    use super::{Cli, Commands};
    use clap::{CommandFactory, Parser};

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

    #[test]
    fn cli_surface_background_and_request_identity() {
        let id = "10000000-0000-4000-8000-000000000001";
        for args in [
            vec![
                "lattice",
                "run",
                "hello",
                "--agent",
                "coder",
                "--turn-id",
                id,
            ],
            vec![
                "lattice",
                "session",
                "send",
                "hello",
                "--session",
                "abcd",
                "--turn-id",
                id,
            ],
            vec!["lattice", "session", "turn", id, "--follow"],
            vec!["lattice", "session", "cancel", "abcd", id],
            vec![
                "lattice",
                "daemon",
                "serve",
                "--state-root",
                "/tmp/isolated-state",
            ],
            vec!["lattice", "daemon", "status"],
            vec!["lattice", "daemon", "stop"],
        ] {
            assert!(Cli::try_parse_from(args).is_ok());
        }
        assert!(
            Cli::try_parse_from([
                "lattice",
                "run",
                "hello",
                "--agent",
                "coder",
                "--turn-id",
                "invalid"
            ])
            .is_err()
        );
    }
    #[test]
    fn replay_by_turn_id_needs_neither_session_nor_agent() {
        for command in [vec!["session", "send"], vec!["run"]] {
            let mut args = vec!["lattice"];
            args.extend(command);
            args.extend([
                "original input",
                "--turn-id",
                "00000000-0000-4000-8000-000000000001",
            ]);
            assert!(Cli::try_parse_from(args).is_ok());
        }
    }
    #[test]
    fn cli_surface_session_send() {
        let cli =
            Cli::try_parse_from(["lattice", "session", "send", "hello", "--session", "sess1"])
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
    fn cli_surface_session_log() {
        let cli = Cli::try_parse_from(["lattice", "session", "log", "sess1"]).unwrap();
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
        for name in &[
            "models", "resolve", "session", "doctor", "audit", "lancet", "tui",
        ] {
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
            vec![
                "run", "daemon", "models", "resolve", "session", "doctor", "audit", "lancet", "tui"
            ]
        );
    }

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

    #[test]
    fn cli_surface_audit_replay_subcommand() {
        let cli = Cli::try_parse_from([
            "lattice",
            "audit",
            "replay",
            "--session",
            "11111111111111111111111111111111",
            "--turn",
            "22222222-2222-4222-8222-222222222222",
        ])
        .unwrap();
        match cli.command {
            Some(Commands::Audit(audit_cmd)) => match audit_cmd.action {
                Some(super::audit::AuditAction::Replay(args)) => {
                    assert_eq!(args.turn, "22222222-2222-4222-8222-222222222222");
                    assert_eq!(args.session, "11111111111111111111111111111111");
                }
                _ => panic!("expected Replay action"),
            },
            _ => panic!("expected Audit command"),
        }
    }

    #[test]
    fn cli_surface_audit_query_subcommand() {
        let cli = Cli::try_parse_from(["lattice", "audit", "query", "--session", "s1"]).unwrap();
        match cli.command {
            Some(Commands::Audit(audit_cmd)) => match audit_cmd.action {
                Some(super::audit::AuditAction::Query(args)) => {
                    assert_eq!(args.session.as_deref(), Some("s1"));
                }
                _ => panic!("expected Query action"),
            },
            _ => panic!("expected Audit command"),
        }
    }
    #[test]
    fn session_help_lists_exactly_the_six_operations() {
        let help = Cli::try_parse_from(["lattice", "session", "--help"])
            .err()
            .expect("help exits parsing")
            .to_string();
        let commands: Vec<_> = help
            .split_once("Commands:\n")
            .unwrap()
            .1
            .split_once("Options:")
            .unwrap()
            .0
            .lines()
            .filter_map(|line| line.split_whitespace().next())
            .collect();
        assert_eq!(
            commands,
            ["send", "rewind", "list", "log", "turn", "cancel"]
        );
    }
}
