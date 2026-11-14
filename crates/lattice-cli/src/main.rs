// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use anyhow::Result;
use clap::{CommandFactory, FromArgMatches, Parser, Subcommand};

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
    #[command(
        help_template = commands::daemon::DAEMON_HELP_TEMPLATE,
        override_usage = "lattice daemon <命令>",
        subcommand_value_name = "命令",
        disable_help_flag = true,
        disable_help_subcommand = true
    )]
    Daemon(commands::daemon::DaemonArgs),
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

fn cli_command() -> clap::Command {
    let mut cmd = Cli::command();
    cmd.build();
    cmd.mut_subcommand("daemon", commands::daemon::localize_help)
}

#[derive(Debug)]
enum CliParseError {
    Clap(clap::Error),
    DaemonInstallJson,
}

impl From<clap::Error> for CliParseError {
    fn from(error: clap::Error) -> Self {
        Self::Clap(error)
    }
}

fn parse_cli(
    args: impl IntoIterator<Item = impl Into<std::ffi::OsString> + Clone>,
) -> Result<Cli, CliParseError> {
    let matches = cli_command().try_get_matches_from(args)?;
    let cli = Cli::from_arg_matches(&matches)?;
    if let Some(Commands::Models {
        auth,
        provider,
        action: Some(_),
    }) = &cli.command
        && (*auth || provider.is_some())
    {
        return Err(CliParseError::Clap(cli_command().error(
            clap::error::ErrorKind::ArgumentConflict,
            "--auth and --provider cannot be used with models refresh",
        )));
    }
    if let Some(Commands::Daemon(daemon)) = &cli.command
        && matches!(daemon.action, commands::daemon::DaemonAction::Install(_))
        && cli.json
    {
        return Err(CliParseError::DaemonInstallJson);
    }
    Ok(cli)
}

fn first_subcommand_arg(args: &[std::ffi::OsString]) -> Option<&std::ffi::OsStr> {
    args.iter()
        .skip(1)
        .take_while(|arg| arg.as_os_str() != "--")
        .find(|arg| !arg.as_encoded_bytes().starts_with(b"-") && arg.as_os_str() != "help")
        .map(std::ffi::OsString::as_os_str)
}

fn args_request_json(args: &[std::ffi::OsString]) -> bool {
    let mut iter = args
        .iter()
        .skip(1)
        .take_while(|arg| arg.as_os_str() != "--");
    while let Some(arg) = iter.next() {
        if arg == "--drain-timeout" {
            let _ = iter.next();
            continue;
        }
        let bytes = arg.as_encoded_bytes();
        if arg == "--json" || bytes.starts_with(b"--json=") {
            return true;
        }
        if bytes.len() > 1 && bytes[0] == b'-' && bytes[1] != b'-' && bytes[1..].contains(&b'j') {
            return true;
        }
    }
    false
}

/// 处理参数解析错误；对于标准帮助/版本显示及其他子命令的默认 clap 文本输出返回 `None`，交由 `clap::Error::print` 输出。
fn render_cli_parse_error(
    args: &[std::ffi::OsString],
    parse_error: &CliParseError,
    out: &mut impl std::io::Write,
    err: &mut impl std::io::Write,
) -> Option<u8> {
    if let CliParseError::Clap(error) = parse_error
        && matches!(
            error.kind(),
            clap::error::ErrorKind::DisplayHelp | clap::error::ErrorKind::DisplayVersion
        )
    {
        return None;
    }

    let subcmd = first_subcommand_arg(args);
    let has_json =
        matches!(parse_error, CliParseError::DaemonInstallJson) || args_request_json(args);
    if has_json {
        let message = if subcmd.is_some_and(|arg| arg == "daemon") {
            match parse_error {
                CliParseError::DaemonInstallJson => {
                    commands::daemon::INSTALL_JSON_UNSUPPORTED_MESSAGE.to_owned()
                }
                CliParseError::Clap(error) => commands::daemon::cli_error_message(error),
            }
        } else {
            "invalid command; use --help".to_owned()
        };
        let _ = writeln!(
            out,
            "{}",
            serde_json::json!({"type":"error", "kind":"bad_request", "message": message})
        );
        return Some(2);
    }

    if subcmd.is_some_and(|arg| arg == "auth") {
        // 包括 help auth 路径在内，clap 错误都可能回显误放在参数中的 key。
        let _ = writeln!(err, "error: invalid auth command; use lattice auth --help");
        return Some(2);
    }
    if subcmd.is_some_and(|arg| arg == "daemon") {
        match parse_error {
            CliParseError::DaemonInstallJson => {
                let _ = writeln!(
                    err,
                    "错误：{}",
                    commands::daemon::INSTALL_JSON_UNSUPPORTED_MESSAGE
                );
            }
            CliParseError::Clap(error) => {
                let _ = writeln!(err, "{}", commands::daemon::format_cli_error(error));
            }
        }
        return Some(2);
    }
    None
}

fn main() -> std::process::ExitCode {
    let raw_args: Vec<std::ffi::OsString> = std::env::args_os().collect();
    let cli = match parse_cli(raw_args.iter().cloned()) {
        Ok(cli) => cli,
        Err(parse_error) => {
            if let Some(code) = render_cli_parse_error(
                &raw_args,
                &parse_error,
                &mut std::io::stdout(),
                &mut std::io::stderr(),
            ) {
                return std::process::ExitCode::from(code);
            }
            if let CliParseError::Clap(error) = parse_error {
                let help = matches!(
                    error.kind(),
                    clap::error::ErrorKind::DisplayHelp | clap::error::ErrorKind::DisplayVersion
                );
                let _ = error.print();
                return std::process::ExitCode::from(if help { 0 } else { 2 });
            }
            return std::process::ExitCode::from(2);
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
    let exit_code = runtime.block_on(async move {
        match run_commands(cli).await {
            Ok(exit_code) => exit_code,
            Err(error) => commands::run::report_error(&error, json),
        }
    });
    drop(runtime);
    exit_code
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
        Some(Commands::Daemon(daemon)) => {
            return commands::daemon::run(daemon.action, cli.json).await;
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
            let _ = cli_command().print_help();
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
    fn daemon_subcommands_expose_status_stop_and_install_with_chinese_help() {
        let mut root = super::cli_command();
        let top_help = root.render_help().to_string();
        assert!(
            top_help.contains("Manage the local daemon for this user and state root"),
            "顶层 lattice --help 列表中 daemon 说明应保持与同列表一致：{top_help}"
        );

        let daemon = root.find_subcommand_mut("daemon").unwrap();
        assert_eq!(
            daemon
                .get_subcommands()
                .map(|c| c.get_name())
                .collect::<Vec<_>>(),
            ["status", "stop", "install"],
            "lattice daemon 应仅暴露 status、stop 与 install，不包含 serve"
        );
        let daemon_help = daemon.render_help().to_string();
        for expected in [
            "管理当前用户与状态根的本地后台守护进程。",
            "用法：lattice daemon <命令>",
            "命令：",
            "status",
            "stop",
            "install",
            "选项：",
            "显示帮助。",
        ] {
            assert!(
                daemon_help.contains(expected),
                "daemon --help 缺少 `{expected}`：{daemon_help}"
            );
        }
        for forbidden in [
            "Usage:",
            "Commands:",
            "Options:",
            "Print help",
            "Verbose logging",
            "Emit JSONL",
            "Manage the local daemon",
        ] {
            assert!(
                !daemon_help.contains(forbidden),
                "daemon --help 不应残留英文 `{forbidden}`：{daemon_help}"
            );
        }

        let install_help = daemon
            .find_subcommand_mut("install")
            .unwrap()
            .render_help()
            .to_string();
        for expected in [
            "不支持 --json",
            "用法：lattice daemon install [--drain-timeout <时长>]",
            "--drain-timeout <时长>",
            "-h, --help",
            "显示帮助。",
        ] {
            assert!(
                install_help.contains(expected),
                "install --help 缺少 `{expected}`：{install_help}"
            );
        }
        for forbidden in [
            "Usage:",
            "Options:",
            "Print help",
            "Verbose logging",
            "-j, --json",
        ] {
            assert!(
                !install_help.contains(forbidden),
                "install --help 不应包含 `{forbidden}`：{install_help}"
            );
        }

        assert!(
            parse_cli(["lattice", "daemon", "serve"]).is_err(),
            "lattice daemon serve 应被拒绝"
        );
        assert!(parse_cli(["lattice", "daemon", "status"]).is_ok());
        assert!(parse_cli(["lattice", "daemon", "stop"]).is_ok());
        assert!(parse_cli(["lattice", "daemon", "install"]).is_ok());
        assert!(parse_cli(["lattice", "daemon", "install", "--drain-timeout", "30s"]).is_ok());
        assert!(parse_cli(["lattice", "--json", "daemon", "install"]).is_err());
    }

    #[test]
    fn daemon_json_errors_and_install_json_rejection_are_chinese_shaped_and_redacted() {
        let mut cmd = super::cli_command();
        let install_sub = cmd
            .find_subcommand_mut("daemon")
            .unwrap()
            .find_subcommand_mut("install")
            .unwrap();
        for help_text in [
            install_sub.render_help().to_string(),
            install_sub.render_long_help().to_string(),
        ] {
            assert!(
                help_text.contains("不支持 --json"),
                "install 帮助应明确写明不支持 --json：{help_text}"
            );
            assert!(
                !help_text.contains("-j, --json"),
                "install 选项列表不应把 --json 列为支持参数：{help_text}"
            );
        }

        let secret = "sk-live-secret-should-not-leak-98765";
        for (args, expected_substr) in [
            (vec!["lattice", "--json", "daemon"], "缺少子命令"),
            (
                vec!["lattice", "--json", "daemon", "status", "--bad"],
                "未知参数",
            ),
            (vec!["lattice", "-j", "daemon", "serve"], "未知子命令"),
            (
                vec!["lattice", "daemon", "install", "--json"],
                "不支持 `--json`",
            ),
            (
                vec!["lattice", "--json", "daemon", "install"],
                "不支持 `--json`",
            ),
            (
                vec!["lattice", "-vj", "daemon", "install"],
                "不支持 `--json`",
            ),
            (
                vec!["lattice", "--json", "daemon", "status", secret],
                "未知参数",
            ),
            (vec!["lattice", "--json", "daemon", secret], "未知子命令"),
            (
                vec![
                    "lattice",
                    "--json",
                    "daemon",
                    "install",
                    "--drain-timeout",
                    secret,
                ],
                "排空时长",
            ),
        ] {
            let os_args: Vec<std::ffi::OsString> = args.iter().map(Into::into).collect();
            let err = super::parse_cli(os_args.iter().cloned())
                .err()
                .expect("非法参数应解析失败");
            let mut out = Vec::new();
            let mut stderr = Vec::new();
            let code = super::render_cli_parse_error(&os_args, &err, &mut out, &mut stderr);
            assert_eq!(code, Some(2), "{args:?} 应退出 2");
            assert!(stderr.is_empty(), "{args:?} JSON 模式下标准错误应为空");
            let stdout = String::from_utf8(out).unwrap();
            assert!(
                !stdout.contains(secret),
                "{args:?} 错误输出不得回显敏感输入：{stdout}"
            );
            let parsed: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
            let obj = parsed.as_object().expect("应输出 JSON 对象");
            assert_eq!(obj.len(), 3, "JSON 错误结构字段数不变：{parsed}");
            assert_eq!(parsed["type"], "error");
            assert_eq!(parsed["kind"], "bad_request");
            let msg = parsed["message"].as_str().expect("message 应为字符串");
            assert!(
                msg.contains(expected_substr) && !msg.contains("invalid command"),
                "{args:?} 期望包含 `{expected_substr}` 且无英文回退，实际：{msg}"
            );
        }

        // 非 daemon 组（含 auth）继续使用既有脱敏 JSON 错误文案。
        let auth_args: Vec<std::ffi::OsString> =
            ["lattice", "--json", "auth", "set", "openai", secret]
                .iter()
                .map(Into::into)
                .collect();
        let auth_err = super::parse_cli(auth_args.iter().cloned()).err().unwrap();
        let mut auth_out = Vec::new();
        let mut auth_stderr = Vec::new();
        assert_eq!(
            super::render_cli_parse_error(&auth_args, &auth_err, &mut auth_out, &mut auth_stderr),
            Some(2)
        );
        let auth_stdout = String::from_utf8(auth_out).unwrap();
        assert!(!auth_stdout.contains(secret));
        let auth_json: serde_json::Value = serde_json::from_str(auth_stdout.trim()).unwrap();
        assert_eq!(auth_json["message"], "invalid command; use --help");
    }
}
