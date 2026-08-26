// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use std::process::{Command, Stdio};

use anyhow::{Context, bail};
use clap::Args;

use crate::repo_root;

const TUI_MOCK_PROFILE: &str = "tui-mock";

#[derive(Args, Debug)]
pub struct TuiMockArgs {
    /// Scenario: long-text, tool-call, disconnect, slow, or error.
    #[arg(long, default_value = "long-text")]
    scenario: String,
}

fn build_args() -> [&'static str; 7] {
    [
        "build",
        "--profile",
        TUI_MOCK_PROFILE,
        "-p",
        "lattice-cli",
        "--bin",
        "lattice",
    ]
}

fn lattice_binary(target_dir: &std::path::Path) -> std::path::PathBuf {
    target_dir.join(TUI_MOCK_PROFILE).join("lattice")
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
    let lattice_bin = lattice_binary(&target_dir);

    let build = Command::new("cargo")
        .current_dir(&root)
        .args(build_args())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .context("build lattice binary")?;
    if !build.success() {
        bail!("cargo build --profile {TUI_MOCK_PROFILE} -p lattice-cli --bin lattice failed");
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mock_build_uses_the_optimized_profile() {
        assert_eq!(
            build_args(),
            [
                "build",
                "--profile",
                "tui-mock",
                "-p",
                "lattice-cli",
                "--bin",
                "lattice",
            ]
        );
        assert_eq!(
            lattice_binary(std::path::Path::new("/target")),
            std::path::Path::new("/target/tui-mock/lattice")
        );
    }
}
