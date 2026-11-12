// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use anyhow::Result;
use clap::{CommandFactory, Parser, Subcommand};

mod commands;
#[cfg(test)]
mod test_support;
use commands::{models, session as session_cmd};

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
        help = "Emit JSONL (submit/configure/cancel/head/entry/text/connection/error records) without human status lines."
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
    /// Manage provider credentials and Sign in with ChatGPT.
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
        provider: Option<lattice_local::ProviderId>,
        #[command(subcommand)]
        action: Option<models::ModelsAction>,
    },
    /// Manage sessions.
    #[command(disable_help_subcommand = true)]
    Session {
        #[command(subcommand)]
        action: session_cmd::SessionAction,
    },
    /// Open the terminal UI.
    Tui {
        /// Existing session; when omitted, a new session is created via configure.
        session_id: Option<String>,
        #[arg(long, required_unless_present = "session_id")]
        agent: Option<lattice_local::AgentName>,
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
    run_with_runtime(cli)
}

fn run_with_runtime(cli: Cli) -> std::process::ExitCode {
    let json = cli.json;
    let _ = lattice_local::init_logging(cli.verbose);
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => return commands::run::report_error(&error.into(), json),
    };
    if let Some(Commands::Daemon {
        action:
            commands::daemon::DaemonAction::Serve {
                state_root,
                drain_timeout,
            },
    }) = cli.command
    {
        return run_daemon_serve(runtime, state_root, drain_timeout, json);
    }
    let exit_code = runtime.block_on(async move {
        match run_commands(cli).await {
            Ok(exit_code) => exit_code,
            Err(error) => commands::run::report_error(&error, json),
        }
    });
    drop(runtime);
    exit_code
}

fn run_daemon_serve(
    runtime: tokio::runtime::Runtime,
    state_root: Option<std::path::PathBuf>,
    drain_timeout: std::time::Duration,
    json: bool,
) -> std::process::ExitCode {
    let outcome = runtime.block_on(async move {
        let mut sigterm =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        let mut sigint = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
        let (stop_tx, stop_rx) = tokio::sync::mpsc::channel(32);
        let signal_task = tokio::spawn(async move {
            loop {
                let received = tokio::select! {
                    res = sigterm.recv() => res,
                    res = sigint.recv() => res,
                };
                if received.is_none() || stop_tx.send(()).await.is_err() {
                    break;
                }
            }
        });
        let result = commands::daemon::serve(state_root.as_deref(), drain_timeout, stop_rx).await;
        signal_task.abort();
        result
    });
    match outcome {
        Ok(lattice_server::local::ServeExit {
            drain,
            failure,
            instance,
            shutdown_deadline,
        }) => {
            let has_failure = failure.is_some();
            if let Some(error) = failure {
                commands::run::report_daemon_failure(&error);
            }
            let code = match drain {
                lattice_server::local::DrainOutcome::TimedOut => 4,
                lattice_server::local::DrainOutcome::Completed => {
                    if has_failure {
                        1
                    } else {
                        0
                    }
                }
            };
            // 关闭 runtime 用第 6 步剩余的时间；实例锁持有到进程退出，由内核在关闭文件描述符时释放。
            runtime.shutdown_timeout(
                shutdown_deadline.saturating_duration_since(std::time::Instant::now()),
            );
            let _held_until_exit = &instance;
            std::process::exit(code);
        }
        Err(error) => {
            let code = commands::run::report_error(&error, json);
            runtime.shutdown_timeout(std::time::Duration::from_secs(5));
            code
        }
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
            let is_stop = match action {
                commands::daemon::DaemonAction::Status => false,
                commands::daemon::DaemonAction::Stop => true,
                commands::daemon::DaemonAction::Serve { .. } => {
                    unreachable!("daemon serve 由专用入口执行")
                }
            };
            return commands::daemon::run_client(is_stop, cli.json).await;
        }
        Some(Commands::Auth { action }) => {
            return commands::auth::run(action, cli.json).await;
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
        Some(Commands::Tui { session_id, agent }) => {
            return lattice_tui::run(lattice_tui::TuiOptions {
                session_id,
                agent,
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
            vec!["auth", "login", "chatgpt"],
            vec!["auth", "logout", "chatgpt"],
        ] {
            assert!(Cli::try_parse_from(std::iter::once("lattice").chain(args)).is_ok());
        }
        for args in [
            vec!["auth", "set"],
            vec!["auth", "set", "openai", "key"],
            vec!["auth", "login", "openai"],
            vec!["auth", "logout", "openai"],
            vec!["auth", "login", "chatgpt", "secret"],
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
        for word in ["set", "remove", "status", "login", "logout"] {
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
                let cli = Cli::try_parse_from(&args).unwrap();
                let choices = match cli.command.unwrap() {
                    Commands::Run(args)
                    | Commands::Session {
                        action: super::session_cmd::SessionAction::Send(args),
                    } => args,
                    _ => panic!("expected submission"),
                };
                assert_eq!(choices.model, None);
                assert_eq!(choices.effort, None);
                for effort in ["off", "minimal", "low", "medium", "high", "xhigh", "max"] {
                    let mut explicit = args.clone();
                    explicit.extend(["--model", "unregistered/org/model", "--effort", effort]);
                    let cli = Cli::try_parse_from(&explicit).unwrap();
                    let choices = match cli.command.unwrap() {
                        Commands::Run(args)
                        | Commands::Session {
                            action: super::session_cmd::SessionAction::Send(args),
                        } => args,
                        _ => panic!("expected submission"),
                    };
                    assert_eq!(
                        choices.model.as_ref().map(ToString::to_string).as_deref(),
                        Some("unregistered/org/model")
                    );
                    assert_eq!(choices.effort.unwrap().as_str(), effort);
                }
                for (flag, value) in [
                    ("--model", "missing-slash"),
                    ("--model", "local/"),
                    ("--model", "Bad/model"),
                    ("--model", "local/control\n"),
                    ("--effort", "none"),
                    ("--effort", "default"),
                    ("--effort", "HIGH"),
                    ("--effort", " high"),
                ] {
                    let mut invalid = args.clone();
                    invalid.extend([flag, value]);
                    assert_eq!(
                        Cli::try_parse_from(&invalid).err().unwrap().kind(),
                        clap::error::ErrorKind::ValueValidation,
                        "{invalid:?}"
                    );
                }
            }
            let mut missing_agent = vec!["lattice"];
            missing_agent.extend(command.clone());
            missing_agent.extend(["hello", "--model", "local/model", "--effort", "off"]);
            assert_eq!(
                Cli::try_parse_from(missing_agent).err().unwrap().kind(),
                clap::error::ErrorKind::MissingRequiredArgument
            );
            let mut root = Cli::command();
            let mut subcommand = &mut root;
            for name in command {
                subcommand = subcommand.find_subcommand_mut(name).unwrap();
            }
            let help = subcommand.render_long_help().to_string();
            for text in [
                "--model",
                "--effort",
                "<provider>/<model>",
                "inherit",
                "off, minimal, low, medium, high, xhigh, max",
            ] {
                assert!(help.contains(text), "missing {text}: {help}");
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

    #[test]
    fn top_level_subcommands_exclude_removed_audit_and_lancet() {
        let command = Cli::command();
        assert_eq!(
            command
                .get_subcommands()
                .map(|c| c.get_name())
                .collect::<Vec<_>>(),
            ["run", "daemon", "auth", "models", "session", "tui"]
        );
        assert!(Cli::try_parse_from(["lattice", "audit"]).is_err());
        assert!(Cli::try_parse_from(["lattice", "lancet"]).is_err());
    }

    #[test]
    fn daemon_serve_drain_timeout_accepts_units_and_rejects_invalid_values() {
        use std::time::Duration;

        let default_cli = Cli::try_parse_from(["lattice", "daemon", "serve"]).unwrap();
        match default_cli.command {
            Some(Commands::Daemon {
                action:
                    crate::commands::daemon::DaemonAction::Serve {
                        state_root,
                        drain_timeout,
                    },
            }) => {
                assert_eq!(state_root, None, "默认不应设置 state_root");
                assert_eq!(
                    drain_timeout,
                    Duration::from_secs(60),
                    "默认排空上限应为 60s"
                );
            }
            _ => panic!("应解析为 daemon serve"),
        }

        for (raw, expected) in [
            ("300ms", Duration::from_millis(300)),
            ("60s", Duration::from_secs(60)),
            ("2m", Duration::from_secs(120)),
            ("60", Duration::from_secs(60)),
            ("0", Duration::ZERO),
            ("0ms", Duration::ZERO),
            ("0s", Duration::ZERO),
            ("0m", Duration::ZERO),
        ] {
            let cli = Cli::try_parse_from(["lattice", "daemon", "serve", "--drain-timeout", raw])
                .unwrap_or_else(|err| panic!("{raw} 应被接受，但失败：{err}"));
            match cli.command {
                Some(Commands::Daemon {
                    action: crate::commands::daemon::DaemonAction::Serve { drain_timeout, .. },
                }) => assert_eq!(drain_timeout, expected, "{raw} 解析结果不符"),
                _ => panic!("应解析为 daemon serve"),
            }
        }

        for invalid in [
            "-1",
            "1h",
            "1.5s",
            "+5",
            " 60s",
            "60s ",
            "60 s",
            "\t60",
            "",
            "ms",
            "s",
            "m",
            "18446744073709551616",
            "307445734561825861m",
        ] {
            let arg = format!("--drain-timeout={invalid}");
            let err = Cli::try_parse_from(["lattice", "daemon", "serve", &arg])
                .err()
                .unwrap_or_else(|| panic!("{invalid:?} 应被拒绝"));
            assert_eq!(
                err.kind(),
                clap::error::ErrorKind::ValueValidation,
                "{invalid:?} 应报 ValueValidation"
            );
            assert!(
                Cli::try_parse_from(["lattice", "daemon", "serve", "--drain-timeout", invalid])
                    .is_err(),
                "分开传参时 {invalid:?} 也应被拒绝"
            );
        }

        let mut root = Cli::command();
        let daemon = root.find_subcommand_mut("daemon").unwrap();
        let serve = daemon.find_subcommand_mut("serve").unwrap();
        let help = serve.render_long_help().to_string();
        assert!(
            help.contains("--drain-timeout"),
            "daemon serve 帮助文本应包含 --drain-timeout: {help}"
        );
    }
}
