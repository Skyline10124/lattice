// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! alpha 发布门禁（#47）。
//!
//! 对锁定依赖构建的 release 二进制运行发布取证流程，并写出带版本号的 JSON 记录：
//!
//!   1. 已跟踪工作树干净检查（已跟踪文件有改动时明确拒绝）
//!   2. 能力探测（核验固定的 rustc 版本，另记录仅供参考的主机信息）
//!   3. `cargo build --locked --release -p lattice-cli --bin lattice`
//!   4. 产物身份（sha256 与大小）
//!   5. `cargo xtask harness-boundary`（依赖与架构边界检查）
//!   6. 以 release 二进制（`LATTICE_BIN`）运行 Direct-Local harness 场景
//!   7. 离线正确性测试组（带零通过保护，空跑不算通过）
//!
//! 跳过不等于通过：每个计划步骤先以 `blocked` 写入记录，只有实际运行后才会覆盖；
//! 全部必需步骤为 `passed` 时结论才是 `pass`（见 [`verdict`]）。
//!
//! 不依赖 KVM、Microsandbox、OCI 镜像，也不依赖任何物理隔离。

use std::env;
use std::fs;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::Context as _;
use clap::{Args, ValueEnum};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::repo_root;

pub const SCHEMA_VERSION: u32 = 1;

/// 失败步骤的捕获输出在记录中最多保留的字节数。
const TAIL_MAX_BYTES: usize = 800;

/// 边界检查只扫描 workspace 并读取 cargo metadata，上限较短。
const CMD_TIMEOUT: Duration = Duration::from_secs(600);
/// 冷缓存下锁定依赖的 release 构建是门禁中最耗时的一步。
const BUILD_TIMEOUT: Duration = Duration::from_secs(3600);
/// Direct-Local 场景各有 30 秒或 180 秒的预算；这里是整组测试的兜底上限，挂起的测试二进制拖不住整个门禁。
const TEST_TIMEOUT: Duration = Duration::from_secs(1800);

#[derive(Args, Debug)]
pub struct RolloutArgs {
    /// 发布通道，目前只有 `alpha`。
    #[arg(long, value_enum, value_name = "通道", hide_possible_values = true)]
    pub channel: Channel,
    /// 主记录的输出路径，默认为 `target/rollout/rollout-<通道>-<时间戳>.json`。
    #[arg(long, value_name = "路径")]
    pub out: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum Channel {
    Alpha,
}

impl Channel {
    fn as_str(self) -> &'static str {
        match self {
            Channel::Alpha => "alpha",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RolloutRecord {
    pub schema_version: u32,
    pub channel: String,
    pub commit_sha: String,
    pub started_utc: String,
    pub finished_utc: String,
    pub artifact: Option<ArtifactIdentity>,
    pub probes: Vec<CapabilityProbe>,
    pub steps: Vec<StepRecord>,
    pub failure_artifacts: Vec<String>,
    pub verdict: Verdict,
    pub rejection: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtifactIdentity {
    pub path: String,
    pub sha256: String,
    pub size_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilityProbe {
    pub name: String,
    pub value: String,
    pub gating: bool,
    pub ok: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepRecord {
    pub id: String,
    pub required: bool,
    pub command: String,
    pub status: StepStatus,
    pub duration_ms: u64,
    pub detail: Option<String>,
    pub evidence_paths: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepStatus {
    Passed,
    Failed,
    Blocked,
    Skipped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    Pass,
    Fail,
}

/// 唯一的结论规则：每个必需步骤都必须是 `Passed`；`Failed`、`Blocked`（没有运行）与 `Skipped`
/// 都判为 `Fail`。步骤在执行前先置为 `Blocked`，只有实际运行才会覆盖，因此被跳过或从未执行的
/// 必需步骤不可能被当作成功。
fn verdict(steps: &[StepRecord]) -> Verdict {
    if steps
        .iter()
        .all(|s| !s.required || s.status == StepStatus::Passed)
    {
        Verdict::Pass
    } else {
        Verdict::Fail
    }
}

/// 门禁涉及的路径一律用绝对路径。harness 在测试进程的当前目录（`cargo test -p` 下是该 crate
/// 的目录）中用 `is_file()` 解析 `LATTICE_BIN`，相对路径会悄悄找不到 release 产物。
struct Ctx {
    root: PathBuf,
    lattice_bin: PathBuf,
    harness_failures: PathBuf,
}

struct Planned {
    id: &'static str,
    display: String,
    action: Action,
}

enum Action {
    CleanWorktree,
    Probes,
    ArtifactIdentity,
    Cmd(Command, Duration),
    CargoTest(Command),
}

pub fn run(args: RolloutArgs) -> anyhow::Result<()> {
    let root = repo_root::repo_root()?;
    let rollout_dir = root.join("target/rollout");
    let harness_failures = rollout_dir.join("harness-failures");
    fs::create_dir_all(&harness_failures).context("创建 rollout 的 harness 失败产物目录")?;
    let ctx = Ctx {
        root: root.clone(),
        lattice_bin: root.join("target/release/lattice"),
        harness_failures,
    };

    let channel = args.channel.as_str();
    let started = chrono::Utc::now();
    let ts = started.format("%Y%m%dT%H%M%SZ");
    let primary = args
        .out
        .clone()
        .unwrap_or_else(|| rollout_dir.join(format!("rollout-{channel}-{ts}.json")));
    let latest = rollout_dir.join(format!("{channel}-latest.json"));
    let record_paths = vec![primary.clone(), latest.clone()];

    let mut plan = build_plan(&ctx);

    // 先取提交 SHA，再判断工作树是否干净，被拒绝的记录也能标明来源。
    let commit_sha = match git_output(&root, &["rev-parse", "HEAD"]) {
        Ok(sha) => sha.trim().to_owned(),
        Err(e) => {
            let mut record = seed_record(channel, "unknown", &plan);
            record.rejection = Some(format!("Git 失败：{e}"));
            persist(&record, &record_paths)?;
            eprintln!("rollout 被拒绝：Git 失败：{e}");
            std::process::exit(2);
        }
    };

    let mut record = seed_record(channel, &commit_sha, &plan);
    persist(&record, &record_paths)?;

    let mut exit_code = 0;
    for planned in &mut plan {
        println!();
        println!("\u{1b}[1m── {} ──\u{1b}[0m", planned.id);
        let start = Instant::now();
        let outcome = execute(planned, &ctx, &mut record);
        let duration_ms = start.elapsed().as_millis() as u64;

        let slot = record
            .steps
            .iter_mut()
            .find(|s| s.id == planned.id)
            .expect("计划步骤已预置在记录中");
        slot.duration_ms = duration_ms;
        let passed = match outcome {
            Ok(()) => {
                slot.status = StepStatus::Passed;
                println!("\u{1b}[0;32m✓\u{1b}[0m {}", planned.id);
                true
            }
            Err(detail) => {
                slot.status = StepStatus::Failed;
                slot.detail = Some(detail.clone());
                eprintln!("\u{1b}[0;31m✗\u{1b}[0m {}: {detail}", planned.id);
                false
            }
        };
        record.finished_utc = now_rfc3339();
        record.verdict = verdict(&record.steps);
        persist(&record, &record_paths)?;

        if !passed {
            if planned.id == "clean_worktree" {
                let dirty = tracked_dirty_paths(
                    &git_output(&root, &["status", "--porcelain=v1", "--untracked-files=no"])
                        .map_err(anyhow::Error::msg)?,
                );
                record.rejection = Some(format!("已跟踪文件有改动（{} 个路径）", dirty.len()));
                persist(&record, &record_paths)?;
                eprintln!(
                    "rollout 要求已跟踪文件没有改动；{} 个已跟踪路径与 HEAD 不同：",
                    dirty.len()
                );
                for p in &dirty {
                    eprintln!("  {p}");
                }
                exit_code = 2;
            } else {
                record.failure_artifacts =
                    collect_failure_artifacts(&ctx.root, &ctx.harness_failures);
                persist(&record, &record_paths)?;
                exit_code = 1;
            }
            break;
        }
    }

    println!();
    println!("rollout 结论：{:?}", record.verdict);
    println!("rollout 记录：{}", primary.display());
    println!("最新记录副本：{}", latest.display());
    std::process::exit(exit_code);
}

fn execute(planned: &mut Planned, ctx: &Ctx, record: &mut RolloutRecord) -> Result<(), String> {
    match &mut planned.action {
        Action::CleanWorktree => clean_worktree(&ctx.root),
        Action::Probes => {
            let probes = collect_probes();
            let failed: Vec<String> = probes
                .iter()
                .filter(|p| p.gating && !p.ok)
                .map(|p| p.name.clone())
                .collect();
            record.probes = probes;
            if failed.is_empty() {
                Ok(())
            } else {
                Err(format!("门禁能力探测未通过：{}", failed.join(", ")))
            }
        }
        Action::ArtifactIdentity => match hash_artifact(&ctx.lattice_bin) {
            Ok(identity) => {
                record.artifact = Some(identity);
                Ok(())
            }
            Err(e) => Err(e),
        },
        Action::Cmd(cmd, timeout) => {
            let out = run_timed(cmd, *timeout).map_err(|e| e.to_string())?;
            status_check(&out)
        }
        Action::CargoTest(cmd) => {
            let out = run_timed(cmd, TEST_TIMEOUT).map_err(|e| e.to_string())?;
            cargo_test_passed(&out)
        }
    }
}

fn build_plan(ctx: &Ctx) -> Vec<Planned> {
    let mut build = Command::new("cargo");
    build.current_dir(&ctx.root).args([
        "build",
        "--locked",
        "--release",
        "-p",
        "lattice-cli",
        "--bin",
        "lattice",
    ]);

    let mut harness_boundary = Command::new("cargo");
    harness_boundary
        .current_dir(&ctx.root)
        .args(["xtask", "harness-boundary"]);

    let mut direct_local = Command::new("cargo");
    direct_local
        .current_dir(&ctx.root)
        .args(["test", "-p", "lattice-system-harness"])
        .env("LATTICE_BIN", &ctx.lattice_bin)
        .env("LATTICE_HARNESS_FAILURE_DIR", &ctx.harness_failures);

    let mut offline = Command::new("cargo");
    offline.current_dir(&ctx.root).args([
        "test",
        "-p",
        "lattice-conversation",
        "-p",
        "lattice-server",
        "-p",
        "lattice-session",
    ]);

    vec![
        Planned {
            id: "clean_worktree",
            display: "<内部：git status --porcelain=v1 --untracked-files=no>".into(),
            action: Action::CleanWorktree,
        },
        Planned {
            id: "capability_probes",
            display: "<内部：能力探测>".into(),
            action: Action::Probes,
        },
        Planned {
            id: "build_release_locked",
            display: display_command(&build),
            action: Action::Cmd(build, BUILD_TIMEOUT),
        },
        Planned {
            id: "artifact_identity",
            display: format!("<内部：sha256 {}>", ctx.lattice_bin.display()),
            action: Action::ArtifactIdentity,
        },
        Planned {
            id: "harness_boundary",
            display: display_command(&harness_boundary),
            action: Action::Cmd(harness_boundary, CMD_TIMEOUT),
        },
        Planned {
            id: "direct_local",
            display: display_command(&direct_local),
            action: Action::CargoTest(direct_local),
        },
        Planned {
            id: "offline_correctness",
            display: display_command(&offline),
            action: Action::CargoTest(offline),
        },
    ]
}

fn seed_record(channel: &str, commit_sha: &str, plan: &[Planned]) -> RolloutRecord {
    let now = now_rfc3339();
    RolloutRecord {
        schema_version: SCHEMA_VERSION,
        channel: channel.to_owned(),
        commit_sha: commit_sha.to_owned(),
        started_utc: now.clone(),
        finished_utc: now,
        artifact: None,
        probes: vec![],
        // 每个计划步骤先置为 `blocked`（未执行）：必需步骤不可能从记录中缺失，也不可能悄悄漏记。
        steps: plan
            .iter()
            .map(|p| StepRecord {
                id: p.id.to_owned(),
                required: true,
                command: p.display.clone(),
                status: StepStatus::Blocked,
                duration_ms: 0,
                detail: None,
                evidence_paths: vec![],
            })
            .collect(),
        failure_artifacts: vec![],
        verdict: Verdict::Fail,
        rejection: None,
    }
}

/// 工作树干净的约定：只要求已跟踪文件没有改动，未跟踪文件不算失败（`--untracked-files=no`）。
/// 源码身份由 `commit_sha` 加上已跟踪文件干净共同确定；未跟踪的临时文件（`.env`、编辑器文件、
/// agent 产物）与被忽略的构建输出（`target/`）不阻塞本地 rollout，否则门禁在真实的开发机上无法使用。
fn clean_worktree(root: &Path) -> Result<(), String> {
    let porcelain = git_output(root, &["status", "--porcelain=v1", "--untracked-files=no"])
        .map_err(|e| format!("git status 失败：{e}"))?;
    let dirty = tracked_dirty_paths(&porcelain);
    if dirty.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "rollout 要求已跟踪文件没有改动；{} 个已跟踪路径与 HEAD 不同：\n{}",
            dirty.len(),
            dirty
                .iter()
                .map(|p| format!("  {p}"))
                .collect::<Vec<_>>()
                .join("\n")
        ))
    }
}

/// 对 `git status --porcelain=v1 --untracked-files=no` 输出的纯判定：任何非空且不以 `??` 开头的行，
/// 都表示已跟踪文件与 HEAD 不同（未暂存的修改、已暂存的改动、已暂存的删除、类型变化）。
/// 只在暂存区有改动的行（如 `M  path`，索引列有值、工作树列为空）同样算作有改动。
fn tracked_dirty_paths(porcelain: &str) -> Vec<String> {
    porcelain
        .lines()
        .filter(|l| !l.is_empty() && !l.starts_with("??"))
        .map(|l| {
            let path = l.get(3..).unwrap_or("").trim();
            // 重命名行形如 `R  old -> new`，报告新路径。
            path.rsplit(" -> ").next().unwrap_or(path).to_owned()
        })
        .collect()
}

fn git_output(root: &Path, args: &[&str]) -> Result<String, String> {
    // 原样返回 stdout，不做 trim：porcelain 状态行按列解析，第一行开头的工作树列空格有意义。
    let out = Command::new("git")
        .current_dir(root)
        .args(args)
        .output()
        .map_err(|e| format!("无法运行 git：{e}"))?;
    if !out.status.success() {
        return Err(format!(
            "`git {}` 失败：{}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn collect_probes() -> Vec<CapabilityProbe> {
    let mut probes = Vec::new();

    // 与 `cargo xtask ci` 一致：核验 `rust-toolchain.toml` 固定的 rustc 1.96.0。
    let rustc = Command::new("rustc").arg("--version").output();
    let (value, ok) = match rustc {
        Ok(o) if o.status.success() => {
            let v = String::from_utf8_lossy(&o.stdout).trim().to_owned();
            let ok = v.contains("1.96.0");
            (v, ok)
        }
        Ok(_) => ("`rustc --version` 执行失败".to_owned(), false),
        Err(e) => (format!("无法运行 `rustc --version`：{e}"), false),
    };
    probes.push(CapabilityProbe {
        name: "rustc_version".into(),
        value,
        gating: true,
        ok,
    });

    let git = Command::new("git").arg("--version").output();
    let (value, ok) = match git {
        Ok(o) if o.status.success() => (String::from_utf8_lossy(&o.stdout).trim().to_owned(), true),
        _ => ("`git --version` 执行失败".to_owned(), false),
    };
    probes.push(CapabilityProbe {
        name: "git_version".into(),
        value,
        gating: false,
        ok,
    });

    // 只记录 KVM 是否存在，不参与门禁判定：主机物理隔离不是 Direct-Local 的前提，不影响任何步骤。
    probes.push(CapabilityProbe {
        name: "dev_kvm_present".into(),
        value: Path::new("/dev/kvm").exists().to_string(),
        gating: false,
        ok: true,
    });
    probes.push(CapabilityProbe {
        name: "os".into(),
        value: env::consts::OS.to_owned(),
        gating: false,
        ok: true,
    });
    probes.push(CapabilityProbe {
        name: "arch".into(),
        value: env::consts::ARCH.to_owned(),
        gating: false,
        ok: true,
    });
    probes
}

fn hash_artifact(path: &Path) -> Result<ArtifactIdentity, String> {
    let bytes = fs::read(path).map_err(|e| format!("读取 {} 失败：{e}", path.display()))?;
    Ok(ArtifactIdentity {
        path: path.display().to_string(),
        sha256: hex(&Sha256::digest(&bytes)),
        size_bytes: bytes.len() as u64,
    })
}

struct TimedOutput {
    success: bool,
    timed_out: bool,
    output: String,
}

fn status_check(out: &TimedOutput) -> Result<(), String> {
    if out.timed_out {
        return Err("步骤超时".to_owned());
    }
    if !out.success {
        return Err(format!("命令失败：\n{}", tail(&out.output)));
    }
    Ok(())
}

/// `cargo test` 步骤只有在退出码为 0、且各 `test result: ok. N passed` 的通过数之和大于 0 时才算通过。
/// 过滤条件匹配不到任何测试时 cargo test 同样以 0 退出（即 v1.4 `commit_baseline` 的失败方式），
/// 这按门禁失败处理，不算通过。
fn cargo_test_passed(out: &TimedOutput) -> Result<(), String> {
    status_check(out)?;
    if passed_test_count(&out.output) == 0 {
        return Err(
            "cargo test 以 0 退出，但没有运行任何测试（过滤条件没有匹配到测试，按门禁失败处理）"
                .to_owned(),
        );
    }
    Ok(())
}

fn passed_test_count(output: &str) -> u64 {
    let re = regex::Regex::new(r"test result: \w+\. (\d+) passed").expect("正则表达式合法");
    re.captures_iter(output)
        .filter_map(|c| c[1].parse::<u64>().ok())
        .sum()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// 取步骤输出的最后约 800 字节，作为记录中的失败详情。
///
/// 不能按原始字节下标切片：切点落在多字节字符内部时 `&s[s.len() - 800..]` 会 panic，而步骤输出
/// 经常含有非 ASCII 字符（rustc 诊断会引用源码，门禁覆盖的 crate 有非 ASCII 的注释与测试字面量）。
/// 在这里 panic 会把一次普通的步骤失败变成门禁崩溃：步骤状态停在预置的 `Blocked`，`detail` 仍为
/// `None`，不写失败产物，进程以 101 而不是 1 退出，恰好丢掉本函数要提供的诊断。
fn tail(output: &str) -> String {
    let trimmed = output.trim();
    if trimmed.len() <= TAIL_MAX_BYTES {
        return trimmed.to_owned();
    }
    // 从切点向后找到第一个字符边界，切片因此总是合法，且不超过 TAIL_MAX_BYTES。
    let mut cut = trimmed.len() - TAIL_MAX_BYTES;
    while cut < trimmed.len() && !trimmed.is_char_boundary(cut) {
        cut += 1;
    }
    format!("…{}", &trimmed[cut..])
}

/// 在硬超时下运行命令，合并捕获 stdout 与 stderr。与 `xtask/src/ci.rs` 一样按进程组终止，
/// 超时时整棵进程树（cargo、rustc 与测试二进制）一起结束，不留下孤儿进程。
fn run_timed(cmd: &mut Command, timeout: Duration) -> anyhow::Result<TimedOutput> {
    let file = tempfile::NamedTempFile::new().context("创建步骤输出临时文件")?;
    // 两个输出流共享同一个文件偏移：调两次 `reopen()` 会拿到两个偏移独立的描述符
    //（`reopen` 只是普通 `open`，不带 `O_APPEND`），交错写入时 stdout 与 stderr 会互相覆盖，
    // 破坏后续 `passed_test_count` 对 `test result:` 的解析与 `tail()` 截取的失败详情。
    // `try_clone` 复制同一个底层文件表项，两个流按到达顺序追加。
    let out = file.reopen().context("为 stdout 重新打开步骤输出文件")?;
    let err = out
        .try_clone()
        .context("为 stderr 复制步骤输出文件描述符")?;
    cmd.stdout(Stdio::from(out))
        .stderr(Stdio::from(err))
        // 子进程自成进程组，超时时可以终止整棵进程树。
        .process_group(0);
    let start = Instant::now();
    let mut child = cmd.spawn().context("启动步骤命令")?;
    let pgid = child.id();

    let (success, timed_out) = loop {
        match child.try_wait()? {
            Some(status) => break (status.success(), false),
            None => {
                if start.elapsed() >= timeout {
                    let _ = Command::new("kill")
                        .arg("-KILL")
                        .arg(format!("-{pgid}"))
                        .stdout(Stdio::null())
                        .stderr(Stdio::null())
                        .status();
                    let _ = child.kill();
                    let _ = child.wait();
                    break (false, true);
                }
                std::thread::sleep(Duration::from_millis(200));
            }
        }
    };

    // 用有损解码，不用 `read_to_string(..).unwrap_or_default()`：后者遇到解码失败会悄悄得到空字符串，
    // 空输出让 `passed_test_count` 返回 0，`cargo_test_passed` 随之报告“以 0 退出，但没有运行任何测试”，
    // 合格的产物被拒绝且没有可诊断的详情。已收到的字节全部保留。
    let output = String::from_utf8_lossy(&fs::read(file.path()).unwrap_or_default()).into_owned();
    if !output.is_empty() {
        print!("{output}");
    }
    Ok(TimedOutput {
        success,
        timed_out,
        output,
    })
}

fn collect_failure_artifacts(root: &Path, dir: &Path) -> Vec<String> {
    let mut out = Vec::new();
    if !dir.is_dir() {
        return out;
    }
    for entry in walkdir::WalkDir::new(dir)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        if entry.file_type().is_file() {
            out.push(rel_path(root, entry.path()));
        }
    }
    out.sort();
    out
}

fn rel_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .display()
        .to_string()
}

/// 记录 `Command` 的实际参数与显式设置的环境变量。命令不经 shell 执行，不涉及 `&&`、`env X=Y`
/// 之类的 shell 语法。
fn display_command(cmd: &Command) -> String {
    let mut s = String::new();
    for (k, v) in cmd.get_envs() {
        if let Some(v) = v {
            s.push_str(&k.to_string_lossy());
            s.push('=');
            s.push_str(&v.to_string_lossy());
            s.push(' ');
        }
    }
    s.push_str(&cmd.get_program().to_string_lossy());
    for a in cmd.get_args() {
        s.push(' ');
        s.push_str(&a.to_string_lossy());
    }
    s
}

fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

fn persist(record: &RolloutRecord, paths: &[PathBuf]) -> anyhow::Result<()> {
    let json = serde_json::to_string_pretty(record).context("序列化 rollout 记录")?;
    for path in paths {
        write_atomic(path, &json).with_context(|| format!("写入 {}", path.display()))?;
    }
    Ok(())
}

/// 原子重写（先写临时文件再重命名）：运行中途崩溃也会留下一份完整、结论为失败的记录，
/// 不完整的记录不可能被当作成功。
fn write_atomic(path: &Path, contents: &str) -> std::io::Result<()> {
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, contents)?;
    fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `tail` 不能按字节切片：切点落在多字节字符内部时会 panic。cargo 诊断经常带有非 ASCII 的路径与
    /// 测试消息，panic 会让 rollout 在报告步骤失败时中止，掩盖原始错误。
    #[test]
    fn tail_truncates_on_a_char_boundary() {
        // 每个字符 3 字节，直接用 `len() - 800` 作切点会落在字符中间。
        let multibyte = "错误".repeat(500);
        assert!(multibyte.len() > TAIL_MAX_BYTES);
        let out = tail(&multibyte);
        assert!(out.starts_with('\u{2026}'), "应以省略号开头");
        // 长度仍有上限；结果是 String，本身就是合法的 UTF-8。
        assert!(out.len() <= TAIL_MAX_BYTES + '\u{2026}'.len_utf8() + 3);
        assert!(multibyte.ends_with(out.trim_start_matches('\u{2026}')));

        // 短于上限的 ASCII 文本只去掉首尾空白。
        assert_eq!(tail("  short  "), "short");
    }

    fn step(id: &str, status: StepStatus) -> StepRecord {
        StepRecord {
            id: id.to_owned(),
            required: true,
            command: "cmd".to_owned(),
            status,
            duration_ms: 1,
            detail: None,
            evidence_paths: vec![],
        }
    }

    #[test]
    fn verdict_passes_only_when_all_required_steps_passed() {
        let steps = vec![step("a", StepStatus::Passed), step("b", StepStatus::Passed)];
        assert_eq!(verdict(&steps), Verdict::Pass);
    }

    #[test]
    fn verdict_fails_when_required_step_skipped_blocked_or_failed() {
        for bad in [StepStatus::Skipped, StepStatus::Blocked, StepStatus::Failed] {
            let steps = vec![step("a", StepStatus::Passed), step("b", bad)];
            assert_eq!(
                verdict(&steps),
                Verdict::Fail,
                "必需步骤为 {bad:?} 时门禁应判为失败"
            );
        }
    }

    #[test]
    fn build_plan_runs_production_release_steps_without_test_features() {
        let temp = tempfile::tempdir().expect("创建临时目录");
        let ctx = Ctx {
            root: temp.path().to_path_buf(),
            lattice_bin: temp.path().join("target/release/lattice"),
            harness_failures: temp.path().join("harness-failures"),
        };
        let plan = build_plan(&ctx);
        let ids: Vec<&str> = plan.iter().map(|s| s.id).collect();
        assert_eq!(
            ids,
            vec![
                "clean_worktree",
                "capability_probes",
                "build_release_locked",
                "artifact_identity",
                "harness_boundary",
                "direct_local",
                "offline_correctness",
            ],
            "rollout 步骤顺序应只包含现行生产发布验证步骤"
        );
        for id in [
            "build_release_locked",
            "direct_local",
            "offline_correctness",
        ] {
            let step = plan.iter().find(|s| s.id == id).expect("计划步骤存在");
            assert!(
                !step.display.contains("--features"),
                "release 产物及其验证步骤不应启用测试 feature ({id}): {}",
                step.display
            );
        }
    }

    fn sample_record() -> RolloutRecord {
        RolloutRecord {
            schema_version: SCHEMA_VERSION,
            channel: "alpha".to_owned(),
            commit_sha: "9f4c2b7a1d3e5f6089aabbccddeeff0011223344".to_owned(),
            started_utc: "2026-07-26T03:00:00Z".to_owned(),
            finished_utc: "2026-07-26T03:24:11Z".to_owned(),
            artifact: Some(ArtifactIdentity {
                path: "/repo/target/release/lattice".to_owned(),
                sha256: "b".repeat(64),
                size_bytes: 48_721_544,
            }),
            probes: vec![CapabilityProbe {
                name: "rustc_version".to_owned(),
                value: "rustc 1.96.0".to_owned(),
                gating: true,
                ok: true,
            }],
            steps: vec![
                step("clean_worktree", StepStatus::Passed),
                step("offline_correctness", StepStatus::Passed),
            ],
            failure_artifacts: vec![],
            verdict: Verdict::Pass,
            rejection: None,
        }
    }

    #[test]
    fn record_round_trips_through_serde_with_mandatory_fields() {
        let record = sample_record();
        let json = serde_json::to_string_pretty(&record).unwrap();
        for key in [
            "schema_version",
            "channel",
            "commit_sha",
            "started_utc",
            "finished_utc",
            "artifact",
            "probes",
            "steps",
            "failure_artifacts",
            "verdict",
            "rejection",
        ] {
            assert!(json.contains(&format!("\"{key}\"")), "缺少字段 {key}");
        }
        let back: RolloutRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(back.schema_version, SCHEMA_VERSION);
        assert_eq!(back.channel, "alpha");
        assert_eq!(back.commit_sha.len(), 40);
        assert!(back.started_utc.ends_with('Z'));
        assert_eq!(back.steps.len(), 2);
        assert_eq!(back.verdict, Verdict::Pass);
        let art = back.artifact.expect("记录应包含产物身份");
        assert_eq!(art.sha256.len(), 64);
        assert!(art.size_bytes > 0);
    }

    #[test]
    fn dirty_worktree_detects_staged_and_unstaged_tracked_changes() {
        let porcelain =
            " M crates/foo.rs\nM  crates/staged_only.rs\nA  crates/added.rs\n?? scratch.env\n";
        let dirty = tracked_dirty_paths(porcelain);
        assert_eq!(
            dirty,
            vec!["crates/foo.rs", "crates/staged_only.rs", "crates/added.rs"]
        );
        // 只在暂存区有改动的行（索引列有值、工作树列为空）算作有改动。
        assert_eq!(
            tracked_dirty_paths("M  only_staged.rs"),
            vec!["only_staged.rs"]
        );
        // 已暂存的删除也算。
        assert_eq!(tracked_dirty_paths("D  gone.rs"), vec!["gone.rs"]);
        assert!(tracked_dirty_paths("").is_empty());
        // 按已跟踪工作树的约定，只有未跟踪文件的工作树算干净。
        assert!(tracked_dirty_paths("?? untracked_only").is_empty());
    }

    #[test]
    fn zero_pass_output_fails_the_guard() {
        // v1.4 `commit_baseline` 的失败方式：以 0 退出，0 个测试通过。
        let vacuous = "running 3 tests\n\n\n3 filtered out\n\ntest result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 3 filtered out\n";
        let out = TimedOutput {
            success: true,
            timed_out: false,
            output: vacuous.to_owned(),
        };
        assert!(cargo_test_passed(&out).is_err());

        let green = "test result: ok. 5 passed; 0 failed\ntest result: ok. 2 passed; 0 failed\n";
        let out = TimedOutput {
            success: true,
            timed_out: false,
            output: green.to_owned(),
        };
        assert!(cargo_test_passed(&out).is_ok());

        let out = TimedOutput {
            success: false,
            timed_out: false,
            output: green.to_owned(),
        };
        assert!(cargo_test_passed(&out).is_err());
    }

    /// 两个子进程输出流共享同一个文件偏移；若调两次 `reopen()` 拿到独立偏移，输出会互相覆盖，
    /// 破坏 `passed_test_count` 与 `tail` 读取的捕获文本。
    #[test]
    fn run_timed_interleaves_stdout_and_stderr_without_overwriting() {
        let mut cmd = Command::new("sh");
        cmd.arg("-c")
            // 先写 stderr，再写 stdout，再写 stderr：两个流的偏移各自独立时，第二次写 stderr 会覆盖 stdout 那一行。
            .arg(
                "echo compiling >&2; echo 'test result: ok. 7 passed; 0 failed'; echo running >&2",
            );

        let out = run_timed(&mut cmd, Duration::from_secs(30)).expect("运行测试命令");

        assert!(out.success, "测试命令应成功：{:?}", out.output);
        assert!(
            out.output.contains("compiling") && out.output.contains("running"),
            "stderr 的行应完整保留：{:?}",
            out.output
        );
        assert!(
            out.output.contains("test result: ok. 7 passed"),
            "stdout 的行应完整保留：{:?}",
            out.output
        );
        assert_eq!(
            passed_test_count(&out.output),
            7,
            "门禁应能从捕获文本中解析出通过数：{:?}",
            out.output
        );
    }
}
