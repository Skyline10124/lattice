// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! microsandbox backend: libkrun microVMs driven by the `microsandbox` Rust SDK 0.5.6.
//!
//! # SDK-only (Plan 33-05)
//!
//! This module drives Upper workspace lifecycle exclusively through the
//! `microsandbox` Rust SDK.  No `msb` CLI subprocess calls exist here
//! (T-33-05: argv/shell injection mitigation).
//!
//! Per-Upper sandbox handles are stored in an internal
//! `Arc<RwLock<HashMap<String, Sandbox>>>` keyed by `UpperHandle.id`.
//! `Sandbox: Send + Sync` was confirmed by Wave 0 assumption A1, so no
//! `Arc<Mutex<Sandbox>>` wrapping is needed.
//!
//! # Egress (EGRESS-01)
//!
//! [`apply_egress`] maps an [`EgressPolicy`] onto a `SandboxBuilder` using
//! `NetworkPolicy::none()` (blocked) or `NetworkPolicy::none().allow_domains(iter)?`
//! (allowlist).  There is no `NetworkPolicy::allowlist()` constructor
//! (Wave 0 A7 verified; see `33-VALIDATION.md`).
//!
//! # Snapshot API (Wave 0 A4, verified 2026-06-13)
//!
//! - SAVE: `Snapshot::builder(&src_sandbox_name).destination(SnapshotDestination::Name(snap)).create().await?`
//! - RESTORE: `Sandbox::builder(&new_name).from_snapshot(&snap_name).cpus(n).memory(mb).create().await?`
//!
//! There is NO `Snapshot::save()` and NO `Snapshot::restore_with()`.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use microsandbox::{NetworkPolicy, Sandbox, Snapshot, SnapshotDestination};

use crate::{
    Backend, CredentialSpec, EgressPolicy, ExecOutput, GitStatus, NetPlan, Result, UpperHandle,
    UpperSpec, Workspace, WorkspaceError, plan_egress, validate_baseline_oid,
};

/// Fixed shell command that installs `git` inside a Debian/Ubuntu-based Upper.
///
/// # Security (T-33-05)
///
/// This is a compile-time constant with **no** interpolated user input.  It is
/// always executed via `exec_with("sh", |e| e.args(["-c", APT_GET_GIT]))` — an
/// argv slice, never `sandbox.shell()` — so there is no shell-injection surface.
/// The literal here is shared by both [`MicrosandboxWorkspace::build_git_ready_template`]
/// (one-time template bake) and the `ImageOrTemplate::Image` + Open/Allowlist
/// fallback in [`MicrosandboxWorkspace::deliver_baseline`].
const APT_GET_GIT: &str =
    "apt-get update -qq && DEBIAN_FRONTEND=noninteractive apt-get install -y -qq git";

/// Wall-clock ceiling for the one-time `apt-get install git` step.
///
/// Applies both to the warm-template bake and the Open/Allowlist `Image`
/// fallback; under `EgressPolicy::Blocked` apt-get is never attempted.
const APT_GET_GIT_TIMEOUT: Duration = Duration::from_secs(300);

/// Apply an [`EgressPolicy`] to a `SandboxBuilder`.
///
/// # API notes (Wave 0 A7, confirmed 2026-06-13)
///
/// - There is **no** `NetworkPolicy::allowlist()` constructor — the real form is
///   `NetworkPolicy::none().allow_domains(iter)` which returns `Result`.
/// - `SandboxBuilder` is at `microsandbox::sandbox::SandboxBuilder`; it is NOT
///   re-exported at the crate root.
/// - Allowlisted domains are resolved DNS-natively by the SDK; no explicit DNS
///   rule is needed.  Rebind-protection state: not tested with private-IP targets
///   (Wave 0 used a public Cloudflare IP — RFC1918 protection unconfirmed).
/// - T-33-03: using `none().allow_domains` as the deny-all baseline prevents
///   allowlist bypass patterns that could arise from a permissive default.
pub(crate) fn apply_egress(
    builder: microsandbox::sandbox::SandboxBuilder,
    policy: &EgressPolicy,
) -> Result<microsandbox::sandbox::SandboxBuilder> {
    Ok(match plan_egress(policy) {
        // SDK default = public internet (private IPs blocked natively).
        NetPlan::Open => builder,
        // deny-all: no network interfaces created.
        NetPlan::Blocked => builder.network(|n| n.policy(NetworkPolicy::none())),
        // deny-all baseline, then allow listed domains only.
        // allow_domains returns Result — map to WorkspaceError::Sdk.
        NetPlan::Allow(domains) => {
            let p = NetworkPolicy::none()
                .allow_domains(&domains)
                .map_err(|e| WorkspaceError::Sdk(format!("invalid allowlist domain: {e}")))?;
            builder.network(move |n| n.policy(p))
        }
    })
}

/// Drives microsandbox through the `microsandbox` Rust SDK 0.5.6.
///
/// Per-Upper `Sandbox` handles live in an internal map keyed by `UpperHandle.id`
/// (a UUID-derived name of the form `upper-<8hex>`).  The live SDK object is never
/// exposed to callers — `UpperHandle` carries only `id + backend` (WS-02, T-33-01).
///
/// # Thread safety
///
/// `Sandbox: Send + Sync` was confirmed live in Wave 0 A1.  The internal map uses
/// `Arc<RwLock<...>>` directly without extra `Mutex` wrapping.
pub struct MicrosandboxWorkspace {
    default_cpus: u32,
    default_memory_mb: u32,
    /// Live sandbox handles: upper-id -> Sandbox.
    /// A1 confirmed Sandbox: Send+Sync so plain Arc<RwLock<HashMap>> is safe.
    sandboxes: Arc<RwLock<HashMap<String, Sandbox>>>,
}

impl MicrosandboxWorkspace {
    /// Create with explicit default resource limits.
    pub fn new(default_cpus: u32, default_memory_mb: u32) -> Self {
        Self {
            default_cpus,
            default_memory_mb,
            sandboxes: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Read defaults from env; falls back to 1 CPU and 512 MiB.
    ///
    /// Reads `UPPER_DEFAULT_CPUS` and `UPPER_DEFAULT_MEMORY_MB`.
    pub fn from_env() -> Self {
        let cpus: u32 = std::env::var("UPPER_DEFAULT_CPUS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(1);
        let mem: u32 = std::env::var("UPPER_DEFAULT_MEMORY_MB")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(512);
        Self::new(cpus, mem)
    }

    /// Convert a microsandbox SDK error into [`WorkspaceError::Sdk`].
    fn sdk_err(e: impl std::fmt::Display) -> WorkspaceError {
        WorkspaceError::Sdk(e.to_string())
    }

    /// Validate a u32 vCPU count fits in the SDK's u8 builder slot.
    fn cpus_u8(cpus: u32) -> Result<u8> {
        u8::try_from(cpus)
            .map_err(|_| WorkspaceError::Sdk(format!("cpus {cpus} exceeds u8 maximum (255)")))
    }

    /// Build a reusable, **git-ready** per-project warm template snapshot.
    ///
    /// Bakes `git` into a snapshot of `base_image` exactly once so that every
    /// subsequent Upper restored from `name` (via `ImageOrTemplate::Template`)
    /// is git-ready without paying a per-Upper `apt-get install git` — including
    /// under [`EgressPolicy::Blocked`](crate::EgressPolicy::Blocked), where
    /// apt-get could not reach the network at all (WS-04 / M-33-05).
    ///
    /// # Flow
    ///
    /// 1. Create a sandbox from `base_image` with the workspace default
    ///    CPU/memory limits (open egress — the SDK default — so apt-get can
    ///    reach the package mirror).
    /// 2. Run [`APT_GET_GIT`] via `exec_with("sh", …)` with a 300 s timeout.
    /// 3. Verify `git --version` exits 0 (fail fast if the install did not take).
    /// 4. `stop()` the sandbox (a snapshot must be taken from a stopped VM).
    /// 5. Save the snapshot with
    ///    `Snapshot::builder(name).destination(SnapshotDestination::Name(name)).create()`
    ///    (Wave 0 A4 authoritative API).
    /// 6. Return the snapshot `name`.
    ///
    /// The created sandbox is **not** stored in the live-handle map: this method
    /// owns its full lifecycle (create → bake → stop → snapshot) and leaves no
    /// live Upper behind.
    ///
    /// # Security (T-33-04)
    ///
    /// This method must **NEVER** call `.env()` with credential values or
    /// [`inject_file_credentials`](Self::inject_file_credentials).  The template
    /// snapshot is a shared rootfs restored for many per-turn Uppers; resolved
    /// secrets must remain per-turn only (applied on `create_upper` /
    /// `restore_upper`).  No `UpperSpec`/`CredentialSpec` is taken here, so there
    /// is structurally nothing to bake in.
    ///
    /// # Security (T-33-05)
    ///
    /// The apt-get command is the fixed [`APT_GET_GIT`] literal executed through
    /// an `exec_with` argv slice — never `sandbox.shell()` and never string
    /// interpolation of caller input.
    pub async fn build_git_ready_template(&self, base_image: &str, name: &str) -> Result<String> {
        // Step 1: boot the bake sandbox from the base image (default open egress
        // so the one-time apt-get can reach the package mirror).
        let sandbox = Sandbox::builder(name)
            .image(base_image)
            .cpus(Self::cpus_u8(self.default_cpus)?)
            .memory(self.default_memory_mb)
            .create()
            .await
            .map_err(Self::sdk_err)?;

        // Step 2: install git via the fixed literal (argv slice, never shell()).
        // T-33-05: APT_GET_GIT is a compile-time constant, no user input.
        if let Err(e) = sandbox
            .exec_with("sh", |e| {
                e.args(["-c", APT_GET_GIT]).timeout(APT_GET_GIT_TIMEOUT)
            })
            .await
        {
            let _ = sandbox.stop().await;
            return Err(Self::sdk_err(e));
        }

        // Step 3: verify git is actually present; fail fast otherwise so a
        // broken template is never snapshotted and handed to deliver_baseline.
        let git_version = sandbox
            .exec("git", ["--version"])
            .await
            .map_err(Self::sdk_err)?;
        if git_version.status().code != 0 {
            // Best-effort stop before surfacing the error.
            let _ = sandbox.stop().await;
            return Err(WorkspaceError::Sdk(format!(
                "build_git_ready_template: git install did not yield a working git \
                 (git --version exit {})",
                git_version.status().code
            )));
        }

        // Step 4: stop before snapshot — a running sandbox snapshot captures
        // dirty state (matches template_create's invariant).
        sandbox.stop().await.map_err(Self::sdk_err)?;

        // Step 5: save the snapshot. A4 authoritative API — Snapshot::builder(src)
        // .destination(Name(snap)).create(); there is NO Snapshot::save().
        // INVARIANT (T-33-04): no .env() / inject_file_credentials on this path.
        Snapshot::builder(name)
            .destination(SnapshotDestination::Name(name.to_string()))
            .create()
            .await
            .map_err(Self::sdk_err)?;

        Ok(name.to_string())
    }

    /// Deliver the baseline git tree into `/workspace` inside a live sandbox.
    ///
    /// Steps:
    /// 1. Export the `baseline_oid` tree to a host tempdir via `git_archive_to_tempdir`.
    /// 2. Create `/workspace` inside the VM.
    /// 3. Deliver the archive content via per-file base64 exec injection (Option B).
    /// 4. `git init`, `git add -A`, `git commit` inside the VM.
    ///
    /// The `host_repo_path` is used only on the host side; it never enters exec argv
    /// or SDK builder chains (WS-02, T-33-01).
    ///
    /// # Note on Option A vs B
    ///
    /// Option A (tar + readonly volume bind) is preferred if the SDK exposes a volume
    /// API that supports readonly bind-mounts of a host tempdir (not a writable Lower
    /// mount).  The SDK 0.5.6 volume builder does not expose a readonly bind for an
    /// arbitrary host path in the feature surface confirmed by Wave 0, so Option B
    /// (per-file base64 exec injection) is used here.  For large repos, a future
    /// optimisation can switch to writing a tar archive to a temp file and exec-ing
    /// `tar -xf` with a read-only bind, once the SDK surface is extended.
    async fn deliver_baseline(&self, sandbox: &Sandbox, spec: &UpperSpec) -> Result<()> {
        use crate::git::git_archive_to_tempdir;

        // Step 1: export baseline_oid tree to a host tempdir (git2-backed, no shell).
        // The TempDir is dropped at the end of this function, deleting the archive.
        let archive_dir = git_archive_to_tempdir(&spec.host_repo_path, &spec.baseline_oid)?;

        // Step 2: create /workspace inside VM.
        sandbox
            .exec("mkdir", ["-p", "/workspace"])
            .await
            .map_err(Self::sdk_err)?;

        // Step 3: deliver files via per-file base64 injection (Option B).
        // Iterate directory entries and inject each file into /workspace/<rel-path>.
        Self::inject_dir_to_vm(
            sandbox,
            archive_dir.path(),
            std::path::Path::new("/workspace"),
        )
        .await?;

        // Step 4: git-init sequence inside Upper (always via exec, never host bind).
        //
        // Probe for git first using `sh -c "command -v git"` (sh is always available).
        // The downstream behaviour when git is MISSING is driven by the rootfs
        // source and egress policy (M-33-05 hardening):
        //
        //   * `ImageOrTemplate::Template` + missing git → hard error.  A git-ready
        //     template is built once by `build_git_ready_template`; a template that
        //     lacks git is a build defect, not a runtime fallback case.
        //   * `ImageOrTemplate::Image` + Open/Allowlist egress → one-time
        //     `apt-get install git` (network reachable), then hard error if it
        //     still is not present.
        //   * `ImageOrTemplate::Image` + Blocked egress → deliver files only and
        //     return Ok (exec-only Upper).  apt-get cannot reach the network under
        //     Blocked; callers that need a git root under Blocked must use a
        //     git-ready template (proven by the integration tests).
        //
        // The old unconditional "swallow apt-get failure and silently skip git
        // init" path is removed: the Template path no longer produces a git-less
        // Upper without surfacing an error.
        let has_git = Self::probe_git(sandbox).await?;

        if !has_git {
            match &spec.image_or_template {
                crate::ImageOrTemplate::Template(_) => {
                    return Err(WorkspaceError::Sdk(
                        "git-ready template is missing git; rebuild with build_git_ready_template"
                            .to_string(),
                    ));
                }
                crate::ImageOrTemplate::Image(_) => match spec.egress {
                    EgressPolicy::Open | EgressPolicy::Allowlist(_) => {
                        // Network reachable — install git once via the fixed literal.
                        // T-33-05: APT_GET_GIT is a compile-time constant, argv slice.
                        let _ = sandbox
                            .exec_with("sh", |e| {
                                e.args(["-c", APT_GET_GIT]).timeout(APT_GET_GIT_TIMEOUT)
                            })
                            .await;
                        if !Self::probe_git(sandbox).await? {
                            return Err(WorkspaceError::Sdk(
                                "failed to install git in Upper; use a git-ready template or an \
                                 image with git pre-installed"
                                    .to_string(),
                            ));
                        }
                        // git now present — fall through to init/add/commit.
                    }
                    EgressPolicy::Blocked => {
                        // Blocked egress: apt-get cannot reach the mirror.  Deliver
                        // files only; the Upper is exec-capable but has no .git root.
                        // Callers needing git under Blocked must use a git-ready
                        // template (the Template branch above guarantees git there).
                        return Ok(());
                    }
                },
            }
        }

        // git is available — run init/add/commit inside Upper.
        sandbox
            .exec("git", ["-C", "/workspace", "init"])
            .await
            .map_err(Self::sdk_err)?;
        sandbox
            .exec("git", ["-C", "/workspace", "add", "-A"])
            .await
            .map_err(Self::sdk_err)?;
        sandbox
            .exec(
                "git",
                [
                    "-C",
                    "/workspace",
                    "-c",
                    "user.email=lattice@local",
                    "-c",
                    "user.name=lattice",
                    "commit",
                    "--allow-empty-message",
                    "-m",
                    &format!("baseline:{}", spec.baseline_oid),
                ],
            )
            .await
            .map_err(Self::sdk_err)?;
        Ok(())
    }

    /// Probe whether `git` is on `PATH` inside the sandbox.
    ///
    /// Returns `Ok(true)` when `command -v git` resolves.  `sh` is always
    /// available, so the probe itself never depends on git being present.
    /// Used by [`deliver_baseline`](Self::deliver_baseline) before deciding
    /// whether to run the git-init sequence, install git, or error.
    async fn probe_git(sandbox: &Sandbox) -> Result<bool> {
        let check = sandbox
            .exec(
                "sh",
                [
                    "-c",
                    "command -v git > /dev/null 2>&1 && echo ok || echo missing",
                ],
            )
            .await
            .map_err(Self::sdk_err)?;
        let stdout = check.stdout().unwrap_or_else(|_| "missing".to_string());
        Ok(stdout.trim().starts_with("ok"))
    }

    /// Recursively inject all files from `host_dir` into `guest_base` via base64.
    ///
    /// Uses `exec_with sh -c "printf '%s' '<b64>' | base64 -d > <path>"` for each file.
    /// Directories are created with `mkdir -p` before files inside them are injected.
    async fn inject_dir_to_vm(
        sandbox: &Sandbox,
        host_dir: &std::path::Path,
        guest_base: &std::path::Path,
    ) -> Result<()> {
        let entries = std::fs::read_dir(host_dir).map_err(WorkspaceError::Io)?;
        for entry in entries {
            let entry = entry.map_err(WorkspaceError::Io)?;
            let host_path = entry.path();
            let file_name = entry.file_name();
            let guest_path = guest_base.join(&file_name);
            let guest_path_str = guest_path.to_string_lossy().to_string();

            if host_path.is_dir() {
                sandbox
                    .exec("mkdir", ["-p", &guest_path_str])
                    .await
                    .map_err(Self::sdk_err)?;
                // Recurse into subdirectory.
                Box::pin(Self::inject_dir_to_vm(sandbox, &host_path, &guest_path)).await?;
            } else if host_path.is_file() {
                let content = std::fs::read(&host_path).map_err(WorkspaceError::Io)?;
                let b64 = STANDARD.encode(&content);
                // Ensure parent directory exists.
                if let Some(parent) = guest_path.parent() {
                    let parent_str = parent.to_string_lossy().to_string();
                    if !parent_str.is_empty() && parent_str != "/" {
                        sandbox
                            .exec("mkdir", ["-p", &parent_str])
                            .await
                            .map_err(Self::sdk_err)?;
                    }
                }
                // Decode base64 into the guest path.
                // NEVER use sandbox.shell() — use exec_with argv slices (T-33-05).
                // Pass payload and destination as positional args so guest paths
                // are never interpolated into the shell command string.
                sandbox
                    .exec_with("sh", |e| {
                        e.args([
                            "-c",
                            "printf '%s' \"$1\" | base64 -d > \"$2\"",
                            "sh",
                            &b64,
                            &guest_path_str,
                        ])
                    })
                    .await
                    .map_err(Self::sdk_err)?;
            }
        }
        Ok(())
    }

    /// Inject `CredentialSpec::File` entries into the sandbox post-boot.
    ///
    /// # Security (CRED-01, T-33-02, T-33-04)
    ///
    /// - Never use `sandbox.shell()` — exec via argv slices only (T-33-05).
    /// - Never log credential values — base64 encoding obfuscates content in argv
    ///   but the real protection is in `CredentialSpec`'s redacting `Debug` impl.
    /// - This function must NEVER be called from `template_create` or the template-
    ///   build path (T-33-04): credentials must not be baked into template snapshots.
    async fn inject_file_credentials(
        sandbox: &Sandbox,
        credentials: &[CredentialSpec],
    ) -> Result<()> {
        for cred in credentials {
            if let CredentialSpec::File {
                content,
                guest_path,
            } = cred
            {
                let b64 = STANDARD.encode(content);
                // Create parent directory.
                let parent = std::path::Path::new(guest_path)
                    .parent()
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_else(|| "/".into());
                sandbox
                    .exec("mkdir", ["-p", &parent])
                    .await
                    .map_err(Self::sdk_err)?;
                // Decode base64 into guest_path via argv slice (T-33-05).
                // Pass payload and destination as positional args so guest paths
                // are never interpolated into the shell command string.
                sandbox
                    .exec_with("sh", |e| {
                        e.args([
                            "-c",
                            "printf '%s' \"$1\" | base64 -d > \"$2\"",
                            "sh",
                            &b64,
                            guest_path,
                        ])
                    })
                    .await
                    .map_err(Self::sdk_err)?;
            }
        }
        Ok(())
    }
}

#[async_trait]
impl Workspace for MicrosandboxWorkspace {
    // ── Upper methods ─────────────────────────────────────────────────────────

    /// Provision an Upper workspace.
    ///
    /// # Flow
    ///
    /// 1. Validate `spec.baseline_oid` (T-33-05).
    /// 2. Generate a unique name `upper-<uuid8>`.
    /// 3. Build a `SandboxBuilder` from either `.image(img)` or
    ///    `.from_snapshot(tmpl_name)` — both branches use the same builder
    ///    for resource, egress, and env-var credential overrides (T-33-04:
    ///    env creds on both branches; file creds post-boot only).
    /// 4. `apply_egress(builder, &spec.egress)?` (returns `Result`).
    /// 5. `.env(k, v)` for each `CredentialSpec::EnvVar`.
    /// 6. `.create().await?` → `Sandbox`.
    /// 7. `deliver_baseline` (exec-based; no host bind-mount of workspace).
    /// 8. `inject_file_credentials` post-boot.
    /// 9. Store in internal map and return `UpperHandle`.
    async fn create_upper(&self, spec: &UpperSpec) -> Result<UpperHandle> {
        // Security: validate OID format before any use (T-33-05).
        validate_baseline_oid(&spec.baseline_oid)?;

        let name = format!("upper-{}", &uuid::Uuid::new_v4().simple().to_string()[..8]);

        // Both branches build a SandboxBuilder and differ only in the rootfs source.
        // Restore is via Sandbox::builder().from_snapshot() — NOT Snapshot::get().restore_with().
        let mut builder = match &spec.image_or_template {
            crate::ImageOrTemplate::Image(img) => Sandbox::builder(&name).image(img.as_str()),
            crate::ImageOrTemplate::Template(tmpl_name) => {
                Sandbox::builder(&name).from_snapshot(tmpl_name.as_str())
            }
        }
        .cpus(Self::cpus_u8(spec.cpus)?)
        .memory(spec.memory_mb)
        .max_duration(spec.timeout_secs);

        builder = apply_egress(builder, &spec.egress)?;

        // Per-turn env creds on the builder for BOTH branches.
        // NEVER bake env creds into a template snapshot (T-33-04).
        for cred in &spec.credentials {
            if let CredentialSpec::EnvVar { name: k, value: v } = cred {
                builder = builder.env(k, v);
            }
        }

        let sandbox = builder.create().await.map_err(Self::sdk_err)?;

        // Deliver baseline tree inside the VM via exec (no host bind-mount — T-33-01).
        self.deliver_baseline(&sandbox, spec).await?;

        // File credentials injected post-boot (not in template — T-33-04).
        Self::inject_file_credentials(&sandbox, &spec.credentials).await?;

        self.sandboxes
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(name.clone(), sandbox);
        Ok(UpperHandle {
            id: name,
            backend: Backend::Microsandbox,
        })
    }

    /// Run `prog args` inside the Upper.
    ///
    /// ALWAYS uses `sandbox.exec(prog, args_slice)` — never `sandbox.shell()`
    /// to prevent shell injection (T-33-05).
    async fn exec_upper(
        &self,
        handle: &UpperHandle,
        prog: &str,
        args: &[&str],
    ) -> Result<ExecOutput> {
        // Clone the Sandbox handle so the RwLockReadGuard is released before .await.
        let sandbox = {
            let sandboxes = self.sandboxes.read().unwrap_or_else(|e| e.into_inner());
            sandboxes
                .get(&handle.id)
                .ok_or_else(|| {
                    WorkspaceError::Sdk(format!("no live sandbox for id={}", handle.id))
                })?
                .clone()
        };

        // ALWAYS exec(prog, args) — NEVER sandbox.shell(cmd_str) (T-33-05).
        // Run in /workspace so relative paths (e.g. `cat README.md`) resolve
        // against the Upper root, matching the Workspace contract that
        // DeterministicWorkspace satisfies via Command::current_dir
        // (V-04 test_baseline_content / test_upper_git_root). /workspace is
        // always created + populated by create_upper before any exec_upper.
        let out = sandbox
            .exec_with(prog, |e| e.args(args.iter().copied()).cwd("/workspace"))
            .await
            .map_err(Self::sdk_err)?;
        Ok(ExecOutput {
            exit_code: out.status().code,
            stdout: out
                .stdout()
                .map_err(|e| WorkspaceError::Sdk(format!("stdout decode: {e}")))?,
            stderr: out
                .stderr()
                .map_err(|e| WorkspaceError::Sdk(format!("stderr decode: {e}")))?,
        })
    }

    /// Run `git status --porcelain=v1` inside the Upper and return parsed output.
    async fn git_status(&self, handle: &UpperHandle) -> Result<GitStatus> {
        let out = self
            .exec_upper(
                handle,
                "git",
                &["-C", "/workspace", "status", "--porcelain=v1"],
            )
            .await?;
        Ok(GitStatus {
            porcelain: out.stdout.clone(),
            is_clean: out.stdout.trim().is_empty(),
        })
    }

    /// Stop the Upper and save it as a warm template snapshot.
    ///
    /// # Security (T-33-04)
    ///
    /// This path intentionally does NOT call `inject_file_credentials` or
    /// `.env()` with credential values.  The snapshot is a shared template
    /// that may be restored for multiple per-turn Uppers; credentials must be
    /// injected only on the per-turn `create_upper` / `restore_upper` path.
    ///
    /// # API (Wave 0 A4, confirmed 2026-06-13)
    ///
    /// Uses `Snapshot::builder(&source.id).destination(SnapshotDestination::Name(name)).create()`.
    /// There is NO `Snapshot::save()`.
    async fn template_create(&self, source: &UpperHandle, name: &str) -> Result<String> {
        // Remove from live map first — sandbox must be stopped before snapshotting.
        let sandbox = self
            .sandboxes
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&source.id)
            .ok_or_else(|| WorkspaceError::Sdk("handle not found for template_create".into()))?;

        // MUST stop before snapshot — a running sandbox snapshot captures dirty state.
        sandbox.stop().await.map_err(Self::sdk_err)?;

        // INVARIANT (T-33-04): this path NEVER injects credentials.
        // Snapshot::builder(src_name).destination(Name(snap)).create() — A4 authoritative API.
        Snapshot::builder(&source.id)
            .destination(SnapshotDestination::Name(name.to_string()))
            .create()
            .await
            .map_err(Self::sdk_err)?;

        Ok(name.to_string())
    }

    /// Boot a fresh CoW Upper from an existing template snapshot.
    ///
    /// # Security (T-33-04)
    ///
    /// Per-turn credentials (env vars via `.env()`, files via
    /// `inject_file_credentials`) are applied here, NOT in the template snapshot.
    ///
    /// # API (Wave 0 A4, confirmed 2026-06-13)
    ///
    /// `Sandbox::builder(new).from_snapshot(template).cpus().memory().env().create()`.
    /// There is NO `Snapshot::get().restore_with()`.
    async fn restore_upper(
        &self,
        template_name: &str,
        new_name: &str,
        spec: &UpperSpec,
    ) -> Result<UpperHandle> {
        let mut builder = Sandbox::builder(new_name)
            .from_snapshot(template_name)
            .cpus(Self::cpus_u8(spec.cpus)?)
            .memory(spec.memory_mb)
            .max_duration(spec.timeout_secs);

        builder = apply_egress(builder, &spec.egress)?;

        // Per-turn env creds ONLY here — never baked into the template snapshot (T-33-04).
        for cred in &spec.credentials {
            if let CredentialSpec::EnvVar { name: k, value: v } = cred {
                builder = builder.env(k, v);
            }
        }

        let sandbox = builder.create().await.map_err(Self::sdk_err)?;

        // File creds injected post-boot (not in template — T-33-04).
        Self::inject_file_credentials(&sandbox, &spec.credentials).await?;

        self.sandboxes
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(new_name.to_string(), sandbox);

        Ok(UpperHandle {
            id: new_name.to_string(),
            backend: Backend::Microsandbox,
        })
    }

    /// Destroy an Upper. Idempotent.
    async fn destroy_upper(&self, handle: &UpperHandle) -> Result<()> {
        // Remove from map first so the write guard is released before .await.
        let maybe_sandbox = self
            .sandboxes
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&handle.id);
        if let Some(sandbox) = maybe_sandbox {
            let _ = sandbox.stop().await; // best-effort, ignore error
        }
        Ok(())
    }
}
