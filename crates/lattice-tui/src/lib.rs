// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Full-screen TUI shell for LATTICE.

mod app;
mod blocks;
mod comment;
mod connection;
mod input;
mod markdown;
mod queue;
mod session;
mod view;
mod wrapping;

use std::path::PathBuf;

#[derive(Clone, Debug)]
pub struct TuiOptions {
    pub session_id: Option<String>,
    pub agent: Option<lattice_local::AgentName>,
    pub session_dir: PathBuf,
    pub project_root: PathBuf,
}

pub async fn run(mut options: TuiOptions) -> anyhow::Result<std::process::ExitCode> {
    let workspace = std::env::current_dir().map_err(|_| {
        lattice_local::LocalError::new(
            lattice_local::LocalErrorKind::Internal,
            "operation failed; inspect durable state",
        )
    })?;
    options.project_root = workspace.clone();
    let client = lattice_local::LocalClient::new(lattice_local::StateLayout::resolve()?);
    client.ensure_running().await?;
    let ctx = session::open_or_create(&options, &workspace, &client).await?;
    app::run_app(options, ctx, client).await?;
    Ok(std::process::ExitCode::SUCCESS)
}
