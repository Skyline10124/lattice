// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! `lattice daemon` 子命令组分派、中文帮助定制与状态输出渲染。

mod install;

use std::{io::Write, process::ExitCode};

use clap::{ArgAction, Args, Subcommand};
use lattice_local::{DaemonStatus, LocalClient, LocalError, LocalErrorKind, StateLayout};

pub(crate) use install::InstallArgs;

/// `lattice daemon` 子命令组帮助模板：使用独立中文说明，保持顶层 `lattice --help` 列表风格一致。
pub(crate) const DAEMON_HELP_TEMPLATE: &str = "\
管理当前用户与状态根的本地后台守护进程。

用法：{usage}

命令：
{subcommands}

选项：
{options}";

/// `lattice daemon` 下级子命令帮助模板。
const SUBCOMMAND_HELP_TEMPLATE: &str = "{about-with-newline}\n用法：{usage}\n\n选项：\n{options}";

#[derive(Args, Debug, Clone)]
pub(crate) struct DaemonArgs {
    /// 显示帮助。
    #[arg(
        short = 'h',
        long = "help",
        action = ArgAction::Help,
        display_order = 1000
    )]
    help: Option<bool>,

    #[command(subcommand)]
    pub(crate) action: DaemonAction,
}

#[derive(Subcommand, Debug, Clone)]
pub(crate) enum DaemonAction {
    /// 查询运行中的本地后台状态（不会拉起后台）。
    #[command(
        help_template = SUBCOMMAND_HELP_TEMPLATE,
        override_usage = "lattice daemon status [--json]",
        disable_help_flag = true
    )]
    Status {
        /// 显示帮助。
        #[arg(
            short = 'h',
            long = "help",
            action = ArgAction::Help,
            display_order = 1000
        )]
        help: Option<bool>,
    },
    /// 停止接收新请求，等待已接管工作与持久化收尾完成并释放状态根。
    #[command(
        help_template = SUBCOMMAND_HELP_TEMPLATE,
        override_usage = "lattice daemon stop [--json]",
        disable_help_flag = true
    )]
    Stop {
        /// 显示帮助。
        #[arg(
            short = 'h',
            long = "help",
            action = ArgAction::Help,
            display_order = 1000
        )]
        help: Option<bool>,
    },
    /// 写出用户级 systemd 服务单元（lattice.service）并启用与启动后台（不支持 --json）。
    #[command(
        help_template = SUBCOMMAND_HELP_TEMPLATE,
        override_usage = "lattice daemon install [--drain-timeout <时长>]",
        disable_help_flag = true
    )]
    Install(InstallArgs),
}

/// `lattice daemon install` 搭配 `--json` / `-j` 时返回的固定中文错误文案。
pub(crate) const INSTALL_JSON_UNSUPPORTED_MESSAGE: &str =
    "`lattice daemon install` 不支持 `--json`；执行 `lattice daemon install --help` 查看用法";

/// 将根命令向下传播的全局选项在 `daemon` 子命令组内替换为中文说明，并在 `install` 中隐藏不使用的 `--json`。
pub(crate) fn localize_help(cmd: clap::Command) -> clap::Command {
    let localize_common = |sub: clap::Command, hide_json: bool| {
        sub.mut_arg("json", |arg| {
            if hide_json {
                arg.hide(true)
            } else {
                arg.help("以 JSON 输出结果，不打印人类可读文本。")
            }
        })
        .mut_arg("verbose", |arg| arg.help("输出详细日志。"))
    };

    localize_common(cmd, false)
        .mut_subcommand("status", |sub| localize_common(sub, false))
        .mut_subcommand("stop", |sub| localize_common(sub, false))
        .mut_subcommand("install", |sub| localize_common(sub, true))
}

/// 生成 `lattice daemon` 组参数解析错误的已脱敏中文正文（不含 `错误：` 前缀，供文本与 JSON 共用）。
pub(crate) fn cli_error_message(error: &clap::Error) -> String {
    use std::error::Error as _;
    match error.kind() {
        clap::error::ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand
        | clap::error::ErrorKind::MissingSubcommand => {
            "缺少子命令；执行 `lattice daemon --help` 查看用法".into()
        }
        clap::error::ErrorKind::InvalidSubcommand => {
            "未知子命令；执行 `lattice daemon --help` 查看用法".into()
        }
        clap::error::ErrorKind::InvalidValue | clap::error::ErrorKind::ValueValidation => {
            if let Some(source) = error.source() {
                let msg = source.to_string();
                if msg.starts_with("排空时长") {
                    return format!("{msg}；执行 `lattice daemon install --help` 查看用法");
                }
            }
            "参数值无效；执行 `lattice daemon --help` 查看用法".into()
        }
        clap::error::ErrorKind::ArgumentConflict => {
            "参数组合冲突；执行 `lattice daemon --help` 查看用法".into()
        }
        clap::error::ErrorKind::UnknownArgument => {
            "包含未知参数；执行 `lattice daemon --help` 查看用法".into()
        }
        clap::error::ErrorKind::TooManyValues | clap::error::ErrorKind::TooFewValues => {
            "参数数量不正确；执行 `lattice daemon --help` 查看用法".into()
        }
        _ => "命令行参数无效；执行 `lattice daemon --help` 查看用法".into(),
    }
}

/// 将 `lattice daemon` 组的 clap 参数解析错误格式化为标准错误中文提示。
pub(crate) fn format_cli_error(error: &clap::Error) -> String {
    format!("错误：{}", cli_error_message(error))
}

pub(crate) async fn run(action: DaemonAction, json: bool) -> anyhow::Result<ExitCode> {
    match action {
        DaemonAction::Status { .. } => run_client(false, json).await,
        DaemonAction::Stop { .. } => run_client(true, json).await,
        DaemonAction::Install(args) => Ok(install::run(args).await),
    }
}

async fn run_client(is_stop: bool, json: bool) -> anyhow::Result<ExitCode> {
    let layout = match StateLayout::resolve() {
        Ok(layout) => layout,
        Err(_) => {
            let msg = "无法解析状态根目录：请设置绝对路径的 XDG_STATE_HOME 或 HOME";
            if json {
                return Err(LocalError::new(LocalErrorKind::Internal, msg).into());
            }
            eprintln!("错误：{msg}");
            return Ok(ExitCode::from(1));
        }
    };
    let client = LocalClient::new(layout);
    let status = if is_stop {
        client.stop().await
    } else {
        client.status().await
    };
    let status = match status {
        Ok(status) => status,
        Err(error) => {
            if json {
                return Err(error.into());
            }
            eprintln!("错误：{}", error.message);
            if let Some(store) = &error.store
                && let Ok(raw) = serde_json::to_string(store)
            {
                eprintln!("{raw}");
            }
            let code = match error.kind {
                LocalErrorKind::BadRequest
                | LocalErrorKind::NotFound
                | LocalErrorKind::Conflict => 2,
                _ => 1,
            };
            return Ok(ExitCode::from(code));
        }
    };
    match render_daemon_status(
        &status,
        is_stop,
        json,
        &mut std::io::stdout(),
        &mut std::io::stderr(),
    ) {
        Ok(code) => Ok(code),
        Err(error) if json => Err(LocalError::new(
            LocalErrorKind::Internal,
            &format!("写出后台状态失败：{error}"),
        )
        .into()),
        Err(error) => {
            eprintln!("错误：写出后台状态失败：{error}");
            Ok(ExitCode::from(1))
        }
    }
}

fn render_daemon_status(
    status: &DaemonStatus,
    is_stop: bool,
    json: bool,
    out: &mut impl Write,
    err: &mut impl Write,
) -> anyhow::Result<ExitCode> {
    if json {
        writeln!(out, "{}", serde_json::to_string(status)?)?;
    } else if is_stop {
        writeln!(
            out,
            "后台已停止（代次：{}，剩余在途执行者：{}，未确认收尾操作：{}）",
            status.epoch, status.owners, status.unconfirmed_operations
        )?;
    } else {
        let state_label = if status.stopping {
            "停机中"
        } else {
            "运行中"
        };
        writeln!(
            out,
            "后台状态：{state_label}（代次：{}，在途执行者：{}，未确认收尾操作：{}）",
            status.epoch, status.owners, status.unconfirmed_operations
        )?;
    }
    if is_stop && status.unconfirmed_operations != 0 {
        writeln!(
            err,
            "有 {} 个操作没有确认收尾，其中的在途 turn 在后台重启后显示为执行者消失。",
            status.unconfirmed_operations
        )?;
    }
    Ok(ExitCode::from(u8::from(status.unconfirmed_operations != 0)))
}

#[cfg(test)]
mod tests {
    use super::render_daemon_status;
    use lattice_local::DaemonStatus;

    #[test]
    fn daemon_stop_warns_on_stderr_when_operations_are_unconfirmed_while_status_stays_silent() {
        let unconfirmed = DaemonStatus {
            epoch: "7".into(),
            stopping: true,
            owners: 2,
            unconfirmed_operations: 2,
        };
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = render_daemon_status(&unconfirmed, true, true, &mut out, &mut err).unwrap();
        assert_eq!(
            code,
            std::process::ExitCode::from(1),
            "未确认操作不为 0 时应退出 1"
        );
        assert_eq!(
            serde_json::from_slice::<DaemonStatus>(&out).unwrap(),
            unconfirmed,
            "--json 标准输出形状不应改变"
        );
        let stderr = String::from_utf8(err).unwrap();
        assert!(
            stderr.contains('2')
                && stderr.contains("没有确认收尾")
                && stderr.contains("执行者消失"),
            "daemon stop 应在标准错误输出未确认收尾提示，实际：{stderr:?}"
        );

        let mut status_out = Vec::new();
        let mut status_err = Vec::new();
        let status_code =
            render_daemon_status(&unconfirmed, false, false, &mut status_out, &mut status_err)
                .unwrap();
        assert_eq!(status_code, std::process::ExitCode::from(1));
        let status_stdout = String::from_utf8(status_out).unwrap();
        assert!(
            status_stdout.contains("后台状态：停机中")
                && status_stdout.contains("代次：7")
                && status_stdout.contains("在途执行者：2")
                && status_stdout.contains("未确认收尾操作：2"),
            "人类可读的 daemon status 应输出中文状态行，实际：{status_stdout:?}"
        );
        assert!(
            status_err.is_empty(),
            "daemon status 不应输出标准错误提示，实际：{:?}",
            String::from_utf8(status_err)
        );

        let running = DaemonStatus {
            epoch: "9".into(),
            stopping: false,
            owners: 0,
            unconfirmed_operations: 0,
        };
        let mut human_stop_out = Vec::new();
        let mut human_stop_err = Vec::new();
        let human_stop_code = render_daemon_status(
            &running,
            true,
            false,
            &mut human_stop_out,
            &mut human_stop_err,
        )
        .unwrap();
        assert_eq!(human_stop_code, std::process::ExitCode::SUCCESS);
        let human_stop_stdout = String::from_utf8(human_stop_out).unwrap();
        assert!(
            human_stop_stdout.contains("后台已停止") && human_stop_stdout.contains("代次：9"),
            "人类可读的 daemon stop 应输出中文已停止提示，实际：{human_stop_stdout:?}"
        );
        assert!(human_stop_err.is_empty());
    }
}
