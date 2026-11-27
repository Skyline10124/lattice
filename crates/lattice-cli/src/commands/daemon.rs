// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! `lattice daemon` 子命令组分派与状态输出渲染。

mod install;

use std::{io::Write, process::ExitCode};

use clap::Subcommand;
use lattice_local::{
    ComponentState, DaemonStatus, LocalClient, LocalError, LocalErrorKind, StateLayout, StopOutcome,
};

pub(crate) use install::InstallArgs;

#[derive(Subcommand, Debug, Clone)]
pub(crate) enum DaemonAction {
    /// 查看运行中的后台状态
    Status,
    /// 启动已安装的用户级 systemd 服务
    Start,
    /// 重启已安装的用户级 systemd 服务
    Restart,
    /// 停止后台并等待在途工作收尾
    Stop,
    /// 安装并启动用户级 systemd 服务
    Install(InstallArgs),
}

impl DaemonAction {
    /// 对不支持 `--json` 的子命令返回完整命令名（不含 `lattice ` 前缀）。
    pub(crate) fn json_unsupported_name(&self) -> Option<&'static str> {
        match self {
            Self::Install(_) => Some("daemon install"),
            Self::Start => Some("daemon start"),
            Self::Restart => Some("daemon restart"),
            Self::Status | Self::Stop => None,
        }
    }
}

pub(crate) async fn run(action: DaemonAction, json: bool) -> anyhow::Result<ExitCode> {
    match action {
        DaemonAction::Status => run_status(json).await,
        DaemonAction::Start => Ok(install::run_start().await),
        DaemonAction::Restart => Ok(install::run_restart().await),
        DaemonAction::Stop => run_stop(json).await,
        DaemonAction::Install(args) => Ok(install::run(args).await),
    }
}

fn resolve_client(json: bool) -> anyhow::Result<Result<LocalClient, ExitCode>> {
    match StateLayout::resolve() {
        Ok(layout) => Ok(Ok(LocalClient::new(layout))),
        Err(_) => {
            let msg = "无法解析状态根目录：请设置绝对路径的 XDG_STATE_HOME 或 HOME";
            if json {
                return Err(LocalError::new(LocalErrorKind::Internal, msg).into());
            }
            eprintln!("错误：{msg}");
            Ok(Err(ExitCode::from(1)))
        }
    }
}

fn report_daemon_error(error: LocalError, json: bool) -> anyhow::Result<ExitCode> {
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
        LocalErrorKind::BadRequest | LocalErrorKind::NotFound | LocalErrorKind::Conflict => 2,
        _ => 1,
    };
    Ok(ExitCode::from(code))
}

async fn run_status(json: bool) -> anyhow::Result<ExitCode> {
    let client = match resolve_client(json)? {
        Ok(client) => client,
        Err(code) => return Ok(code),
    };
    let status = match client.status().await {
        Ok(status) => status,
        Err(error) => return report_daemon_error(error, json),
    };
    match render_daemon_status(
        &status,
        false,
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

async fn run_stop(json: bool) -> anyhow::Result<ExitCode> {
    let client = match resolve_client(json)? {
        Ok(client) => client,
        Err(code) => return Ok(code),
    };
    let outcome = match client.stop().await {
        Ok(outcome) => outcome,
        Err(error) => return report_daemon_error(error, json),
    };
    match render_stop_outcome(
        &outcome,
        json,
        &mut std::io::stdout(),
        &mut std::io::stderr(),
    ) {
        Ok(code) => Ok(code),
        Err(error) if json => Err(LocalError::new(
            LocalErrorKind::Internal,
            &format!("写出后台停机结果失败：{error}"),
        )
        .into()),
        Err(error) => {
            eprintln!("错误：写出后台停机结果失败：{error}");
            Ok(ExitCode::from(1))
        }
    }
}

fn render_stop_outcome(
    outcome: &StopOutcome,
    json: bool,
    out: &mut impl Write,
    err: &mut impl Write,
) -> anyhow::Result<ExitCode> {
    match outcome {
        StopOutcome::Status(status) => render_daemon_status(status, true, json, out, err),
        StopOutcome::CrossVersionControl {
            epoch,
            owners,
            unconfirmed_operations,
        } => {
            if json {
                writeln!(
                    out,
                    "{}",
                    serde_json::json!({
                        "cross_version": true,
                        "method": "control",
                        "epoch": epoch,
                        "stopping": true,
                        "owners": owners,
                        "unconfirmed_operations": unconfirmed_operations,
                    })
                )?;
            } else {
                writeln!(
                    out,
                    "后台已停止（跨版本控制停机，代次：{epoch}，剩余在途执行者：{owners}，未确认收尾操作：{unconfirmed_operations}）"
                )?;
            }
            if *unconfirmed_operations != 0 {
                writeln!(
                    err,
                    "有 {unconfirmed_operations} 个操作没有确认收尾，其中的在途 turn 在后台重启后显示为执行者消失。"
                )?;
            }
            Ok(ExitCode::from(u8::from(*unconfirmed_operations != 0)))
        }
        StopOutcome::CrossVersionSignal { peer_pid } => {
            if json {
                writeln!(
                    out,
                    "{}",
                    serde_json::json!({
                        "cross_version": true,
                        "method": "signal",
                        "peer_pid": peer_pid,
                    })
                )?;
            } else {
                writeln!(
                    out,
                    "已向旧版本后台（对端进程号：{peer_pid}）发送停机信号，状态根已释放。"
                )?;
            }
            writeln!(
                err,
                "跨版本信号停机无法读取排空计数；如有在途 turn，请在启动新后台后检查对应会话状态。"
            )?;
            Ok(ExitCode::SUCCESS)
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
        if status.components.is_empty() {
            writeln!(out, "组件：无")?;
        } else {
            writeln!(out, "组件：")?;
            for component in &status.components {
                match &component.state {
                    ComponentState::Mounted {
                        configuration_error: None,
                    } => {
                        writeln!(out, "  {}：已挂载", component.name)?;
                    }
                    ComponentState::Mounted {
                        configuration_error: Some(reason),
                    } => {
                        writeln!(
                            out,
                            "  {}：已挂载；仍按上一份配置运行；{reason}",
                            component.name
                        )?;
                    }
                    ComponentState::Unmounted { reason } => {
                        writeln!(out, "  {}：未挂载；{reason}", component.name)?;
                    }
                }
            }
        }
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
    use super::{render_daemon_status, render_stop_outcome};
    use lattice_local::{ComponentState, ComponentStatus, DaemonStatus, StopOutcome};

    #[test]
    fn daemon_stop_warns_on_stderr_when_operations_are_unconfirmed_while_status_stays_silent() {
        let unconfirmed = DaemonStatus {
            epoch: "7".into(),
            stopping: true,
            owners: 2,
            unconfirmed_operations: 2,
            components: vec![],
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
                && status_stdout.contains("未确认收尾操作：2")
                && status_stdout.contains("组件：无"),
            "人类可读的 daemon status 应输出中文状态行与无组件行，实际：{status_stdout:?}"
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
            components: vec![],
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

    #[test]
    fn daemon_status_renders_component_states_in_text_and_json_while_preserving_exit_code() {
        let with_components = DaemonStatus {
            epoch: "12".into(),
            stopping: false,
            owners: 1,
            unconfirmed_operations: 0,
            components: vec![
                ComponentStatus {
                    name: "alpha".into(),
                    state: ComponentState::Mounted {
                        configuration_error: None,
                    },
                },
                ComponentStatus {
                    name: "beta".into(),
                    state: ComponentState::Mounted {
                        configuration_error: Some("workspace 不是绝对路径".into()),
                    },
                },
                ComponentStatus {
                    name: "im".into(),
                    state: ComponentState::Unmounted {
                        reason: "缺少 Telegram 平台凭据（im/auth.json）".into(),
                    },
                },
            ],
        };

        let mut text_out = Vec::new();
        let mut text_err = Vec::new();
        let code =
            render_daemon_status(&with_components, false, false, &mut text_out, &mut text_err)
                .unwrap();
        assert_eq!(
            code,
            std::process::ExitCode::SUCCESS,
            "可选组件未挂载不应改变 daemon status 的退出码 0"
        );
        assert!(text_err.is_empty());
        assert_eq!(
            String::from_utf8(text_out).unwrap(),
            concat!(
                "后台状态：运行中（代次：12，在途执行者：1，未确认收尾操作：0）\n",
                "组件：\n",
                "  alpha：已挂载\n",
                "  beta：已挂载；仍按上一份配置运行；workspace 不是绝对路径\n",
                "  im：未挂载；缺少 Telegram 平台凭据（im/auth.json）\n",
            )
        );

        let mut json_out = Vec::new();
        let mut json_err = Vec::new();
        let json_code =
            render_daemon_status(&with_components, false, true, &mut json_out, &mut json_err)
                .unwrap();
        assert_eq!(json_code, std::process::ExitCode::SUCCESS);
        assert!(json_err.is_empty());
        assert_eq!(
            serde_json::from_slice::<DaemonStatus>(&json_out).unwrap(),
            with_components
        );
    }

    #[test]
    fn cross_version_stop_outcomes_render_clean_text_json_warnings_and_exit_codes() {
        // 1. 跨版本控制帧（无未确认操作）：退出 0，JSON 使用显式跨版本形状且不伪造 components。
        let control_clean = StopOutcome::CrossVersionControl {
            epoch: "15".into(),
            owners: 0,
            unconfirmed_operations: 0,
        };
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = render_stop_outcome(&control_clean, true, &mut out, &mut err).unwrap();
        assert_eq!(code, std::process::ExitCode::SUCCESS);
        assert!(err.is_empty());
        let parsed: serde_json::Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(
            parsed,
            serde_json::json!({
                "cross_version": true,
                "method": "control",
                "epoch": "15",
                "stopping": true,
                "owners": 0,
                "unconfirmed_operations": 0,
            })
        );

        let mut text_out = Vec::new();
        let mut text_err = Vec::new();
        let text_code =
            render_stop_outcome(&control_clean, false, &mut text_out, &mut text_err).unwrap();
        assert_eq!(text_code, std::process::ExitCode::SUCCESS);
        assert!(text_err.is_empty());
        let text = String::from_utf8(text_out).unwrap();
        assert!(
            text.contains("后台已停止")
                && text.contains("跨版本控制停机")
                && text.contains("代次：15"),
            "人类可读跨版本控制停机输出不符：{text:?}"
        );

        // 2. 跨版本控制帧（存在未确认收尾操作）：退出 1，stderr 给出明确警告。
        let control_unconfirmed = StopOutcome::CrossVersionControl {
            epoch: "16".into(),
            owners: 1,
            unconfirmed_operations: 2,
        };
        let mut unc_out = Vec::new();
        let mut unc_err = Vec::new();
        let unc_code =
            render_stop_outcome(&control_unconfirmed, true, &mut unc_out, &mut unc_err).unwrap();
        assert_eq!(unc_code, std::process::ExitCode::from(1));
        let unc_stderr = String::from_utf8(unc_err).unwrap();
        assert!(
            unc_stderr.contains('2')
                && unc_stderr.contains("没有确认收尾")
                && unc_stderr.contains("执行者消失"),
            "跨版本控制停机未确认收尾提示不符：{unc_stderr:?}"
        );

        // 3. 跨版本信号兜底：退出 0，JSON 输出 signal 形状，stderr 注明无法读取排空计数。
        let signal_stop = StopOutcome::CrossVersionSignal { peer_pid: 1248 };
        let mut sig_json_out = Vec::new();
        let mut sig_json_err = Vec::new();
        let sig_json_code =
            render_stop_outcome(&signal_stop, true, &mut sig_json_out, &mut sig_json_err).unwrap();
        assert_eq!(sig_json_code, std::process::ExitCode::SUCCESS);
        let sig_parsed: serde_json::Value = serde_json::from_slice(&sig_json_out).unwrap();
        assert_eq!(
            sig_parsed,
            serde_json::json!({
                "cross_version": true,
                "method": "signal",
                "peer_pid": 1248,
            })
        );
        let sig_json_stderr = String::from_utf8(sig_json_err).unwrap();
        assert!(
            sig_json_stderr.contains("无法读取排空计数"),
            "信号停机 JSON 模式应在 stderr 说明无法读取排空计数：{sig_json_stderr:?}"
        );

        let mut sig_text_out = Vec::new();
        let mut sig_text_err = Vec::new();
        let sig_text_code =
            render_stop_outcome(&signal_stop, false, &mut sig_text_out, &mut sig_text_err).unwrap();
        assert_eq!(sig_text_code, std::process::ExitCode::SUCCESS);
        let sig_text = String::from_utf8(sig_text_out).unwrap();
        assert!(
            sig_text.contains("对端进程号：1248") && sig_text.contains("状态根已释放"),
            "信号停机人类可读输出不符：{sig_text:?}"
        );
        assert!(
            String::from_utf8(sig_text_err)
                .unwrap()
                .contains("无法读取排空计数")
        );
    }
}
