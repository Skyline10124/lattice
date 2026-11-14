// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! LATTICE 本地后台守护进程入口。

mod notify;

use std::{
    io::{self, IsTerminal},
    path::{Path, PathBuf},
    process::ExitCode,
    time::{Duration, Instant},
};

use clap::{ArgAction, Parser, builder::TypedValueParser as _};
use lattice_ipc::{DrainTimeout, LocalError, LocalErrorKind, parse_drain_timeout};
use lattice_server::{DrainOutcome, HostLifecycle, ServeExit};
use lattice_store::StateLayout;
use rustix::net::{AddressFamily, SocketAddrUnix, SocketFlags, SocketType, connect, socket_with};
use tracing_subscriber::EnvFilter;

use crate::notify::Notifier;

/// 中文帮助模板：替换 clap 默认的英文段落标题。
const HELP_TEMPLATE: &str = "{about-with-newline}
用法：{usage}

选项：
{options}";

#[derive(Debug, Parser)]
#[command(
    name = "lattice-daemon",
    about = "运行 LATTICE 本地后台守护进程。",
    help_template = HELP_TEMPLATE,
    override_usage = "lattice-daemon --state-root <绝对路径> [--drain-timeout <时长>]",
    disable_help_flag = true,
    disable_version_flag = true
)]
struct Cli {
    /// 状态根目录的绝对路径（不会回退到环境变量解析）。
    #[arg(
        long = "state-root",
        value_name = "绝对路径",
        value_parser = clap::builder::OsStringValueParser::new().try_map(|raw| parse_state_root(PathBuf::from(raw)))
    )]
    state_root: PathBuf,

    /// 停机时等待在途操作排空的上限（非负整数，单位支持 ms、s、m，无单位按秒计；默认 60s）。
    #[arg(
        long = "drain-timeout",
        value_name = "时长",
        default_value = "60s",
        hide_default_value = true,
        allow_hyphen_values = true,
        value_parser = parse_drain_timeout
    )]
    drain_timeout: DrainTimeout,

    /// 显示帮助。
    #[arg(
        short = 'h',
        long = "help",
        action = ArgAction::Help,
        display_order = 1000
    )]
    help: Option<bool>,
}

fn parse_state_root(path: PathBuf) -> Result<PathBuf, String> {
    if path.as_os_str().is_empty() {
        return Err("`--state-root` 不能为空".into());
    }
    if !path.is_absolute() {
        return Err("`--state-root` 必须是绝对路径".into());
    }
    Ok(path)
}

fn format_cli_error(error: &clap::Error) -> String {
    use std::error::Error as _;
    match error.kind() {
        clap::error::ErrorKind::MissingRequiredArgument => {
            "错误：缺少必填参数 `--state-root <绝对路径>`；执行 `lattice-daemon --help` 查看用法"
                .into()
        }
        clap::error::ErrorKind::InvalidValue | clap::error::ErrorKind::ValueValidation => {
            if let Some(source) = error.source() {
                format!("错误：{source}；执行 `lattice-daemon --help` 查看用法")
            } else {
                "错误：参数值无效；执行 `lattice-daemon --help` 查看用法".into()
            }
        }
        clap::error::ErrorKind::UnknownArgument => {
            "错误：包含未知参数；执行 `lattice-daemon --help` 查看用法".into()
        }
        clap::error::ErrorKind::TooManyValues | clap::error::ErrorKind::TooFewValues => {
            "错误：参数数量不正确；执行 `lattice-daemon --help` 查看用法".into()
        }
        _ => "错误：命令行参数无效；执行 `lattice-daemon --help` 查看用法".into(),
    }
}

fn init_logging() {
    let filter = EnvFilter::try_from_env("LATTICE_LOG").unwrap_or_else(|_| EnvFilter::new("error"));
    let ansi = io::stderr().is_terminal() && std::env::var_os("NO_COLOR").is_none();
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(io::stderr)
        .compact()
        .with_ansi(ansi)
        .try_init();
}

struct DaemonLifecycle {
    notifier: Notifier,
}

impl HostLifecycle for DaemonLifecycle {
    fn on_ready(&self) -> lattice_ipc::Result<()> {
        self.notifier.notify_ready().map_err(|error| {
            LocalError::new(
                LocalErrorKind::Internal,
                &format!("发送 systemd 就绪通知失败：{error}"),
            )
        })
    }

    fn on_stopping(&self) {
        if let Err(error) = self.notifier.notify_stopping() {
            tracing::warn!("发送 systemd 停机通知失败：{error}");
        }
    }
}

/// 状态根已被占用时，连接已发布的 `api.sock` 并通过 `SO_PEERCRED` 读取对端进程号。
/// 为覆盖胜出进程刚拿到 `owner.lock`、正在原子重命名 `api.sock` 的短暂窗口，在有界时间内短暂重试连接。
fn probe_existing_owner_pid(layout: &StateLayout) -> Option<i32> {
    let deadline = Instant::now() + Duration::from_millis(500);
    loop {
        let root = std::fs::canonicalize(layout.lattice_root())
            .unwrap_or_else(|_| layout.lattice_root().to_path_buf());
        let socket_path: PathBuf = root.join("local").join("api.sock");
        if let Ok(addr) = SocketAddrUnix::new(Path::new(&socket_path))
            && let Ok(sock) = socket_with(
                AddressFamily::UNIX,
                SocketType::STREAM,
                SocketFlags::CLOEXEC | SocketFlags::NONBLOCK,
                None,
            )
        {
            match connect(&sock, &addr) {
                Ok(()) => return lattice_ipc::ipc::peer_pid(&sock),
                Err(errno) => {
                    let kind = io::Error::from(errno).kind();
                    if !matches!(
                        kind,
                        io::ErrorKind::NotFound
                            | io::ErrorKind::ConnectionRefused
                            | io::ErrorKind::WouldBlock
                    ) {
                        return None;
                    }
                }
            }
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn format_already_running_diagnostic(peer_pid: Option<i32>) -> String {
    match peer_pid {
        Some(pid) => format!("错误：状态根已被另一个后台占用（对端进程号：{pid}）"),
        None => "错误：状态根已被另一个后台占用（对端进程号不可用）".into(),
    }
}

fn map_serve_exit_code(drain: DrainOutcome, has_failure: bool) -> i32 {
    match drain {
        DrainOutcome::TimedOut => 4,
        DrainOutcome::Completed => {
            if has_failure {
                1
            } else {
                0
            }
        }
    }
}

fn main() -> ExitCode {
    let cli = match Cli::try_parse_from(std::env::args_os()) {
        Ok(cli) => cli,
        Err(error) => {
            if error.kind() == clap::error::ErrorKind::DisplayHelp {
                let _ = error.print();
                return ExitCode::from(0);
            }
            eprintln!("{}", format_cli_error(&error));
            return ExitCode::from(2);
        }
    };

    init_logging();
    let notifier = Notifier::from_env();
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("错误：构建异步运行时失败：{error}");
            return ExitCode::from(1);
        }
    };

    let layout = StateLayout::in_state_root(&cli.state_root);
    let serve_layout = layout.clone();
    let drain_timeout = cli.drain_timeout.as_duration();
    let outcome: Result<lattice_ipc::Result<ServeExit>, io::Error> = runtime.block_on(async move {
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
        let result = lattice_server::serve(
            serve_layout,
            std::env::var_os("HOME").map(PathBuf::from),
            drain_timeout,
            stop_rx,
            DaemonLifecycle { notifier },
        )
        .await;
        signal_task.abort();
        Ok(result)
    });

    match outcome {
        Ok(Ok(ServeExit {
            drain,
            failure,
            instance,
            shutdown_deadline,
        })) => {
            let has_failure = failure.is_some();
            if let Some(error) = failure {
                eprintln!("错误：{}", error.message);
            }
            let code = map_serve_exit_code(drain, has_failure);
            // 关闭 runtime 用停机第 6 步剩余的时间；实例锁持有到进程退出，由内核在关闭文件描述符时释放。
            runtime.shutdown_timeout(shutdown_deadline.saturating_duration_since(Instant::now()));
            let _held_until_exit = &instance;
            std::process::exit(code);
        }
        Ok(Err(error)) => {
            let code = if error.kind == LocalErrorKind::AlreadyRunning {
                let pid = probe_existing_owner_pid(&layout);
                eprintln!("{}", format_already_running_diagnostic(pid));
                3
            } else {
                eprintln!("错误：{}", error.message);
                1
            };
            runtime.shutdown_timeout(Duration::from_secs(5));
            std::process::exit(code);
        }
        Err(signal_error) => {
            eprintln!("错误：注册停机信号失败：{signal_error}");
            runtime.shutdown_timeout(Duration::from_secs(5));
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory as _;

    #[test]
    fn cli_help_is_chinese_and_validates_state_root_and_drain_timeout() {
        let help = Cli::command().render_long_help().to_string();
        for required in [
            "用法：lattice-daemon --state-root <绝对路径> [--drain-timeout <时长>]",
            "选项：",
            "--state-root <绝对路径>",
            "--drain-timeout <时长>",
            "-h, --help",
            "显示帮助。",
        ] {
            assert!(
                help.contains(required),
                "帮助输出缺少 `{required}`，实际：
{help}"
            );
        }
        for forbidden in ["Usage:", "Options:", "[default:"] {
            assert!(
                !help.contains(forbidden),
                "帮助输出不应混入英文段落或默认值标记 `{forbidden}`，实际：
{help}"
            );
        }

        let parsed = Cli::try_parse_from(["lattice-daemon", "--state-root", "/tmp/lattice-state"])
            .expect("合法绝对路径应解析成功");
        assert_eq!(parsed.state_root, PathBuf::from("/tmp/lattice-state"));
        assert_eq!(parsed.drain_timeout.as_duration(), Duration::from_secs(60));

        let parsed_ms = Cli::try_parse_from([
            "lattice-daemon",
            "--state-root",
            "/tmp/lattice-state",
            "--drain-timeout",
            "250ms",
        ])
        .expect("带单位排空时长应解析成功");
        assert_eq!(
            parsed_ms.drain_timeout.as_duration(),
            Duration::from_millis(250)
        );

        let missing = Cli::try_parse_from(["lattice-daemon"]).unwrap_err();
        assert_eq!(
            missing.kind(),
            clap::error::ErrorKind::MissingRequiredArgument
        );
        assert!(format_cli_error(&missing).contains("--state-root"));

        for relative in ["relative/path", "./state", "../state", ""] {
            let err =
                Cli::try_parse_from(["lattice-daemon", "--state-root", relative]).unwrap_err();
            let msg = format_cli_error(&err);
            assert!(
                msg.contains("--state-root"),
                "相对或空路径 `{relative}` 应报中文 `--state-root` 错误，实际：{msg}"
            );
        }

        for bad_timeout in ["-1", "1h", "1.5s", "", "18446744073709551616"] {
            let err = Cli::try_parse_from([
                "lattice-daemon",
                "--state-root",
                "/tmp/lattice-state",
                "--drain-timeout",
                bad_timeout,
            ])
            .unwrap_err();
            let msg = format_cli_error(&err);
            assert!(
                msg.contains("排空时长"),
                "非法时长 `{bad_timeout}` 应保留中文错误信息，实际：{msg}"
            );
        }
    }

    #[test]
    fn exit_code_mapping_prioritizes_timed_out_over_failure() {
        assert_eq!(map_serve_exit_code(DrainOutcome::Completed, false), 0);
        assert_eq!(map_serve_exit_code(DrainOutcome::Completed, true), 1);
        assert_eq!(map_serve_exit_code(DrainOutcome::TimedOut, false), 4);
        assert_eq!(map_serve_exit_code(DrainOutcome::TimedOut, true), 4);
        assert_eq!(
            format_already_running_diagnostic(Some(4321)),
            "错误：状态根已被另一个后台占用（对端进程号：4321）"
        );
        assert_eq!(
            format_already_running_diagnostic(None),
            "错误：状态根已被另一个后台占用（对端进程号不可用）"
        );
    }
}
