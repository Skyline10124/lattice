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
        help = "输出 JSONL；TurnEvent 保留原形，并提供 submission/acceptance/snapshot/transcript/gap 控制记录，不混入人类状态行。"
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
    /// 向既有会话提交请求；等同于 session send。
    Run {
        session_id: String,
        prompt: String,
        /// 提交前保留的规范 TurnId；确认未知时仍使用这个 ID。
        #[arg(long, value_parser = session_cmd::parse_turn_id)]
        turn_id: Option<lattice_api::local::TurnId>,
    },
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
    Session {
        #[command(subcommand)]
        action: session_cmd::SessionAction,
    },
    /// Run diagnostics.
    Doctor,
    /// Inspect finalized CEP audit records.
    Audit(Box<audit::AuditCmd>),
    /// LANCET inspection and self-check subcommands.
    Lancet {
        #[command(subcommand)]
        action: Option<lancet_cmd::LancetAction>,
    },
    /// Open the terminal UI.
    Tui {
        /// Optional session id to open. If omitted, a new session is created.
        session_id: Option<String>,
        /// Agent profile name used when creating a new TUI session.
        #[arg(long, default_value = "default")]
        agent: String,
        /// Project target branch for new TUI sessions.
        /// Required when creating a new session; ignored when opening an existing one.
        #[arg(long, required_unless_present = "session_id")]
        project_branch: Option<String>,
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
        Some(Commands::Run {
            session_id,
            prompt,
            turn_id,
        }) => {
            return session_cmd::run(
                session_cmd::SessionAction::Send {
                    session_id,
                    prompt,
                    turn_id,
                },
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
            lancet_cmd::run(action, cli.json)?;
        }
        Some(Commands::Tui {
            session_id,
            agent,
            project_branch,
        }) => {
            let agents_dir =
                commands::run::safe_agents_dir(None).map_err(|error| anyhow::anyhow!(error))?;
            return lattice_tui::run(lattice_tui::TuiOptions {
                session_id,
                agent,
                project_branch,
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

    // -- Positive parsing: CLI supported commands --

    #[test]
    fn cli_surface_background_and_request_identity() {
        for args in [
            vec![
                "lattice",
                "run",
                "session-id",
                "hello",
                "--turn-id",
                "10000000-0000-4000-8000-000000000001",
            ],
            vec![
                "lattice",
                "session",
                "send",
                "session-id",
                "hello",
                "--turn-id",
                "10000000-0000-4000-8000-000000000001",
            ],
            vec![
                "lattice",
                "session",
                "turn",
                "session-id",
                "10000000-0000-4000-8000-000000000001",
                "--follow",
            ],
            vec![
                "lattice",
                "session",
                "cancel",
                "session-id",
                "10000000-0000-4000-8000-000000000001",
            ],
            vec!["lattice", "session", "recover", "session-id"],
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
            assert!(Cli::try_parse_from(&args).is_ok(), "must parse {args:?}");
        }
        assert!(
            Cli::try_parse_from([
                "lattice",
                "session",
                "send",
                "s",
                "hi",
                "--turn-id",
                "not-a-uuid"
            ])
            .is_err()
        );
    }

    #[test]
    fn cli_surface_session_new_agent() {
        let cli = Cli::try_parse_from([
            "lattice",
            "session",
            "new",
            "--agent",
            "coder",
            "--project-branch",
            "main",
        ])
        .unwrap();
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
    fn cli_surface_tui_requires_project_branch_for_new_session() {
        assert!(
            Cli::try_parse_from(["lattice", "tui"]).is_err(),
            "new TUI sessions must require --project-branch"
        );
    }

    #[test]
    fn cli_surface_tui_parses_session_id_and_agent() {
        let cli = Cli::try_parse_from(["lattice", "tui", "sess1", "--agent", "coder"]).unwrap();
        match cli.command.unwrap() {
            Commands::Tui {
                session_id,
                agent,
                project_branch,
            } => {
                assert_eq!(session_id.as_deref(), Some("sess1"));
                assert_eq!(agent, "coder");
                assert!(project_branch.is_none());
            }
            _ => panic!("expected Tui"),
        }
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
    fn cli_surface_tui_accepts_project_branch() {
        let cli = Cli::try_parse_from([
            "lattice",
            "tui",
            "--agent",
            "coder",
            "--project-branch",
            "feature/x",
        ])
        .unwrap();
        match cli.command.unwrap() {
            Commands::Tui {
                session_id,
                agent,
                project_branch,
            } => {
                assert!(session_id.is_none());
                assert_eq!(agent, "coder");
                assert_eq!(project_branch.as_deref(), Some("feature/x"));
            }
            _ => panic!("expected Tui"),
        }
    }

    #[test]
    fn cli_surface_session_new_requires_project_branch() {
        // session new without --project-branch should fail after 36-15.
        // Currently this parses fine (no required flag), making this test RED.
        assert!(
            Cli::try_parse_from(["lattice", "session", "new", "--agent", "coder"]).is_err(),
            "session new should require --project-branch"
        );
    }
}
