// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use clap::Subcommand;
use lattice_local::{DaemonStatus, LocalClient, StateLayout};
use std::io::Write;

#[derive(Subcommand)]
pub enum DaemonAction {
    /// Query the running daemon without starting one.
    Status,
    /// Stop accepting new work, wait for taken-over work and durable closure to finish,
    /// then wait for the daemon to remove its socket and release the instance lock.
    Stop,
}

pub async fn run_client(is_stop: bool, json: bool) -> anyhow::Result<std::process::ExitCode> {
    let client = LocalClient::new(StateLayout::resolve()?);
    let status = if is_stop {
        client.stop().await?
    } else {
        client.status().await?
    };
    render_daemon_status(
        &status,
        is_stop,
        json,
        &mut std::io::stdout(),
        &mut std::io::stderr(),
    )
}

fn render_daemon_status(
    status: &DaemonStatus,
    is_stop: bool,
    json: bool,
    out: &mut impl Write,
    err: &mut impl Write,
) -> anyhow::Result<std::process::ExitCode> {
    if json {
        writeln!(out, "{}", serde_json::to_string(status)?)?;
    } else {
        writeln!(
            out,
            "epoch={} stopping={} owners={} unconfirmed_operations={}",
            status.epoch, status.stopping, status.owners, status.unconfirmed_operations
        )?;
    }
    if is_stop && status.unconfirmed_operations != 0 {
        writeln!(
            err,
            "有 {} 个操作没有确认收尾，其中的在途 turn 在后台重启后显示为执行者消失。",
            status.unconfirmed_operations
        )?;
    }
    Ok(std::process::ExitCode::from(u8::from(
        status.unconfirmed_operations != 0,
    )))
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
        assert!(
            status_err.is_empty(),
            "daemon status 不应输出标准错误提示，实际：{:?}",
            String::from_utf8(status_err)
        );
    }
}
