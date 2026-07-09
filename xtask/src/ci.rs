// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Local CI replica.
//!
//! Compile parallelism is capped via `CARGO_BUILD_JOBS` (default 4). The dev
//! machine has 24 cores and runs a heavy desktop whose Electron apps massively
//! over-reserve virtual address space; cargo's default of one rustc per core
//! plus codegen allocation spikes has triggered global OOM during the compile
//! phase (see FINDINGS.md "OOM: dev-machine compile-phase oversubscription").
//! The cap bounds the compile footprint on oversubscribed hosts without
//! slowing down resource-rich CI, where `CARGO_BUILD_JOBS` can be raised.
//!
//! The test step runs one `cargo test -p <crate>` per workspace member in
//! dependency order, each under a hard timeout. A single runaway (e.g. a
//! network test hanging in a DNS-blackhole environment — see FINDINGS.md
//! L-26-01) is bounded to its crate's budget and fails fast instead of
//! hanging the whole gate indefinitely.

use std::env;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::Context as _;
use cargo_metadata::{Metadata, MetadataCommand, PackageId};
use clap::Args;

use crate::{harness_boundary, repo_root};

#[derive(Args, Debug)]
pub struct CiArgs {}

/// Default cargo build/test parallelism for the local CI gate. Override with
/// `CARGO_BUILD_JOBS`. See the module docs for why this is capped.
const DEFAULT_BUILD_JOBS: &str = "4";

/// Per-crate test timeout in seconds. Override with
/// `LATTICE_TEST_TIMEOUT_SECS`.
const DEFAULT_TEST_TIMEOUT_SECS: u64 = 300;

pub fn run(_args: CiArgs) -> anyhow::Result<()> {
    let root = repo_root::repo_root()?;
    let lattice_bin = root.join("target/debug/lattice");
    let harness_failures = root.join("target/harness-failures");
    let build_jobs = env::var("CARGO_BUILD_JOBS").unwrap_or_else(|_| DEFAULT_BUILD_JOBS.to_owned());

    step("fmt", || {
        run_cargo(&root, &["fmt", "--all", "--check"], &build_jobs)
    })?;

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
            &build_jobs,
        )
    })?;

    step("build lattice (debug)", || {
        run_cargo(
            &root,
            &["build", "-p", "lattice-cli", "--bin", "lattice"],
            &build_jobs,
        )
    })?;

    step("system harness boundary", harness_boundary::run)?;

    step("test", || {
        run_tests(&root, &lattice_bin, &harness_failures, &build_jobs)
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

fn run_cargo(root: &std::path::Path, args: &[&str], build_jobs: &str) -> anyhow::Result<()> {
    let status = Command::new("cargo")
        .current_dir(root)
        .env("CARGO_BUILD_JOBS", build_jobs)
        .args(args)
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()?;
    if !status.success() {
        anyhow::bail!("cargo {} failed", args.join(" "));
    }
    Ok(())
}

fn run_tests(
    root: &Path,
    lattice_bin: &Path,
    harness_failures: &Path,
    build_jobs: &str,
) -> anyhow::Result<()> {
    let metadata = MetadataCommand::new()
        .current_dir(root)
        .exec()
        .map_err(|e| anyhow::anyhow!("failed to read workspace metadata: {e}"))?;
    let crates = ordered_test_crates(&metadata)?;
    let timeout_secs = env::var("LATTICE_TEST_TIMEOUT_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_TEST_TIMEOUT_SECS);
    let skip_known_hangs = env::var_os("LATTICE_SKIP_KNOWN_HANGS").is_some();

    println!();
    println!(
        "\u{1b}[1m── test ({} crates, {build_jobs} build jobs, {timeout_secs}s/crate) ──\u{1b}[0m",
        crates.len()
    );

    let mut failures: Vec<String> = Vec::new();
    for name in &crates {
        let mut libtest: Vec<&str> = Vec::new();
        if name == "lancet-native" && skip_known_hangs {
            println!(
                "\u{1b}[33m  {name}: skipping fetch_effect_contract_negative \
                 (LATTICE_SKIP_KNOWN_HANGS; see FINDINGS.md L-26-01)\u{1b}[0m"
            );
            libtest.push("--skip");
            libtest.push("fetch_effect_contract_negative");
        }
        match run_cargo_test(
            root,
            name,
            &libtest,
            build_jobs,
            lattice_bin,
            harness_failures,
            timeout_secs,
        ) {
            Ok(()) => println!("\u{1b}[0;32m✓\u{1b}[0m test {name}"),
            Err(e) => {
                println!("\u{1b}[0;31m✗\u{1b}[0m test {name}: {e}");
                failures.push(name.clone());
            }
        }
    }

    if !failures.is_empty() {
        anyhow::bail!(
            "cargo test failed for {} crate(s): {}",
            failures.len(),
            failures.join(", ")
        );
    }
    Ok(())
}

fn run_cargo_test(
    root: &Path,
    crate_name: &str,
    extra_libtest_args: &[&str],
    build_jobs: &str,
    lattice_bin: &Path,
    harness_failures: &Path,
    timeout_secs: u64,
) -> anyhow::Result<()> {
    let mut cmd = Command::new("cargo");
    cmd.current_dir(root)
        .env("CARGO_BUILD_JOBS", build_jobs)
        .arg("test")
        .arg("-p")
        .arg(crate_name)
        .arg("--")
        .args(extra_libtest_args)
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        // Put cargo in its own process group so a timeout can kill the whole
        // tree (cargo + rustc + test binaries) instead of orphaning them.
        .process_group(0);
    if env::var_os("LATTICE_BIN").is_none() {
        cmd.env("LATTICE_BIN", lattice_bin);
    }
    if env::var_os("LATTICE_HARNESS_FAILURE_DIR").is_none() {
        cmd.env("LATTICE_HARNESS_FAILURE_DIR", harness_failures);
    }

    let start = Instant::now();
    let deadline = Duration::from_secs(timeout_secs);
    let mut child = cmd
        .spawn()
        .with_context(|| format!("failed to spawn `cargo test -p {crate_name}`"))?;
    // `process_group(0)` makes the child its own pgrp leader, so its pid is the
    // pgrp id used to signal the whole tree on timeout.
    let pgid = child.id();

    loop {
        match child.try_wait()? {
            Some(status) => {
                if status.success() {
                    return Ok(());
                }
                anyhow::bail!("`cargo test -p {crate_name}` failed: {status}");
            }
            None => {
                if start.elapsed() >= deadline {
                    // Kill the whole process group, then the cargo child as a
                    // backstop, and reap it so we don't leave a zombie.
                    let _ = Command::new("kill")
                        .arg("-KILL")
                        .arg(format!("-{pgid}"))
                        .stdout(Stdio::null())
                        .stderr(Stdio::null())
                        .status();
                    let _ = child.kill();
                    let _ = child.wait();
                    anyhow::bail!(
                        "`cargo test -p {crate_name}` timed out after {timeout_secs}s \
                         (set LATTICE_TEST_TIMEOUT_SECS to raise the bound)"
                    );
                }
                std::thread::sleep(Duration::from_millis(200));
            }
        }
    }
}

/// Workspace member crate names in dependency order (dependencies before
/// dependents), excluding `xtask` (which has no tests). Deterministic: ties
/// are broken by name so the gate output is stable across runs.
fn ordered_test_crates(metadata: &Metadata) -> anyhow::Result<Vec<String>> {
    use std::collections::{BTreeMap, HashSet};

    let id_to_name: BTreeMap<&PackageId, &str> = metadata
        .packages
        .iter()
        .map(|p| (&p.id, p.name.as_str()))
        .collect();
    let member_ids: HashSet<&PackageId> = metadata.workspace_members.iter().collect();
    let nodes: std::collections::HashMap<&PackageId, &cargo_metadata::Node> = metadata
        .resolve
        .as_ref()
        .map(|r| r.nodes.iter().map(|n| (&n.id, n)).collect())
        .unwrap_or_default();

    let mut order: Vec<String> = Vec::new();
    let mut visited: HashSet<&PackageId> = HashSet::new();

    // Deterministic start set: all workspace members sorted by name.
    let mut roots: Vec<&PackageId> = member_ids.iter().copied().collect();
    roots.sort_by_key(|id| id_to_name.get(id).copied().unwrap_or(""));

    fn visit<'a>(
        id: &'a PackageId,
        member_ids: &HashSet<&'a PackageId>,
        nodes: &std::collections::HashMap<&'a PackageId, &'a cargo_metadata::Node>,
        id_to_name: &BTreeMap<&'a PackageId, &'a str>,
        visited: &mut HashSet<&'a PackageId>,
        order: &mut Vec<String>,
    ) {
        if !member_ids.contains(id) || !visited.insert(id) {
            return;
        }
        if let Some(node) = nodes.get(id) {
            let mut deps: Vec<&PackageId> = node
                .dependencies
                .iter()
                .filter(|d| member_ids.contains(d))
                .collect();
            deps.sort_by_key(|d| id_to_name.get(d).copied().unwrap_or(""));
            for dep in deps {
                visit(dep, member_ids, nodes, id_to_name, visited, order);
            }
        }
        let name = id_to_name.get(id).copied().unwrap_or("unknown");
        if name != "xtask" {
            order.push(name.to_owned());
        }
    }

    for root in roots {
        visit(
            root,
            &member_ids,
            &nodes,
            &id_to_name,
            &mut visited,
            &mut order,
        );
    }

    Ok(order)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordered_test_crates_is_deterministic_and_excludes_xtask() {
        let root = repo_root::repo_root().unwrap();
        let metadata = MetadataCommand::new()
            .current_dir(&root)
            .exec()
            .expect("workspace metadata");
        let a = ordered_test_crates(&metadata).unwrap();
        let b = ordered_test_crates(&metadata).unwrap();
        assert_eq!(a, b, "order must be deterministic");
        assert!(
            !a.iter().any(|n| n == "xtask"),
            "xtask has no tests and must be excluded: {a:?}"
        );
        // Sanity: the known test-bearing crates are all present.
        for expected in [
            "net-guard",
            "lancet-protocol",
            "lancet-native",
            "lattice-api",
            "lattice-cli",
            "lattice-system-harness",
        ] {
            assert!(
                a.iter().any(|n| n == expected),
                "expected crate {expected} missing from order: {a:?}"
            );
        }
    }

    #[test]
    fn ordered_test_crates_puts_dependencies_before_dependents() {
        let root = repo_root::repo_root().unwrap();
        let metadata = MetadataCommand::new()
            .current_dir(&root)
            .exec()
            .expect("workspace metadata");
        let order = ordered_test_crates(&metadata).unwrap();
        // `lancet-protocol` is a leaf dependency of `lancet-catalog`, which in
        // turn is a dependency of higher layers. It must appear before any of
        // its known dependents.
        let proto = order
            .iter()
            .position(|n| n == "lancet-protocol")
            .expect("lancet-protocol present");
        let catalog = order
            .iter()
            .position(|n| n == "lancet-catalog")
            .expect("lancet-catalog present");
        assert!(
            proto < catalog,
            "lancet-protocol must come before lancet-catalog: {order:?}"
        );
    }
}
