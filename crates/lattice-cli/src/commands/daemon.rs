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
    /// Run the daemon in the foreground, inheriting the launching process's environment, just like automatic startup.
    Serve {
        /// XDG state root; must be an absolute path. Defaults to the submitter's HOME/XDG resolution.
        #[arg(long)]
        state_root: Option<PathBuf>,
    },
    /// Query the running daemon without starting one.
    Status,
    /// Stop accepting new work and wait for taken-over work and durable closure to finish.
    Stop,
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
