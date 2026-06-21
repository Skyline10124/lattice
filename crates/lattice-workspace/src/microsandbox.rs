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
    Backend, CredentialSpec, DirectoryEntry, EgressPolicy, ExecOutput, ExecRequest, FindRequest,
    GitStatus, GrepMatch, GrepRequest, MAX_WORKSPACE_WRITE_BYTES, NetPlan, PathFacts, PathIntent,
    PathKind, Result, UpperHandle, UpperSpec, Workspace, WorkspaceError, WorkspacePath,
    CandidateExport, plan_egress, validate_baseline_oid,
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

    /// Deliver the exact host baseline commit into `/workspace/.git` inside a live sandbox.
    ///
    /// Builds a host pack via [`crate::git::build_baseline_pack`], uploads it in bounded
    /// binary-safe chunks, ingests with `git index-pack`, attaches `HEAD` to
    /// [`crate::git::LATTICE_UPPER_REF`], and verifies the exact baseline OID.
    async fn deliver_baseline(&self, sandbox: &Sandbox, spec: &UpperSpec) -> Result<()> {
        sandbox
            .exec("mkdir", ["-p", "/workspace"])
            .await
            .map_err(Self::sdk_err)?;

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
                    }
                    EgressPolicy::Blocked => {
                        return Ok(());
                    }
                },
            }
        }

        let pack = crate::git::build_baseline_pack(&spec.host_repo_path, &spec.baseline_oid)?;

        Self::exec_git_workspace(sandbox, &["init"], "git init").await?;

        let guest_pack = Self::upload_baseline_pack(sandbox, pack.path()).await?;

        Self::exec_git_workspace(sandbox, &["index-pack", &guest_pack], "index-pack").await?;
        Self::exec_git_workspace(
            sandbox,
            &[
                "update-ref",
                crate::git::LATTICE_UPPER_REF,
                &spec.baseline_oid,
            ],
            "update-ref lattice-upper",
        )
        .await?;
        Self::exec_git_workspace(
            sandbox,
            &["symbolic-ref", "HEAD", crate::git::LATTICE_UPPER_REF],
            "symbolic-ref HEAD",
        )
        .await?;
        Self::exec_git_workspace(
            sandbox,
            &["checkout", "-f", &spec.baseline_oid],
            "checkout baseline",
        )
        .await?;

        let actual = Self::exec_git_workspace_stdout(sandbox, &["rev-parse", "HEAD"], "rev-parse HEAD")
            .await?
            .trim()
            .to_string();
        if actual != spec.baseline_oid {
            return Err(WorkspaceError::BaselineIdentityMismatch {
                expected: spec.baseline_oid.clone(),
                actual,
            });
        }

        Ok(())
    }

    /// Upload a host pack file to a guest tempfile in bounded base64 chunks.
    async fn upload_baseline_pack(
        sandbox: &Sandbox,
        pack_path: &std::path::Path,
    ) -> Result<String> {
        use std::io::Read;

        const GUEST_PACK_PATH: &str = "/tmp/lattice-baseline.pack";

        sandbox
            .exec_with("sh", |e| {
                e.args(["-c", ": > \"$1\"", "sh", GUEST_PACK_PATH])
            })
            .await
            .map_err(Self::sdk_err)?;

        let mut file = std::fs::File::open(pack_path).map_err(WorkspaceError::Io)?;
        let mut buf = vec![0u8; crate::git::BASELINE_TRANSFER_CHUNK_BYTES];
        loop {
            let n = file.read(&mut buf).map_err(WorkspaceError::Io)?;
            if n == 0 {
                break;
            }
            let b64 = STANDARD.encode(&buf[..n]);
            sandbox
                .exec_with("sh", |e| {
                    e.args([
                        "-c",
                        "printf '%s' \"$1\" | base64 -d >> \"$2\"",
                        "sh",
                        &b64,
                        GUEST_PACK_PATH,
                    ])
                })
                .await
                .map_err(Self::sdk_err)?;
        }

        Ok(GUEST_PACK_PATH.to_string())
    }

    /// Download a guest bundle file into a synced host tempfile using bounded base64 chunks.
    async fn download_guest_bundle(
        sandbox: &Sandbox,
        guest_path: &str,
        max_artifact_bytes: u64,
    ) -> Result<(tempfile::NamedTempFile, u64)> {
        use std::io::Write;

        let size_out = sandbox
            .exec_with("stat", |e| e.args(["-c", "%s", guest_path]))
            .await
            .map_err(Self::sdk_err)?;
        if size_out.status().code != 0 {
            let stderr = size_out
                .stderr()
                .map_err(|e| WorkspaceError::Sdk(format!("stat stderr: {e}")))?;
            return Err(WorkspaceError::Git(format!("stat bundle failed: {stderr}")));
        }
        let artifact_len: u64 = size_out
            .stdout()
            .map_err(|e| WorkspaceError::Sdk(format!("stat stdout: {e}")))?
            .trim()
            .parse()
            .map_err(|_| WorkspaceError::Git("invalid bundle size from stat".into()))?;
        if artifact_len > max_artifact_bytes {
            return Err(WorkspaceError::ArtifactTooLarge {
                actual: artifact_len,
                max: max_artifact_bytes,
            });
        }

        let mut bundle_file = tempfile::NamedTempFile::new().map_err(WorkspaceError::Io)?;
        let chunk = crate::git::CANDIDATE_EXPORT_CHUNK_BYTES;
        let mut offset = 0u64;
        while offset < artifact_len {
            let skip = offset / chunk as u64;
            let chunk_arg = chunk.to_string();
            let skip_arg = skip.to_string();
            let out = sandbox
                .exec_with("sh", |e| {
                    e.args([
                        "-c",
                        "dd if=\"$1\" bs=\"$2\" skip=\"$3\" count=1 2>/dev/null | base64",
                        "sh",
                        guest_path,
                        &chunk_arg,
                        &skip_arg,
                    ])
                })
                .await
                .map_err(Self::sdk_err)?;
            if out.status().code != 0 {
                let stderr = out
                    .stderr()
                    .map_err(|e| WorkspaceError::Sdk(format!("dd stderr: {e}")))?;
                return Err(WorkspaceError::Git(format!("bundle chunk read failed: {stderr}")));
            }
            let b64 = out
                .stdout()
                .map_err(|e| WorkspaceError::Sdk(format!("dd stdout: {e}")))?
                .trim()
                .to_string();
            if b64.is_empty() {
                break;
            }
            let bytes = STANDARD
                .decode(b64.as_bytes())
                .map_err(|e| WorkspaceError::Git(format!("base64 decode failed: {e}")))?;
            bundle_file
                .write_all(&bytes)
                .map_err(WorkspaceError::Io)?;
            offset += bytes.len() as u64;
        }

        bundle_file.flush().map_err(WorkspaceError::Io)?;
        bundle_file.as_file().sync_all().map_err(WorkspaceError::Io)?;
        Ok((bundle_file, artifact_len))
    }

    async fn exec_git_workspace(sandbox: &Sandbox, args: &[&str], ctx: &str) -> Result<()> {
        let _ = Self::exec_git_workspace_stdout(sandbox, args, ctx).await?;
        Ok(())
    }

    async fn exec_git_workspace_stdout(
        sandbox: &Sandbox,
        args: &[&str],
        ctx: &str,
    ) -> Result<String> {
        let out = sandbox
            .exec_with("git", |e| {
                let mut argv = vec!["-C", "/workspace"];
                argv.extend_from_slice(args);
                e.args(argv)
            })
            .await
            .map_err(Self::sdk_err)?;
        if out.status().code != 0 {
            let stderr = out
                .stderr()
                .map_err(|e| WorkspaceError::Sdk(format!("stderr decode: {e}")))?;
            return Err(WorkspaceError::Git(format!(
                "{ctx} failed (exit {}): {stderr}",
                out.status().code
            )));
        }
        out.stdout()
            .map_err(|e| WorkspaceError::Sdk(format!("stdout decode: {e}")))
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

    fn guest_relative(path: &WorkspacePath, allow_root: bool) -> Result<&str> {
        if path.is_project_root() {
            if allow_root {
                Ok(".")
            } else {
                Err(WorkspaceError::InvalidPath(
                    "project root sentinel is not valid for file intents".into(),
                ))
            }
        } else {
            Ok(path.as_str())
        }
    }

    async fn clone_sandbox(&self, handle: &UpperHandle) -> Result<Sandbox> {
        let sandboxes = self.sandboxes.read().unwrap_or_else(|e| e.into_inner());
        sandboxes
            .get(&handle.id)
            .cloned()
            .ok_or_else(|| WorkspaceError::Sdk(format!("no live sandbox for id={}", handle.id)))
    }

    async fn exec_in_workspace(
        &self,
        sandbox: &Sandbox,
        prog: &str,
        args: &[&str],
    ) -> Result<ExecOutput> {
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

    async fn resolve_path_guest(
        &self,
        sandbox: &Sandbox,
        path: &WorkspacePath,
        intent: PathIntent,
    ) -> Result<PathFacts> {
        let rel = Self::guest_relative(
            path,
            matches!(intent, PathIntent::ListDirectory | PathIntent::ExecCwd),
        )?;
        let out = self
            .exec_in_workspace(
                sandbox,
                "sh",
                &[
                    "-c",
                    r#"p="$1"; if [ "$p" = "." ]; then target="/workspace"; else target="/workspace/$p"; fi; if [ ! -e "$target" ]; then echo missing; exit 0; fi; if [ -L "$target" ]; then echo symlink; elif [ -d "$target" ]; then echo directory; elif [ -f "$target" ]; then echo file; else echo other; fi"#,
                    "sh",
                    rel,
                ],
            )
            .await?;
        let line = out.stdout.trim();
        let (exists, kind) = match line {
            "missing" => (false, None),
            "file" => (true, Some(PathKind::File)),
            "directory" => (true, Some(PathKind::Directory)),
            "symlink" => (true, Some(PathKind::Symlink)),
            _ => (true, Some(PathKind::File)),
        };
        let byte_size = if kind == Some(PathKind::File) {
            let stat = self
                .exec_in_workspace(
                    sandbox,
                    "sh",
                    &[
                        "-c",
                        r#"p="$1"; if [ "$p" = "." ]; then target="/workspace"; else target="/workspace/$p"; fi; wc -c < "$target""#,
                        "sh",
                        rel,
                    ],
                )
                .await?;
            stat.stdout.trim().parse::<u64>().ok()
        } else {
            None
        };
        let parent_rel = if path.is_project_root() {
            ".".to_string()
        } else {
            crate::paths::parent_relative(path.as_str())
        };
        let parent_path = WorkspacePath::parse(&parent_rel)
            .map_err(|e| WorkspaceError::InvalidPath(format!("invalid parent path: {e}")))?;
        let parent_facts = if parent_path.is_project_root() && path.is_project_root() {
            PathFacts {
                normalized_path: ".".to_string(),
                exists: true,
                kind: kind.or(Some(PathKind::Directory)),
                parent_exists: true,
                parent_kind: Some(PathKind::Directory),
                byte_size: None,
                canonical_under_workspace: true,
            }
        } else {
            Box::pin(self.resolve_path_guest(sandbox, &parent_path, PathIntent::ListDirectory))
                .await?
        };
        Ok(PathFacts {
            normalized_path: path.as_str().to_string(),
            exists,
            kind,
            parent_exists: parent_facts.exists,
            parent_kind: parent_facts.kind,
            byte_size,
            canonical_under_workspace: exists || path.is_project_root(),
        })
    }

    async fn read_file_guest(&self, sandbox: &Sandbox, path: &WorkspacePath) -> Result<Vec<u8>> {
        let rel = Self::guest_relative(path, false)?;
        let out = self.exec_in_workspace(sandbox, "cat", &[rel]).await?;
        if out.exit_code != 0 {
            return Err(WorkspaceError::NotFound {
                path: path.as_str().to_string(),
            });
        }
        Ok(out.stdout.into_bytes())
    }

    async fn write_file_guest(
        &self,
        sandbox: &Sandbox,
        path: &WorkspacePath,
        content: &[u8],
    ) -> Result<()> {
        let rel = Self::guest_relative(path, false)?;
        if content.len() > MAX_WORKSPACE_WRITE_BYTES {
            return Err(WorkspaceError::ContentTooLarge {
                actual: content.len(),
                max: MAX_WORKSPACE_WRITE_BYTES,
            });
        }
        let b64 = STANDARD.encode(content);
        if let Some(parent) = std::path::Path::new(rel).parent().and_then(|p| p.to_str())
            && !parent.is_empty()
        {
            sandbox
                .exec_with("mkdir", |e| e.args(["-p", parent]).cwd("/workspace"))
                .await
                .map_err(Self::sdk_err)?;
        }
        sandbox
            .exec_with("sh", |e| {
                e.args([
                    "-c",
                    "printf '%s' \"$1\" | base64 -d > \"$2\"",
                    "sh",
                    &b64,
                    rel,
                ])
                .cwd("/workspace")
            })
            .await
            .map_err(Self::sdk_err)?;
        Ok(())
    }

    async fn remove_file_guest(&self, sandbox: &Sandbox, path: &WorkspacePath) -> Result<()> {
        let rel = Self::guest_relative(path, false)?;
        let out = self.exec_in_workspace(sandbox, "rm", &["-f", rel]).await?;
        if out.exit_code != 0 {
            return Err(WorkspaceError::NotFound {
                path: path.as_str().to_string(),
            });
        }
        Ok(())
    }

    async fn list_dir_guest(
        &self,
        sandbox: &Sandbox,
        path: &WorkspacePath,
    ) -> Result<Vec<DirectoryEntry>> {
        let rel = Self::guest_relative(path, true)?;
        let out = self
            .exec_in_workspace(
                sandbox,
                "sh",
                &[
                    "-c",
                    r#"p="$1"; if [ "$p" = "." ]; then target="/workspace"; else target="/workspace/$p"; fi; for entry in "$target"/* "$target"/.[!.]*; do [ -e "$entry" ] || continue; name="${entry##*/}"; if [ -L "$entry" ]; then echo "symlink $name"; elif [ -d "$entry" ]; then echo "directory $name"; elif [ -f "$entry" ]; then echo "file $name"; fi; done"#,
                    "sh",
                    rel,
                ],
            )
            .await?;
        if out.exit_code != 0 {
            return Err(WorkspaceError::WrongKind {
                path: path.as_str().to_string(),
                expected: "directory".into(),
            });
        }
        let mut entries = Vec::new();
        for line in out.stdout.lines() {
            let mut parts = line.splitn(2, ' ');
            let kind = match parts.next() {
                Some("file") => PathKind::File,
                Some("directory") => PathKind::Directory,
                Some("symlink") => PathKind::Symlink,
                _ => continue,
            };
            if let Some(name) = parts.next() {
                entries.push(DirectoryEntry {
                    name: name.to_string(),
                    kind,
                });
            }
        }
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(entries)
    }

    async fn grep_guest(&self, sandbox: &Sandbox, request: &GrepRequest) -> Result<Vec<GrepMatch>> {
        let rel = Self::guest_relative(&request.path, true)?;
        let out = self
            .exec_in_workspace(
                sandbox,
                "sh",
                &[
                    "-c",
                    r#"pat="$1"; root="$2"; max="$3"; grep -R -n -E "$pat" "$root" 2>/dev/null | head -n "$max""#,
                    "sh",
                    &request.pattern,
                    rel,
                    &request.max_results.to_string(),
                ],
            )
            .await?;
        let mut matches = Vec::new();
        for line in out.stdout.lines() {
            if matches.len() >= request.max_results {
                break;
            }
            let Some((path_part, rest)) = line.split_once(':') else {
                continue;
            };
            let Some((line_no, content)) = rest.split_once(':') else {
                continue;
            };
            let normalized = path_part.strip_prefix("./").unwrap_or(path_part);
            let normalized = normalized.strip_prefix("/workspace/").unwrap_or(normalized);
            let wp = if normalized == "/workspace" || normalized.is_empty() {
                WorkspacePath::project_root()
            } else {
                WorkspacePath::parse(normalized)
                    .map_err(|e| WorkspaceError::InvalidPath(format!("grep result path: {e}")))?
            };
            matches.push(GrepMatch {
                path: wp,
                line_number: line_no.parse::<u64>().unwrap_or(0),
                line: content.to_string(),
            });
        }
        Ok(matches)
    }

    async fn find_files_guest(
        &self,
        sandbox: &Sandbox,
        request: &FindRequest,
    ) -> Result<Vec<WorkspacePath>> {
        let rel = Self::guest_relative(&request.path, true)?;
        let out = self
            .exec_in_workspace(
                sandbox,
                "sh",
                &[
                    "-c",
                    r#"root="$1"; pat="$2"; max="$3"; find "$root" -name "$pat" 2>/dev/null | head -n "$max""#,
                    "sh",
                    rel,
                    &request.pattern,
                    &request.max_results.to_string(),
                ],
            )
            .await?;
        let mut found = Vec::new();
        for line in out.stdout.lines() {
            if found.len() >= request.max_results {
                break;
            }
            let normalized = line.strip_prefix("./").unwrap_or(line);
            let normalized = normalized.strip_prefix("/workspace/").unwrap_or(normalized);
            if normalized.is_empty() || normalized == "/workspace" {
                found.push(WorkspacePath::project_root());
            } else {
                found.push(
                    WorkspacePath::parse(normalized).map_err(|e| {
                        WorkspaceError::InvalidPath(format!("find result path: {e}"))
                    })?,
                );
            }
        }
        Ok(found)
    }

    async fn exec_guest(&self, sandbox: &Sandbox, request: &ExecRequest) -> Result<ExecOutput> {
        let cwd = if request.cwd.is_project_root() {
            "/workspace".to_string()
        } else {
            format!("/workspace/{}", request.cwd.as_str())
        };
        let args: Vec<&str> = request.argv.iter().map(String::as_str).collect();
        let prog = args[0];
        let prog_args = &args[1..];
        let out = sandbox
            .exec_with(prog, |e| {
                let mut builder = e.args(prog_args.iter().copied()).cwd(&cwd);
                for (k, v) in &request.env {
                    builder = builder.env(k, v);
                }
                builder
            })
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

    async fn export_candidate(
        &self,
        handle: &UpperHandle,
        baseline_oid: &str,
        max_artifact_bytes: u64,
    ) -> Result<CandidateExport> {
        validate_baseline_oid(baseline_oid)?;

        let status = self
            .exec_upper(
                handle,
                "git",
                &[
                    "-C",
                    "/workspace",
                    "status",
                    "--porcelain=v1",
                    "-z",
                    "--untracked-files=all",
                    "--ignore-submodules=none",
                ],
            )
            .await?;
        if !status.stdout.is_empty() {
            return Err(WorkspaceError::UpperDirty {
                porcelain: status.stdout,
            });
        }

        let candidate_oid = Self::exec_git_workspace_stdout(
            &self.clone_sandbox(handle).await?,
            &["rev-parse", "HEAD"],
            "rev-parse HEAD",
        )
        .await?
        .trim()
        .to_string();
        validate_baseline_oid(&candidate_oid)?;

        let sandbox = self.clone_sandbox(handle).await?;
        let is_ancestor = sandbox
            .exec_with("git", |e| {
                e.args([
                    "-C",
                    "/workspace",
                    "merge-base",
                    "--is-ancestor",
                    baseline_oid,
                    &candidate_oid,
                ])
            })
            .await
            .map_err(Self::sdk_err)?;
        match is_ancestor.status().code {
            0 => {}
            1 => {
                return Err(WorkspaceError::NotDescendant {
                    baseline: baseline_oid.to_owned(),
                    candidate: candidate_oid.clone(),
                });
            }
            _ => {
                let stderr = is_ancestor
                    .stderr()
                    .map_err(|e| WorkspaceError::Sdk(format!("merge-base stderr: {e}")))?;
                return Err(WorkspaceError::Git(format!(
                    "merge-base --is-ancestor failed: {stderr}"
                )));
            }
        }

        const GUEST_BUNDLE: &str = "/tmp/lattice-candidate.bundle";
        let export_ref = format!(
            "refs/lattice/export/candidate-{}",
            uuid::Uuid::new_v4().simple()
        );

        Self::exec_git_workspace(
            &sandbox,
            &["update-ref", &export_ref, &candidate_oid],
            "update-ref export ref",
        )
        .await?;

        let range = format!("{baseline_oid}..{export_ref}");
        Self::exec_git_workspace(
            &sandbox,
            &["bundle", "create", GUEST_BUNDLE, &range],
            "bundle create",
        )
        .await?;

        let export = match Self::download_guest_bundle(&sandbox, GUEST_BUNDLE, max_artifact_bytes).await
        {
            Ok(export) => export,
            Err(err) => {
                let _ = sandbox.exec("rm", ["-f", GUEST_BUNDLE]).await;
                let _ = Self::exec_git_workspace(
                    &sandbox,
                    &["update-ref", "-d", &export_ref],
                    "delete export ref",
                )
                .await;
                return Err(err);
            }
        };

        let _ = sandbox.exec("rm", ["-f", GUEST_BUNDLE]).await;
        let _ = Self::exec_git_workspace(
            &sandbox,
            &["update-ref", "-d", &export_ref],
            "delete export ref",
        )
        .await;

        Ok(crate::CandidateExport::from_parts(
            export.0,
            baseline_oid.to_owned(),
            candidate_oid,
            export.1,
        ))
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

    async fn resolve_path(
        &self,
        handle: &UpperHandle,
        path: &WorkspacePath,
        intent: PathIntent,
    ) -> Result<PathFacts> {
        let sandbox = self.clone_sandbox(handle).await?;
        self.resolve_path_guest(&sandbox, path, intent).await
    }

    async fn read_file(&self, handle: &UpperHandle, path: &WorkspacePath) -> Result<Vec<u8>> {
        let sandbox = self.clone_sandbox(handle).await?;
        self.read_file_guest(&sandbox, path).await
    }

    async fn write_file(
        &self,
        handle: &UpperHandle,
        path: &WorkspacePath,
        content: &[u8],
    ) -> Result<()> {
        if content.len() > MAX_WORKSPACE_WRITE_BYTES {
            return Err(WorkspaceError::ContentTooLarge {
                actual: content.len(),
                max: MAX_WORKSPACE_WRITE_BYTES,
            });
        }
        let sandbox = self.clone_sandbox(handle).await?;
        self.write_file_guest(&sandbox, path, content).await
    }

    async fn remove_file(&self, handle: &UpperHandle, path: &WorkspacePath) -> Result<()> {
        let sandbox = self.clone_sandbox(handle).await?;
        self.remove_file_guest(&sandbox, path).await
    }

    async fn list_dir(
        &self,
        handle: &UpperHandle,
        path: &WorkspacePath,
    ) -> Result<Vec<DirectoryEntry>> {
        let sandbox = self.clone_sandbox(handle).await?;
        self.list_dir_guest(&sandbox, path).await
    }

    async fn grep(&self, handle: &UpperHandle, request: &GrepRequest) -> Result<Vec<GrepMatch>> {
        let sandbox = self.clone_sandbox(handle).await?;
        self.grep_guest(&sandbox, request).await
    }

    async fn find_files(
        &self,
        handle: &UpperHandle,
        request: &FindRequest,
    ) -> Result<Vec<WorkspacePath>> {
        let sandbox = self.clone_sandbox(handle).await?;
        self.find_files_guest(&sandbox, request).await
    }

    async fn exec(&self, handle: &UpperHandle, request: &ExecRequest) -> Result<ExecOutput> {
        let sandbox = self.clone_sandbox(handle).await?;
        self.exec_guest(&sandbox, request).await
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
