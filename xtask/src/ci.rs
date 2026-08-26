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

use crate::{harness_boundary, lancet_boundary, repo_root, tool_build, tool_codegen};

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

    step("toolchain precheck", check_toolchain)?;

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

    step("lancet boundary", lancet_boundary::run)?;

    step("tool-codegen --check", tool_codegen::check)?;

    step("test", || {
        run_tests(&root, &lattice_bin, &harness_failures, &build_jobs)
    })?;

    step("tool-build --verify (warn-only)", tool_build::verify)?;

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

/// Assert the Source Tool build prerequisites (#5 slice B decision 1): the
/// pinned rustc from `rust-toolchain.toml`, the `wasm32-unknown-unknown`
/// target std, and the `wasm-tools` encoder. `tool-codegen`/`tool-build` and
/// the guest crate both need the wasm target installed; a missing target would
/// otherwise surface as an opaque linker error deep in `tool-build`.
fn check_toolchain() -> anyhow::Result<()> {
    let rustc_out = Command::new("rustc")
        .arg("--version")
        .stderr(Stdio::inherit())
        .output()
        .context("failed to run `rustc --version`")?;
    if !rustc_out.status.success() {
        anyhow::bail!("`rustc --version` failed");
    }
    let version = String::from_utf8_lossy(&rustc_out.stdout);
    // The pinned channel is 1.96.0 (rust-toolchain.toml). We assert the major
    // minor so a host on an older stable fails the gate early with a clear
    // message instead of a later wasmtime/wit-bindgen MSRV error.
    if !version.contains("1.96.0") {
        anyhow::bail!("pinned rustc 1.96.0 required; got: {version}");
    }

    let targets = Command::new("rustup")
        .args(["target", "list", "--installed"])
        .stderr(Stdio::inherit())
        .output()
        .context("failed to run `rustup target list --installed`")?;
    if !targets.status.success() {
        anyhow::bail!("`rustup target list --installed` failed");
    }
    let installed = String::from_utf8_lossy(&targets.stdout);
    if !installed
        .lines()
        .any(|l| l.trim() == "wasm32-unknown-unknown")
    {
        anyhow::bail!(
            "wasm32-unknown-unknown target not installed; run `rustup target add wasm32-unknown-unknown`"
        );
    }

    // `wasm-tools` is an implicit external build dependency: `tool-build`
    // shells out to it to encode the core module into a Component and to
    // validate the import surface. It is not a Cargo dependency, so nothing
    // installs it for us, and the encoder determines the committed
    // Component's bytes. Absence is fatal here (the gate's last step would
    // fail anyway, but ~9 steps later); a version mismatch is a warning
    // because reproducibility is still warn-only per #5 decision 1 / #73.
    match probe_wasm_tools() {
        WasmToolsProbe::Version(version) => {
            if version != EXPECTED_WASM_TOOLS {
                println!(
                    "warn: wasm-tools {version} installed, expected {EXPECTED_WASM_TOOLS}; \
                     Component digests may drift (see #73)"
                );
            }
        }
        WasmToolsProbe::NotOnPath => anyhow::bail!(
            "wasm-tools not found on PATH; `cargo xtask tool-build` needs it to encode and \
             validate Source Tool Components. Install {EXPECTED_WASM_TOOLS} (for example \
             `cargo install wasm-tools --version {EXPECTED_WASM_TOOLS} --locked`)."
        ),
        // Present but unusable: reinstalling the same version is the wrong
        // advice, so report what actually happened instead.
        WasmToolsProbe::Failed(why) => anyhow::bail!(
            "wasm-tools is on PATH but {why}; `cargo xtask tool-build` needs a working \
             {EXPECTED_WASM_TOOLS} encoder. Repair the installation, then re-run."
        ),
    }

    Ok(())
}

/// The `wasm-tools` release the committed Component artifacts were encoded
/// with. Three places must agree, each asserted by a test below: this constant,
/// the pinned installer in `.github/workflows/full-gate.yml`, and the
/// `[artifact] toolchain` fingerprint in `tools/*/manifest.toml`.
pub(crate) const EXPECTED_WASM_TOOLS: &str = "1.254.0";

/// Why `wasm-tools --version` could not be answered. Distinguished so the
/// caller can tell a user to *install* the encoder versus *repair* it —
/// collapsing all three into "not found on PATH" sends someone whose binary is
/// present but broken off to do a reinstall that changes nothing.
pub(crate) enum WasmToolsProbe {
    Version(String),
    NotOnPath,
    Failed(String),
}

/// The installed `wasm-tools` version, or `None` when the binary is missing,
/// exits non-zero, or prints output this can't parse. Prefer
/// [`probe_wasm_tools`] when the distinction matters.
pub(crate) fn wasm_tools_version() -> Option<String> {
    match probe_wasm_tools() {
        WasmToolsProbe::Version(v) => Some(v),
        _ => None,
    }
}

pub(crate) fn probe_wasm_tools() -> WasmToolsProbe {
    let out = match Command::new("wasm-tools")
        .arg("--version")
        .stderr(Stdio::null())
        .output()
    {
        Ok(out) => out,
        // Only ENOENT means "not installed"; anything else (permissions, exec
        // format) is a broken installation.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return WasmToolsProbe::NotOnPath,
        Err(e) => return WasmToolsProbe::Failed(format!("could not be executed: {e}")),
    };
    if !out.status.success() {
        return WasmToolsProbe::Failed(format!(
            "`wasm-tools --version` exited with {}",
            out.status
        ));
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    match parse_wasm_tools_version(&stdout) {
        Some(v) => WasmToolsProbe::Version(v),
        None => WasmToolsProbe::Failed(format!(
            "`wasm-tools --version` printed unparseable output: {:?}",
            stdout.trim()
        )),
    }
}

/// `wasm-tools --version` prints `wasm-tools <semver>`. Split out from the
/// process call so the parse is unit-testable without the binary installed.
fn parse_wasm_tools_version(stdout: &str) -> Option<String> {
    stdout.split_whitespace().nth(1).map(str::to_owned)
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
    fn parses_the_version_out_of_wasm_tools_version_output() {
        assert_eq!(
            parse_wasm_tools_version("wasm-tools 1.254.0\n").as_deref(),
            Some("1.254.0")
        );
    }

    #[test]
    fn wasm_tools_version_parse_rejects_output_without_a_version_field() {
        assert_eq!(parse_wasm_tools_version("wasm-tools\n"), None);
        assert_eq!(parse_wasm_tools_version(""), None);
    }

    /// The pinned version must stay in step with the installer in
    /// `.github/workflows/full-gate.yml`; a silent divergence would make the
    /// local gate warn on exactly the version CI uses (see #80).
    #[test]
    fn expected_wasm_tools_matches_the_full_gate_installer_pin() {
        let root = repo_root::repo_root().unwrap();
        let workflow =
            std::fs::read_to_string(root.join(".github/workflows/full-gate.yml")).unwrap();
        assert!(
            workflow.contains(&format!("wasm-tools@{EXPECTED_WASM_TOOLS}")),
            "full-gate.yml must install wasm-tools@{EXPECTED_WASM_TOOLS}"
        );
    }

    /// The encoder determines the committed Component's bytes, so every
    /// manifest's `[artifact] toolchain` fingerprint must name the encoder its
    /// pinned `sha256` was produced with. Without this the fingerprint claims
    /// to identify the build while omitting a factor that changes the digest
    /// (see #80), and `EXPECTED_WASM_TOOLS` could be bumped without anyone
    /// re-pinning the artifacts.
    #[test]
    fn every_tool_manifest_fingerprint_names_the_expected_wasm_tools() {
        let root = repo_root::repo_root().unwrap();
        let manifests =
            crate::tool_codegen::discover_tool_manifests(&root).expect("discover manifests");
        assert!(
            !manifests.is_empty(),
            "expected at least one tools/*/manifest.toml"
        );
        let needle = format!("wasm-tools-{EXPECTED_WASM_TOOLS}");
        for manifest in manifests {
            let raw = std::fs::read_to_string(&manifest).unwrap();
            let toolchain = raw
                .lines()
                .find_map(|l| l.trim().strip_prefix("toolchain = "))
                .unwrap_or_else(|| panic!("{} has no [artifact] toolchain", manifest.display()))
                .trim_matches('"');
            assert!(
                toolchain.contains(&needle),
                "{}: [artifact] toolchain is {toolchain:?}, which does not name {needle}; \
                 re-run `cargo xtask tool-build` after changing the encoder pin",
                manifest.display()
            );
        }
    }

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
