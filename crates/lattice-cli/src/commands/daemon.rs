// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use clap::Subcommand;
use lattice_api::{
    StateLayout,
    local::{self, LocalClient},
};
use std::path::{Path, PathBuf};

#[derive(Subcommand)]
pub enum DaemonAction {
    /// 前台运行后台主体；不加载工作区 dotenv 或继承提交端凭据。
    Serve {
        /// XDG 状态根，必须是绝对路径；默认按提交端 HOME/XDG 解析。
        #[arg(long)]
        state_root: Option<PathBuf>,
    },
    /// 查询已有后台，不自动启动。
    Status,
    /// 停止接收新任务，等待已接管任务与持久收尾完成。
    Stop,
}

pub fn state_root(explicit: Option<&Path>) -> anyhow::Result<PathBuf> {
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

pub async fn run(action: DaemonAction, json: bool) -> anyhow::Result<std::process::ExitCode> {
    let status = match action {
        DaemonAction::Serve { state_root: root } => {
            local::serve(StateLayout::in_state_root(&state_root(root.as_deref())?)).await?;
            return Ok(std::process::ExitCode::SUCCESS);
        }
        DaemonAction::Status => LocalClient::new(StateLayout::resolve()?).status().await?,
        DaemonAction::Stop => LocalClient::new(StateLayout::resolve()?).stop().await?,
    };
    if json {
        println!("{}", serde_json::to_string(&status)?);
    } else {
        println!(
            "epoch={} stopping={} owners={} unconfirmed_operations={}",
            status.epoch, status.stopping, status.owners, status.unconfirmed_operations
        );
    }
    Ok(std::process::ExitCode::from(u8::from(
        status.unconfirmed_operations != 0,
    )))
}
