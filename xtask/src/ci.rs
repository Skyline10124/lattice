// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use std::env;
use std::process::{Command, Stdio};

use clap::Args;

use crate::{harness_boundary, repo_root};

#[derive(Args, Debug)]
pub struct CiArgs {}

pub fn run(_args: CiArgs) -> anyhow::Result<()> {
    let root = repo_root::repo_root()?;
    let lattice_bin = root.join("target/debug/lattice");
    let harness_failures = root.join("target/harness-failures");

    step("fmt", || run_cargo(&root, &["fmt", "--all", "--check"]))?;

    step("clippy", || {
        run_cargo(
            &root,
            &[
                "clippy",
                "--workspace",
                "--all-targets",
                "--",
                "-D",
                "warnings",
            ],
        )
    })?;

    step("build lattice (debug)", || {
        run_cargo(&root, &["build", "-p", "lattice-cli", "--bin", "lattice"])
    })?;

    step("system harness boundary", harness_boundary::run)?;

    step("test", || {
        let mut cmd = Command::new("cargo");
        cmd.current_dir(&root);
        cmd.arg("test").arg("--workspace");
        if env::var_os("LATTICE_BIN").is_none() {
            cmd.env("LATTICE_BIN", &lattice_bin);
        }
        if env::var_os("LATTICE_HARNESS_FAILURE_DIR").is_none() {
            cmd.env("LATTICE_HARNESS_FAILURE_DIR", &harness_failures);
        }
        cmd.stdout(Stdio::inherit()).stderr(Stdio::inherit());
        let status = cmd.status()?;
        if !status.success() {
            anyhow::bail!("cargo test --workspace failed");
        }
        Ok(())
    })?;

    println!();
    println!("\u{1b}[1;32mAll checks passed.\u{1b}[0m");
    Ok(())
}

fn step(name: &str, run: impl FnOnce() -> anyhow::Result<()>) -> anyhow::Result<()> {
    println!();
    println!("\u{1b}[1m── {name} ──\u{1b}[0m");
    run()?;
    println!("\u{1b}[0;32m✓\u{1b}[0m {name}");
    Ok(())
}

fn run_cargo(root: &std::path::Path, args: &[&str]) -> anyhow::Result<()> {
    let status = Command::new("cargo")
        .current_dir(root)
        .args(args)
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()?;
    if !status.success() {
        anyhow::bail!("cargo {} failed", args.join(" "));
    }
    Ok(())
}
