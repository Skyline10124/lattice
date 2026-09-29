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
    pub agent: String,
    pub session_dir: PathBuf,
    pub agents_dir: PathBuf,
    pub project_root: PathBuf,
}

pub async fn run(options: TuiOptions) -> anyhow::Result<std::process::ExitCode> {
    let client = lattice_api::local::LocalClient::new(lattice_api::StateLayout::resolve()?);
    client.ensure_running().await?;
    let ctx = session::open_or_create(&options, &client).await?;
    app::run_app(options, ctx, client).await?;
    Ok(std::process::ExitCode::SUCCESS)
}
