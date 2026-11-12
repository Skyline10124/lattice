// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use clap::Subcommand;
use lattice_api::{
    StateLayout,
    local::{self, LocalClient},
};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Subcommand)]
pub enum DaemonAction {
    /// Run the daemon in the foreground, inheriting the launching process's environment, just like automatic startup.
    Serve {
        /// XDG state root; must be an absolute path. Defaults to the submitter's HOME/XDG resolution.
        #[arg(long)]
        state_root: Option<PathBuf>,
        /// Maximum time to wait for active operations to drain during shutdown (ms, s, m; bare integer means seconds).
        #[arg(
            long,
            default_value = "60s",
            allow_hyphen_values = true,
            value_parser = parse_drain_timeout
        )]
        drain_timeout: Duration,
    },
    /// Query the running daemon without starting one.
    Status,
    /// Stop accepting new work, wait for taken-over work and durable closure to finish,
    /// then wait for the daemon to remove its socket and release the instance lock.
    Stop,
}

fn parse_drain_timeout(raw: &str) -> Result<Duration, String> {
    if raw.is_empty() {
        return Err("排空时长不能为空".into());
    }
    enum Unit {
        Millis,
        Secs,
        Mins,
    }
    let (digits, unit) = if let Some(digits) = raw.strip_suffix("ms") {
        (digits, Unit::Millis)
    } else if let Some(digits) = raw.strip_suffix('s') {
        (digits, Unit::Secs)
    } else if let Some(digits) = raw.strip_suffix('m') {
        (digits, Unit::Mins)
    } else {
        (raw, Unit::Secs)
    };
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err("排空时长必须是非负整数，单位只支持 ms、s 或 m".into());
    }
    let value = digits
        .parse::<u64>()
        .map_err(|_| "排空时长数值溢出".to_string())?;
    match unit {
        Unit::Millis => Ok(Duration::from_millis(value)),
        Unit::Secs => Ok(Duration::from_secs(value)),
        Unit::Mins => value
            .checked_mul(60)
            .map(Duration::from_secs)
            .ok_or_else(|| "排空时长数值溢出".to_string()),
    }
}

fn state_root(explicit: Option<&Path>) -> anyhow::Result<PathBuf> {
    if let Some(root) = explicit {
        if !root.is_absolute() {
            return Err(
                lattice_api::ApiError::BadRequest("state root must be absolute".into()).into(),
            );
        }
        return Ok(root.to_owned());
    }
    Ok(StateLayout::resolve()?
        .lattice_root()
        .parent()
        .ok_or_else(|| anyhow::anyhow!("missing state root"))?
        .to_owned())
}

pub async fn serve(
    explicit_state_root: Option<&Path>,
    drain_timeout: Duration,
    stop_requests: tokio::sync::mpsc::Receiver<()>,
) -> anyhow::Result<local::ServeExit> {
    let root = state_root(explicit_state_root)?;
    local::serve(
        StateLayout::in_state_root(&root),
        std::env::var_os("HOME").map(PathBuf::from),
        drain_timeout,
        stop_requests,
    )
    .await
    .map_err(Into::into)
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
    status: &local::DaemonStatus,
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
    use lattice_api::local::DaemonStatus;

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
