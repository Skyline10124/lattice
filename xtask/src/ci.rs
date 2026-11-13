// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 本地 CI 的复刻。
//!
//! 编译并发由 `CARGO_BUILD_JOBS` 限制，默认 4。一台 24 核开发机上常驻的 Electron
//! 应用预留了极大的虚拟地址空间，cargo 默认每核一个 rustc，加上 codegen 的瞬时分配，
//! 曾在编译阶段触发全局 OOM。限制并发后，编译占用在资源紧张的主机上有上限；资源充足的
//! CI 可以调高 `CARGO_BUILD_JOBS`。原始记录见
//! `git show ead22771^:docs/archive/records/FINDINGS.md` 中的 ENV-OOM-01。
//!
//! 测试步骤按依赖顺序，对每个 workspace 成员单独运行 `cargo test -p <crate>`，每次都有
//! 超时上限。某个 crate 失控时（例如网络测试在 DNS 黑洞环境里挂起），只耗尽它自己的时间
//! 预算并快速失败，整个门禁随之结束。原始记录见
//! `git show ead22771^:docs/archive/records/FIX.md` 中的 L-26-01。
//!
//! 门禁按两轮验证被测二进制：第一轮构建无测试 feature 的生产二进制并跑全部默认测试；
//! 第二轮先检查测试 feature 下的 clippy，再用 `--features lattice-server/test-fake-tools,lattice-server/test-crash-barriers`
//! 重新构建并覆盖同一个 `target/debug/lattice`（`LATTICE_BIN` 路径不变），随后运行假执行器与
//! 全部 harness 目标（含假工具场景与崩溃屏障）。`xtask ci` 结束后，`target/debug/lattice`
//! 留存的是第二轮带测试 feature 的构建产物。

use std::env;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::Context as _;
use cargo_metadata::{Metadata, MetadataCommand, PackageId};
use clap::Args;

use crate::{crate_boundary, docs_check, harness_boundary, repo_root};

#[derive(Args, Debug)]
pub struct CiArgs {}

/// 本地 CI 门禁中 cargo 构建与测试的默认并发数，可用 `CARGO_BUILD_JOBS` 覆盖；设上限的原因见模块文档。
const DEFAULT_BUILD_JOBS: &str = "4";

/// 每个 crate 的测试超时秒数，可用 `LATTICE_TEST_TIMEOUT_SECS` 覆盖。
const DEFAULT_TEST_TIMEOUT_SECS: u64 = 300;

pub fn run(_args: CiArgs) -> anyhow::Result<()> {
    let root = repo_root::repo_root()?;
    let lattice_bin = root.join("target/debug/lattice");
    let harness_failures = root.join("target/harness-failures");
    let build_jobs = env::var("CARGO_BUILD_JOBS").unwrap_or_else(|_| DEFAULT_BUILD_JOBS.to_owned());

    step("工具链预检", check_toolchain)?;

    step("文档链接检查", docs_check::run)?;

    step("格式检查", || {
        run_cargo(&root, &["fmt", "--all", "--check"], &build_jobs)
    })?;

    step("clippy 检查", || {
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

    step("构建 lattice（debug）", || {
        run_cargo(
            &root,
            &["build", "-p", "lattice-cli", "--bin", "lattice"],
            &build_jobs,
        )
    })?;

    step("harness 边界检查", harness_boundary::run)?;

    step("crate 依赖边界检查", crate_boundary::run)?;

    step("测试", || {
        run_tests(&root, &lattice_bin, &harness_failures, &build_jobs)
    })?;

    // 第二轮：检查测试专用 feature 的编译与 lint，再用带测试 feature 的产物覆盖同一个
    // target/debug/lattice（LATTICE_BIN 不变），运行假执行器单元测试、假工具场景与崩溃屏障。
    step(
        "clippy 检查（带测试 feature：产品 crate）",
        || {
            run_cargo(
                &root,
                &[
                    "clippy",
                    "-p",
                    "lattice-server",
                    "-p",
                    "lattice-cli",
                    "--all-targets",
                    "--features",
                    "lattice-server/test-fake-tools,lattice-server/test-crash-barriers",
                    "--",
                    "-D",
                    "warnings",
                ],
                &build_jobs,
            )
        },
    )?;

    step("clippy 检查（带测试 feature：harness）", || {
        run_cargo(
            &root,
            &[
                "clippy",
                "-p",
                "lattice-system-harness",
                "--all-targets",
                "--features",
                "fake-tools,crash-barriers",
                "--",
                "-D",
                "warnings",
            ],
            &build_jobs,
        )
    })?;

    step("构建 lattice（debug，带测试 feature）", || {
        run_cargo(
            &root,
            &[
                "build",
                "-p",
                "lattice-cli",
                "--bin",
                "lattice",
                "--features",
                "lattice-server/test-fake-tools,lattice-server/test-crash-barriers",
            ],
            &build_jobs,
        )
    })?;

    step("测试（带测试 feature）", || {
        run_feature_tests(&root, &lattice_bin, &harness_failures, &build_jobs)
    })?;

    println!();
    println!("\u{1b}[1;32m全部检查通过\u{1b}[0m");
    Ok(())
}

fn step(name: &str, run: impl FnOnce() -> anyhow::Result<()>) -> anyhow::Result<()> {
    println!();
    println!("\u{1b}[1m── {name} ──\u{1b}[0m");
    run()?;
    println!("\u{1b}[0;32m✓\u{1b}[0m {name}");
    Ok(())
}

/// 核验 `rust-toolchain.toml` 固定的 `rustc` 版本，让工具链版本不符的主机在门禁开头快速失败。
fn check_toolchain() -> anyhow::Result<()> {
    let rustc_out = Command::new("rustc")
        .arg("--version")
        .stderr(Stdio::inherit())
        .output()
        .context("无法运行 `rustc --version`")?;
    if !rustc_out.status.success() {
        anyhow::bail!("`rustc --version` 执行失败");
    }
    let version = String::from_utf8_lossy(&rustc_out.stdout);
    if !version.contains("1.96.0") {
        anyhow::bail!("需要固定版本 rustc 1.96.0，实际为：{}", version.trim());
    }

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
        anyhow::bail!("`cargo {}` 失败", args.join(" "));
    }
    Ok(())
}

fn test_timeout_secs() -> u64 {
    env::var("LATTICE_TEST_TIMEOUT_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_TEST_TIMEOUT_SECS)
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
        .map_err(|e| anyhow::anyhow!("无法读取 workspace 元数据：{e}"))?;
    let crates = ordered_test_crates(&metadata)?;
    let timeout_secs = test_timeout_secs();

    println!();
    println!(
        "\u{1b}[1m── 测试（{} 个 crate，构建并发 {build_jobs}，每个 crate 限时 {timeout_secs} 秒） ──\u{1b}[0m",
        crates.len()
    );

    let mut failures: Vec<String> = Vec::new();
    for name in &crates {
        match run_cargo_test(
            root,
            name,
            None,
            build_jobs,
            lattice_bin,
            harness_failures,
            timeout_secs,
        ) {
            Ok(()) => println!("\u{1b}[0;32m✓\u{1b}[0m 测试 {name}"),
            Err(e) => {
                println!("\u{1b}[0;31m✗\u{1b}[0m 测试 {name}：{e}");
                failures.push(name.clone());
            }
        }
    }

    if !failures.is_empty() {
        anyhow::bail!(
            "cargo test 在 {} 个 crate 上失败：{}",
            failures.len(),
            failures.join(", ")
        );
    }
    Ok(())
}

fn run_feature_tests(
    root: &Path,
    lattice_bin: &Path,
    harness_failures: &Path,
    build_jobs: &str,
) -> anyhow::Result<()> {
    let targets = [
        ("lattice-server", "test-fake-tools,test-crash-barriers"),
        ("lattice-system-harness", "fake-tools,crash-barriers"),
    ];
    let timeout_secs = test_timeout_secs();

    println!();
    println!(
        "\u{1b}[1m── 测试（带测试 feature：{} 个 crate，构建并发 {build_jobs}，每个 crate 限时 {timeout_secs} 秒） ──\u{1b}[0m",
        targets.len()
    );

    let mut failures: Vec<String> = Vec::new();
    for (name, features) in targets {
        let label = format!("{name}（{features}）");
        match run_cargo_test(
            root,
            name,
            Some(features),
            build_jobs,
            lattice_bin,
            harness_failures,
            timeout_secs,
        ) {
            Ok(()) => println!("\u{1b}[0;32m✓\u{1b}[0m 测试 {label}"),
            Err(e) => {
                println!("\u{1b}[0;31m✗\u{1b}[0m 测试 {label}：{e}");
                failures.push(label);
            }
        }
    }

    if !failures.is_empty() {
        anyhow::bail!(
            "带测试 feature 的 cargo test 在 {} 个目标上失败：{}",
            failures.len(),
            failures.join(", ")
        );
    }
    Ok(())
}

fn run_cargo_test(
    root: &Path,
    crate_name: &str,
    features: Option<&str>,
    build_jobs: &str,
    lattice_bin: &Path,
    harness_failures: &Path,
    timeout_secs: u64,
) -> anyhow::Result<()> {
    let cmd_desc = match features {
        Some(f) => format!("cargo test -p {crate_name} --features {f}"),
        None => format!("cargo test -p {crate_name}"),
    };
    let mut cmd = Command::new("cargo");
    cmd.current_dir(root)
        .env("CARGO_BUILD_JOBS", build_jobs)
        .arg("test")
        .arg("-p")
        .arg(crate_name);
    if let Some(features) = features {
        cmd.arg("--features").arg(features);
    }
    cmd.stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        // 让 cargo 自成进程组，超时时整棵进程树（cargo、rustc 与测试二进制）一起终止，不留下孤儿进程。
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
        .with_context(|| format!("启动 `{cmd_desc}` 失败"))?;
    // `process_group(0)` 让子进程成为进程组组长，它的 pid 就是超时时向整棵进程树发信号所用的进程组 id。
    let pgid = child.id();

    loop {
        match child.try_wait()? {
            Some(status) => {
                if status.success() {
                    return Ok(());
                }
                anyhow::bail!("`{cmd_desc}` 失败：{status}");
            }
            None => {
                if start.elapsed() >= deadline {
                    // 先终止整个进程组，再单独终止 cargo 子进程兜底，并回收它，避免留下僵尸进程。
                    let _ = Command::new("kill")
                        .arg("-KILL")
                        .arg(format!("-{pgid}"))
                        .stdout(Stdio::null())
                        .stderr(Stdio::null())
                        .status();
                    let _ = child.kill();
                    let _ = child.wait();
                    anyhow::bail!(
                        "`{cmd_desc}` 在 {timeout_secs}s 后超时（可设置 LATTICE_TEST_TIMEOUT_SECS 提高上限）"
                    );
                }
                std::thread::sleep(Duration::from_millis(200));
            }
        }
    }
}

/// 按依赖顺序（被依赖的在前）列出 workspace 成员 crate 名，不含 `xtask`。顺序是确定的：
/// 同层按名称排序，门禁输出在多次运行之间保持一致。
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

    // 起点集合固定：全部 workspace 成员按名称排序。
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
