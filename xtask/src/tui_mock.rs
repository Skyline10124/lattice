// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use std::process::{Command, Stdio};

use anyhow::{Context, bail};
use clap::Args;

use crate::repo_root;

#[derive(Args, Debug)]
pub struct TuiMockArgs {
    /// 场景：long-text（默认）、tool-call、disconnect、slow 或 error。
    #[arg(
        long,
        default_value = "long-text",
        value_name = "场景",
        hide_default_value = true
    )]
    scenario: String,
}

fn cli_build_args() -> [&'static str; 5] {
    ["build", "-p", "lattice-cli", "--bin", "lattice"]
}

/// `tool-call` 场景经 `LATTICE_TEST_TOOLS` 启用假工具，后台二进制须开启 `lattice-server/test-fake-tools`。
fn daemon_build_args() -> [&'static str; 7] {
    [
        "build",
        "-p",
        "lattice-daemon",
        "--bin",
        "lattice-daemon",
        "--features",
        "lattice-server/test-fake-tools",
    ]
}

fn lattice_binary(target_dir: &std::path::Path) -> std::path::PathBuf {
    target_dir.join("debug").join("lattice")
}

fn lattice_daemon_binary(target_dir: &std::path::Path) -> std::path::PathBuf {
    target_dir.join("debug").join("lattice-daemon")
}

pub fn run(args: TuiMockArgs) -> anyhow::Result<()> {
    let root = repo_root::repo_root()?;
    let target_dir = cargo_metadata::MetadataCommand::new()
        .current_dir(&root)
        .no_deps()
        .exec()
        .context("解析 Cargo target 目录")?
        .target_directory
        .into_std_path_buf();
    let lattice_bin = lattice_binary(&target_dir);
    let lattice_daemon_bin = lattice_daemon_binary(&target_dir);

    for (desc, build_args) in [
        ("构建 lattice 二进制", cli_build_args().as_slice()),
        (
            "构建带假工具 feature 的 lattice-daemon 二进制",
            daemon_build_args().as_slice(),
        ),
    ] {
        let build = Command::new("cargo")
            .current_dir(&root)
            .args(build_args)
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status()
            .context(desc)?;
        if !build.success() {
            bail!("`cargo {}` 失败", build_args.join(" "));
        }
    }

    let status = Command::new("cargo")
        .current_dir(&root)
        .env("LATTICE_BIN", lattice_bin)
        .env("LATTICE_DAEMON_BIN", lattice_daemon_bin)
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
        .context("运行 TUI 模拟场景")?;
    if !status.success() {
        bail!("TUI 模拟场景异常退出：{status}");
    }
    Ok(())
}
