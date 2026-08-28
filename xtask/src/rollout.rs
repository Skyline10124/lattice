// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Alpha rollout gate (#47).
//!
//! Runs the release evidence lane against a LOCKED RELEASE binary and records
//! a versioned JSON record:
//!
//!   1. clean tracked worktree check (explicit rejection on tracked dirt)
//!   2. capability probes (rustc pin, wasm32 target, informational host facts)
//!   3. `cargo build --locked --release -p lattice-cli --bin lattice`
//!   4. artifact identity (sha256 + size)
//!   5. dependency/architecture boundary checks (public xtask contracts)
//!   6. Direct-Local harness scenarios against the release binary (`LATTICE_BIN`)
//!   7. offline correctness suites (with an anti-vacuous zero-pass guard)
//!   8. release operator Component audit matrix (exact-named test)
//!   9. `lattice --json lancet doctor` (JSON saved as evidence)
//!
//! SKIPPED IS NOT PASS: every planned step is seeded `blocked` and only
//! overwritten by actually running it; the verdict is `pass` iff every
//! required step is `passed` (see [`verdict`]).
//!
//! No KVM, no Microsandbox, no OCI image, no physical-isolation dependency.

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

/// Max bytes of a failed step's captured output retained in the record.
const TAIL_MAX_BYTES: usize = 800;

/// Boundary checks only scan/metadata the workspace; keep them bounded.
const CMD_TIMEOUT: Duration = Duration::from_secs(600);
/// A cold locked release build is the long pole of the gate.
const BUILD_TIMEOUT: Duration = Duration::from_secs(3600);
/// Direct-Local scenarios have their own 30s/180s budgets; this is the
/// whole-suite backstop so one hung test binary cannot stall the gate.
const TEST_TIMEOUT: Duration = Duration::from_secs(1800);
const DOCTOR_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Args, Debug)]
pub struct RolloutArgs {
    /// Rollout channel (`alpha` is the only valid value today).
    #[arg(long, value_enum)]
    pub channel: Channel,
    /// Override the primary record output path
    /// (default: target/rollout/rollout-<channel>-<timestamp>.json).
    #[arg(long)]
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

/// The ONLY verdict rule: every required step must be `Passed`. `Failed`,
/// `Blocked` (never ran), and `Skipped` all yield `Fail`. Steps are seeded
/// `Blocked` before execution and overwritten only by actually running them,
/// so a required step that was skipped or never attempted can never read as
/// success.
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

/// Absolute paths for everything the gate touches. `LATTICE_BIN` is resolved
/// by the harness via `is_file()` from the TEST-PROCESS cwd (the crate dir
/// under `cargo test -p`), so relative paths silently miss the release
/// artifact; absolute paths are mandatory here.
struct Ctx {
    root: PathBuf,
    lattice_bin: PathBuf,
    harness_failures: PathBuf,
    evidence_dir: PathBuf,
    doctor_home: PathBuf,
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
    Doctor(Command),
}

pub fn run(args: RolloutArgs) -> anyhow::Result<()> {
    let root = repo_root::repo_root()?;
    let rollout_dir = root.join("target/rollout");
    let evidence_dir = rollout_dir.join("evidence");
    let harness_failures = rollout_dir.join("harness-failures");
    fs::create_dir_all(&evidence_dir).context("create rollout evidence dir")?;
    fs::create_dir_all(&harness_failures).context("create rollout harness-failure dir")?;
    // Temp HOME for the doctor step: the gate must be reproducible regardless
    // of the operator's interactive environment.
    let doctor_home = tempfile::tempdir().context("create doctor temp HOME")?;
    let ctx = Ctx {
        root: root.clone(),
        lattice_bin: root.join("target/release/lattice"),
        harness_failures,
        evidence_dir,
        doctor_home: doctor_home.path().to_path_buf(),
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

    // Commit SHA is captured BEFORE the dirtiness decision so even a
    // rejection record identifies its source.
    let commit_sha = match git_output(&root, &["rev-parse", "HEAD"]) {
        Ok(sha) => sha.trim().to_owned(),
        Err(e) => {
            let mut record = seed_record(channel, "unknown", &plan);
            record.rejection = Some(format!("git failure: {e}"));
            persist(&record, &record_paths)?;
            eprintln!("rollout rejected: git failure: {e}");
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
            .expect("seeded step exists");
        slot.duration_ms = duration_ms;
        let passed = match outcome {
            Ok(evidence) => {
                slot.status = StepStatus::Passed;
                slot.evidence_paths = evidence;
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
                record.rejection = Some(format!("tracked-dirty worktree ({} paths)", dirty.len()));
                persist(&record, &record_paths)?;
                eprintln!(
                    "rollout requires a clean tracked worktree; {} tracked path(s) differ from HEAD:",
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
    println!("verdict: {:?}", record.verdict);
    println!("rollout record: {}", primary.display());
    println!("latest copy:    {}", latest.display());
    std::process::exit(exit_code);
}

fn execute(
    planned: &mut Planned,
    ctx: &Ctx,
    record: &mut RolloutRecord,
) -> Result<Vec<String>, String> {
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
                Ok(vec![])
            } else {
                Err(format!(
                    "gating capability probe(s) failed: {}",
                    failed.join(", ")
                ))
            }
        }
        Action::ArtifactIdentity => match hash_artifact(&ctx.lattice_bin) {
            Ok(identity) => {
                record.artifact = Some(identity);
                Ok(vec![])
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
        Action::Doctor(cmd) => {
            let out = run_timed(cmd, DOCTOR_TIMEOUT).map_err(|e| e.to_string())?;
            let evidence = ctx.evidence_dir.join("doctor.json");
            fs::write(&evidence, &out.output)
                .map_err(|e| format!("write {}: {e}", evidence.display()))?;
            status_check(&out).map(|_| vec![rel_path(&ctx.root, &evidence)])
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

    let mut lancet_boundary = Command::new("cargo");
    lancet_boundary
        .current_dir(&ctx.root)
        .args(["xtask", "lancet-boundary"]);

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
        "lancet-protocol",
        "-p",
        "lancet-runtime",
        "-p",
        "lancet-authority",
        "-p",
        "lancet-audit",
        "-p",
        "lancet-api",
        "-p",
        "lattice-api",
        "-p",
        "lattice-session",
    ]);
    // Exact-named, feature-gated Component audit scenario so a renamed test
    // fails loudly instead of filtering to zero.
    let mut release_operator_matrix = Command::new("cargo");
    release_operator_matrix
        .current_dir(&ctx.root)
        .args([
            "test",
            "-p",
            "lattice-system-harness",
            "--features",
            "release-operator",
            "--test",
            "release_operator",
            "release_operator_component_matrix",
            "--",
            "--exact",
        ])
        .env("LATTICE_BIN", &ctx.lattice_bin)
        .env("LATTICE_HARNESS_FAILURE_DIR", &ctx.harness_failures);

    // Doctor validates the temporary Local Workspace and generated catalog; run
    // with a cleared environment for reproducibility.
    let mut doctor = Command::new(&ctx.lattice_bin);
    doctor
        .args(["--json", "lancet", "doctor"])
        .current_dir(&ctx.doctor_home)
        .env_clear()
        .env("HOME", &ctx.doctor_home)
        .env("PATH", env::var("PATH").unwrap_or_default());

    vec![
        Planned {
            id: "clean_worktree",
            display: "<internal: git status --porcelain=v1 --untracked-files=no>".into(),
            action: Action::CleanWorktree,
        },
        Planned {
            id: "capability_probes",
            display: "<internal: probes>".into(),
            action: Action::Probes,
        },
        Planned {
            id: "build_release_locked",
            display: display_command(&build, false),
            action: Action::Cmd(build, BUILD_TIMEOUT),
        },
        Planned {
            id: "artifact_identity",
            display: format!("<internal: sha256 {}>", ctx.lattice_bin.display()),
            action: Action::ArtifactIdentity,
        },
        Planned {
            id: "harness_boundary",
            display: display_command(&harness_boundary, false),
            action: Action::Cmd(harness_boundary, CMD_TIMEOUT),
        },
        Planned {
            id: "lancet_boundary",
            display: display_command(&lancet_boundary, false),
            action: Action::Cmd(lancet_boundary, CMD_TIMEOUT),
        },
        Planned {
            id: "direct_local",
            display: display_command(&direct_local, false),
            action: Action::CargoTest(direct_local),
        },
        Planned {
            id: "offline_correctness",
            display: display_command(&offline, false),
            action: Action::CargoTest(offline),
        },
        Planned {
            id: "release_operator_matrix",
            display: display_command(&release_operator_matrix, false),
            action: Action::CargoTest(release_operator_matrix),
        },
        Planned {
            id: "doctor",
            display: display_command(&doctor, true),
            action: Action::Doctor(doctor),
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
        // Every planned step seeded `blocked` ("not executed"): it is
        // structurally impossible for a required step to be absent from the
        // record or to remain silently unaccounted.
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

/// The clean-worktree contract. Untracked files are intentionally NOT fatal
/// (`--untracked-files=no`): the contract is a clean *tracked* worktree.
/// Source identity is pinned by commit_sha + tracked cleanliness; untracked
/// scratch (.env, editor files, agent work products) and ignored build output
/// (target/) must not block a local rollout, or the gate would be unusable on
/// any real developer machine.
fn clean_worktree(root: &Path) -> Result<Vec<String>, String> {
    let porcelain = git_output(root, &["status", "--porcelain=v1", "--untracked-files=no"])
        .map_err(|e| format!("git status failed: {e}"))?;
    let dirty = tracked_dirty_paths(&porcelain);
    if dirty.is_empty() {
        Ok(vec![])
    } else {
        Err(format!(
            "rollout requires a clean tracked worktree; {} tracked path(s) differ from HEAD:\n{}",
            dirty.len(),
            dirty
                .iter()
                .map(|p| format!("  {p}"))
                .collect::<Vec<_>>()
                .join("\n")
        ))
    }
}

/// Pure predicate over `git status --porcelain=v1 --untracked-files=no`
/// output: any non-empty, non-`??` line is a tracked deviation from HEAD
/// (unstaged modifications, staged changes, staged deletions, type changes).
/// Staged-only lines like `M  path` (index column set, worktree column blank)
/// count as dirty.
fn tracked_dirty_paths(porcelain: &str) -> Vec<String> {
    porcelain
        .lines()
        .filter(|l| !l.is_empty() && !l.starts_with("??"))
        .map(|l| {
            let path = l.get(3..).unwrap_or("").trim();
            // Rename lines look like `R  old -> new`; report the new path.
            path.rsplit(" -> ").next().unwrap_or(path).to_owned()
        })
        .collect()
}

fn git_output(root: &Path, args: &[&str]) -> Result<String, String> {
    // Raw stdout, NOT trimmed: porcelain status lines are column-based and a
    // leading worktree-column space on the first line is significant.
    let out = Command::new("git")
        .current_dir(root)
        .args(args)
        .output()
        .map_err(|e| format!("failed to run git: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "`git {}` failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn collect_probes() -> Vec<CapabilityProbe> {
    let mut probes = Vec::new();

    // Same toolchain contract as `cargo xtask ci`: pinned 1.96.0 +
    // wasm32-unknown-unknown std.
    let rustc = Command::new("rustc").arg("--version").output();
    let (value, ok) = match rustc {
        Ok(o) if o.status.success() => {
            let v = String::from_utf8_lossy(&o.stdout).trim().to_owned();
            let ok = v.contains("1.96.0");
            (v, ok)
        }
        Ok(_) => ("`rustc --version` failed".to_owned(), false),
        Err(e) => (format!("failed to run `rustc --version`: {e}"), false),
    };
    probes.push(CapabilityProbe {
        name: "rustc_version".into(),
        value,
        gating: true,
        ok,
    });

    let targets = Command::new("rustup")
        .args(["target", "list", "--installed"])
        .output();
    let ok = targets
        .as_ref()
        .map(|o| {
            o.status.success()
                && String::from_utf8_lossy(&o.stdout)
                    .lines()
                    .any(|l| l.trim() == "wasm32-unknown-unknown")
        })
        .unwrap_or(false);
    probes.push(CapabilityProbe {
        name: "wasm32_target_installed".into(),
        value: "wasm32-unknown-unknown".into(),
        gating: true,
        ok,
    });

    // The Component encoder. Non-gating (rollout builds the CLI, not the
    // Source Tool Components), but recorded so a release record can answer
    // "which encoder produced the embedded Components" — the `[artifact]
    // toolchain` fingerprint does not carry it yet. See #73 / #80.
    // `ok` tracks whether the probe *answered*, not whether the capability is
    // required — same convention as `git_version` below. A missing encoder
    // still must not gate (rollout builds the CLI, not the Components), but
    // recording ok:true for "not installed" would make the release record
    // claim a successful probe of a tool that was never found.
    let (value, ok) = match crate::ci::wasm_tools_version() {
        Some(v) => (v, true),
        None => ("`wasm-tools --version` unavailable".to_owned(), false),
    };
    probes.push(CapabilityProbe {
        name: "wasm_tools_version".into(),
        value,
        gating: false,
        ok,
    });

    let git = Command::new("git").arg("--version").output();
    let (value, ok) = match git {
        Ok(o) if o.status.success() => (String::from_utf8_lossy(&o.stdout).trim().to_owned(), true),
        _ => ("`git --version` failed".to_owned(), false),
    };
    probes.push(CapabilityProbe {
        name: "git_version".into(),
        value,
        gating: false,
        ok,
    });

    // KVM presence is recorded but NEVER gating: host physical isolation is
    // not a Direct-Local prerequisite and must not influence any step.
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
    let bytes = fs::read(path).map_err(|e| format!("read {}: {e}", path.display()))?;
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

fn status_check(out: &TimedOutput) -> Result<Vec<String>, String> {
    if out.timed_out {
        return Err("step timed out".to_owned());
    }
    if !out.success {
        return Err(format!("command failed:\n{}", tail(&out.output)));
    }
    Ok(vec![])
}

/// A `cargo test` step passes only if the exit status is 0 AND the summed
/// `test result: ok. N passed` counts are > 0. A filter that matches zero
/// tests exits 0 vacuously (the v1.4 `commit_baseline` failure mode); that is
/// a gate failure, never a pass.
fn cargo_test_passed(out: &TimedOutput) -> Result<Vec<String>, String> {
    status_check(out)?;
    if passed_test_count(&out.output) == 0 {
        return Err(
            "cargo test exited 0 but ran 0 tests (vacuous filter is a gate failure)".to_owned(),
        );
    }
    Ok(vec![])
}

fn passed_test_count(output: &str) -> u64 {
    let re = regex::Regex::new(r"test result: \w+\. (\d+) passed").expect("valid regex");
    re.captures_iter(output)
        .filter_map(|c| c[1].parse::<u64>().ok())
        .sum()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Last ~800 bytes of a step's output, for the failure detail stored in the
/// record.
///
/// Must not slice on a raw byte index: `&s[s.len() - 800..]` panics when the cut
/// lands inside a multi-byte codepoint, and step output routinely contains
/// non-ASCII (rustc diagnostics quote source, and gate-covered crates have
/// non-ASCII comments and test literals). A panic here would replace an ordinary
/// step failure with a crashed gate: the step's status would stay seeded
/// `Blocked`, `detail` would stay `None`, no failure artifact would be written,
/// and the process would exit 101 instead of 1 — losing exactly the diagnostic
/// this function exists to produce.
fn tail(output: &str) -> String {
    let trimmed = output.trim();
    if trimmed.len() <= TAIL_MAX_BYTES {
        return trimmed.to_owned();
    }
    // Walk forward to the next char boundary at or after the cut point, so the
    // slice is always valid and never longer than TAIL_MAX_BYTES.
    let mut cut = trimmed.len() - TAIL_MAX_BYTES;
    while cut < trimmed.len() && !trimmed.is_char_boundary(cut) {
        cut += 1;
    }
    format!("…{}", &trimmed[cut..])
}

/// Run a command under a hard timeout, capturing combined stdout+stderr.
/// Mirrors `xtask/src/ci.rs`'s process-group kill so a timeout takes down the
/// whole tree (cargo + rustc + test binaries) instead of orphaning them.
fn run_timed(cmd: &mut Command, timeout: Duration) -> anyhow::Result<TimedOutput> {
    let file = tempfile::NamedTempFile::new().context("create step output temp file")?;
    // Both streams must share ONE file offset. `reopen()` twice would hand out
    // two descriptors with independent offsets (tempfile's `reopen` is a plain
    // `OpenOptions::read().write().open()` — no `O_APPEND`), so interleaved
    // stdout/stderr writes overwrite each other instead of appending. That
    // corrupts everything downstream: `passed_test_count` parses `test result:`
    // out of this text, `tail()` builds the failure detail stored in the record,
    // and `Action::Doctor` writes it straight out as `evidence/doctor.json`.
    // `try_clone` duplicates the descriptor, which shares the offset, so writes
    // from either stream land in order.
    let out = file.reopen().context("reopen step output for stdout")?;
    let err = out.try_clone().context("clone step output for stderr")?;
    cmd.stdout(Stdio::from(out))
        .stderr(Stdio::from(err))
        // Own process group so a timeout can kill the whole tree.
        .process_group(0);
    let start = Instant::now();
    let mut child = cmd.spawn().context("spawn step command")?;
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

    // Lossy rather than `read_to_string(..).unwrap_or_default()`: that silently
    // turned any decode failure into an EMPTY string, and empty output makes
    // `passed_test_count` return 0, which `cargo_test_passed` reports as "exited
    // 0 but ran 0 tests" — a green artifact rejected with no diagnosable detail.
    // Keep whatever bytes arrived instead of discarding the lot.
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

/// Records the real argv + explicit env of a `Command` (never executed via a
/// shell, so no `&&`/`env X=Y` shell syntax is involved).
fn display_command(cmd: &Command, cleared_env: bool) -> String {
    let mut s = String::new();
    if cleared_env {
        s.push_str("env -i ");
    }
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
    let json = serde_json::to_string_pretty(record).context("serialize rollout record")?;
    for path in paths {
        write_atomic(path, &json).with_context(|| format!("write {}", path.display()))?;
    }
    Ok(())
}

/// Atomic rewrite (temp file + rename) so a crash mid-run still leaves a
/// valid, failing record — a partial record can never read as success.
fn write_atomic(path: &Path, contents: &str) -> std::io::Result<()> {
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, contents)?;
    fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `tail` byte-sliced the captured output, which panics when the cutoff
    /// lands inside a multibyte character. Cargo diagnostics routinely carry
    /// non-ASCII paths and test messages, so this aborted the rollout while it
    /// was reporting a step failure, masking the original error.
    #[test]
    fn tail_truncates_on_a_char_boundary() {
        // 3 bytes per char, so the naive `len() - 800` cutoff lands mid-char.
        let multibyte = "错误".repeat(500);
        assert!(multibyte.len() > TAIL_MAX_BYTES);
        let out = tail(&multibyte);
        assert!(out.starts_with('\u{2026}'), "expected the ellipsis marker");
        // Still bounded, and valid UTF-8 by construction (it is a String).
        assert!(out.len() <= TAIL_MAX_BYTES + '\u{2026}'.len_utf8() + 3);
        assert!(multibyte.ends_with(out.trim_start_matches('\u{2026}')));

        // ASCII shorter than the cap is returned untouched.
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
                "{bad:?} required step must fail the gate"
            );
        }
    }

    #[test]
    fn release_operator_step_targets_component_matrix() {
        let temp = tempfile::tempdir().expect("tempdir");
        let ctx = Ctx {
            root: temp.path().to_path_buf(),
            lattice_bin: temp.path().join("target/release/lattice"),
            harness_failures: temp.path().join("harness-failures"),
            evidence_dir: temp.path().join("rollout-evidence"),
            doctor_home: temp.path().join("doctor-home"),
        };
        let plan = build_plan(&ctx);
        let step = plan
            .iter()
            .find(|step| step.id == "release_operator_matrix")
            .expect("release operator step");

        assert!(
            step.display.contains("--features release-operator")
                && step.display.contains("--test release_operator")
                && step.display.contains("release_operator_component_matrix"),
            "release lane must execute its own release-binary matrix: {}",
            step.display
        );
        assert!(
            !step.display.contains("--test direct_operator"),
            "release lane must not rerun the Direct-Local scenario: {}",
            step.display
        );
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
                step("doctor", StepStatus::Passed),
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
            assert!(json.contains(&format!("\"{key}\"")), "missing key {key}");
        }
        let back: RolloutRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(back.schema_version, SCHEMA_VERSION);
        assert_eq!(back.channel, "alpha");
        assert_eq!(back.commit_sha.len(), 40);
        assert!(back.started_utc.ends_with('Z'));
        assert_eq!(back.steps.len(), 2);
        assert_eq!(back.verdict, Verdict::Pass);
        let art = back.artifact.expect("artifact present");
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
        // Staged-only line (index column set, worktree column blank) is dirty.
        assert_eq!(
            tracked_dirty_paths("M  only_staged.rs"),
            vec!["only_staged.rs"]
        );
        // Staged deletion counts too.
        assert_eq!(tracked_dirty_paths("D  gone.rs"), vec!["gone.rs"]);
        assert!(tracked_dirty_paths("").is_empty());
        // Untracked-only trees are clean per the tracked-worktree contract.
        assert!(tracked_dirty_paths("?? untracked_only").is_empty());
    }

    #[test]
    fn rename_lines_report_the_new_path() {
        assert_eq!(tracked_dirty_paths("R  old.rs -> new.rs"), vec!["new.rs"]);
    }

    #[test]
    fn zero_pass_output_fails_the_guard() {
        // The v1.4 `commit_baseline` failure mode: exit 0, 0 passed.
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

    /// `tail` used to slice on a raw byte index, which panics when the 800-byte
    /// cut lands inside a multi-byte codepoint. Step output routinely contains
    /// non-ASCII, and a panic here crashes the gate (exit 101, no record detail)
    /// instead of reporting a step failure.
    #[test]
    fn tail_does_not_panic_when_the_cut_lands_mid_codepoint() {
        // Place a 3-byte codepoint so that len-800 falls strictly inside it.
        let s = format!("{}中{}", "x".repeat(64), "y".repeat(799));
        assert!(
            !s.is_char_boundary(s.len() - 800),
            "fixture must put the cut inside a codepoint"
        );

        let out = tail(&s);

        assert!(out.starts_with('…'), "long output must be marked elided");
        assert!(out.ends_with('y'));
        // Never returns more than the cap (leading marker excluded).
        assert!(out.len() - '…'.len_utf8() <= 800);
    }

    #[test]
    fn tail_returns_short_output_verbatim_and_trimmed() {
        assert_eq!(tail("  ok — done  "), "ok — done");
    }

    /// Both child streams must share one file offset. Two independent
    /// descriptors on the same file (what `reopen()` twice yields) have separate
    /// offsets and overwrite each other, corrupting the text that
    /// `passed_test_count`, `tail`, and the doctor evidence file are built from.
    #[test]
    fn run_timed_interleaves_stdout_and_stderr_without_overwriting() {
        let mut cmd = Command::new("sh");
        cmd.arg("-c")
            // stderr first, then stdout, then stderr again: with independent
            // offsets the second stderr write clobbers the stdout line.
            .arg(
                "echo compiling >&2; echo 'test result: ok. 7 passed; 0 failed'; echo running >&2",
            );

        let out = run_timed(&mut cmd, Duration::from_secs(30)).expect("run");

        assert!(
            out.success,
            "fixture command must succeed: {:?}",
            out.output
        );
        assert!(
            out.output.contains("compiling") && out.output.contains("running"),
            "stderr lines must survive: {:?}",
            out.output
        );
        assert!(
            out.output.contains("test result: ok. 7 passed"),
            "stdout line must survive: {:?}",
            out.output
        );
        assert_eq!(
            passed_test_count(&out.output),
            7,
            "the gate's pass count must be parseable from the captured text: {:?}",
            out.output
        );
    }
}
