// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use std::process::{Command, Stdio};

use anyhow::{Context, bail};
use clap::Args;

use crate::repo_root;

#[derive(Args, Debug)]
pub struct TuiMockArgs {
    /// Scenario: long-text, tool-call, disconnect, slow, or error.
    #[arg(long, default_value = "long-text")]
    scenario: String,
}

pub fn run(args: TuiMockArgs) -> anyhow::Result<()> {
    let root = repo_root::repo_root()?;
    let target_dir = cargo_metadata::MetadataCommand::new()
        .current_dir(&root)
        .no_deps()
        .exec()
        .context("resolve Cargo target directory")?
        .target_directory
        .into_std_path_buf();
    let lattice_bin = target_dir.join("debug/lattice");

    let build = Command::new("cargo")
        .current_dir(&root)
        .args(["build", "-p", "lattice-cli", "--bin", "lattice"])
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .context("build lattice binary")?;
    if !build.success() {
        bail!("cargo build -p lattice-cli --bin lattice failed");
    }

    let status = Command::new("cargo")
        .current_dir(&root)
        .env("LATTICE_BIN", lattice_bin)
        .args([
            "run",
            "-p",
            "lattice-system-harness",
            "--bin",
            "tui_mock",
            "--",
            "--scenario",
            &args.scenario,
        ])
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .context("run TUI mock scenario")?;
    if !status.success() {
        bail!("TUI mock exited with {status}");
    }
    Ok(())
}
