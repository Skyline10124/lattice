// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Full-screen TUI shell for LATTICE.

mod app;
mod input;
mod session;
mod view;

use std::path::PathBuf;

#[derive(Clone, Debug)]
pub struct TuiOptions {
    pub session_id: Option<String>,
    pub agent: String,
    pub project_branch: Option<String>,
    pub session_dir: PathBuf,
    pub agents_dir: PathBuf,
    pub project_root: PathBuf,
}

pub async fn run(options: TuiOptions) -> anyhow::Result<std::process::ExitCode> {
    // #171: eagerly compose the Component runtime at startup so the first
    // prompt does not pay the Wasmtime compilation cost. Composition failures
    // are latched and surface on the first Turn.
    tokio::task::spawn_blocking(lattice_api::warmup_component_runtime)
        .await
        .map_err(|err| anyhow::anyhow!("startup composition worker failed: {err}"))?
        .map_err(|err| anyhow::anyhow!("startup composition failed: {err}"))?;
    let ctx = session::open_or_create(&options)?;
    app::run_app(options, ctx).await?;
    Ok(std::process::ExitCode::SUCCESS)
}
