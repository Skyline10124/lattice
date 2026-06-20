// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Upper-workspace execution adapter seam.
//!
//! This crate provides the [`Workspace`] trait, a single swappable boundary
//! between turn-execution code and the concrete sandbox backend.  The trait
//! exposes the `UpperSpec`/`UpperHandle` lifecycle that powers production Upper
//! workspace provisioning.
//!
//! # Backends
//!
//! - [`microsandbox::MicrosandboxWorkspace`] — libkrun microVMs via the
//!   `microsandbox` Rust SDK 0.5.6.  Full Upper lifecycle: baseline-commit
//!   provisioning, command output capture, egress policy enforcement, warm
//!   templates, and disk snapshots.
//! - The `DeterministicWorkspace` test adapter (`git2` + tempdir, behind the
//!   `test-utils` feature) satisfies the same contract without KVM, for CI.
//!
//! # Security Notes
//!
//! - [`UpperHandle`] carries only `id` + `backend`; no host paths or live SDK
//!   handles are exposed to callers (WS-02, T-33-01).
//! - [`CredentialSpec`] implements `Debug` manually to redact secret values
//!   from logs and traces (CRED-01, T-33-02).
//! - [`validate_baseline_oid`] enforces hex-only 40-char format before any OID
//!   is used in exec arguments (T-33-05).
//! - [`apply_egress`] builds `NetworkPolicy::none().allow_domains([...])?` for
//!   allowlist policies; there is no `NetworkPolicy::allowlist()` constructor
//!   (T-33-03; Wave 0 A7 confirmed DNS-native resolution for allowlisted domains).

use std::path::PathBuf;

use async_trait::async_trait;

pub mod microsandbox;
mod paths;
#[cfg(any(test, feature = "test-utils"))]
mod primitives;

pub use paths::{
    DEFAULT_FIND_MAX_RESULTS, DEFAULT_GREP_MAX_RESULTS, DirectoryEntry, ExecRequest,
    ExecRequestError, FindRequest, GrepMatch, GrepRequest, MAX_WORKSPACE_WRITE_BYTES, PathFacts,
    PathIntent, PathKind, WorkspacePath, WorkspacePathError,
};

// Shared git2 helpers used by both MicrosandboxWorkspace and DeterministicWorkspace.
// Exposed as pub so downstream crates (33-04 DeterministicWorkspace) can call them.
pub mod git;
mod template;

pub use git::{checkout_tree_to_dir, git_archive_to_tempdir};

// Feature-gated test infrastructure.
#[cfg(any(test, feature = "test-utils"))]
pub mod testing;

// Re-export WarmTemplate for consumers that need to track template staleness.
pub use template::WarmTemplate;

// ── Backend ──────────────────────────────────────────────────────────────────

/// Which concrete sandbox technology backs a [`Workspace`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Backend {
    /// libkrun microVMs driven by the `microsandbox` Rust SDK 0.5.6.
    Microsandbox,
    /// In-process test adapter using `git2` + tempdir; no KVM required.
    Deterministic,
}

impl Backend {
    pub fn as_str(self) -> &'static str {
        match self {
            Backend::Microsandbox => "microsandbox",
            Backend::Deterministic => "deterministic",
        }
    }
}

impl std::fmt::Display for Backend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

// ── Upper types ───────────────────────────────────────────────────────────────

/// Rootfs source for an Upper: either a fresh OCI image or a warm SDK snapshot.
#[derive(Clone, Debug)]
pub enum ImageOrTemplate {
    /// Fully-qualified OCI image ref.  Never a bare tag like `"python:3.12-slim"`;
    /// always include the registry host, e.g.
    /// `"docker.m.daocloud.io/library/python:3.12-slim"`.
    Image(String),
    /// SDK snapshot name stored under `~/.microsandbox/snapshots/`.
    /// Matches the value returned by [`Workspace::template_create`].
    Template(String),
}

/// A single allowlist entry for [`EgressPolicy::Allowlist`].
#[derive(Clone, Debug)]
pub struct AllowlistEntry {
    /// Domain name or CIDR string, e.g. `"api.openai.com"` or `"10.0.0.0/8"`.
    pub target: String,
}

/// Network egress policy applied at Upper creation time.
///
/// Mapped to `NetworkPolicy` via [`plan_egress`] + [`apply_egress`].
#[derive(Clone, Debug)]
pub enum EgressPolicy {
    /// Default public internet (no `.network()` call — SDK default).
    Open,
    /// Only the listed domains/CIDRs are reachable from inside the Upper.
    ///
    /// Implemented as `NetworkPolicy::none().allow_domains([...])?` (Wave 0 A7
    /// confirmed DNS-native resolution; there is **no** `NetworkPolicy::allowlist()`).
    Allowlist(Vec<AllowlistEntry>),
    /// No network access at all (`NetworkPolicy::none()`).
    Blocked,
}

/// Credential to inject into an Upper at creation time.
///
/// # Security (CRED-01, T-33-02)
///
/// This type intentionally does **not** derive `Debug`.  The manual `Debug`
/// implementation below redacts secret values so they never appear in logs,
/// tracing spans, or panic messages.
///
/// `UpperSpec` derives `Debug` only because `CredentialSpec`'s redacting `Debug`
/// propagates through the `Vec<CredentialSpec>` field.
pub enum CredentialSpec {
    /// Inject a resolved env-var value into the Upper at builder time.
    EnvVar { name: String, value: String },
    /// Deliver a file into the Upper via exec-based injection after boot.
    /// `content` is the raw file bytes; `guest_path` is the absolute path inside
    /// the Upper (e.g. `"/run/secrets/api-key"`).
    File {
        content: Vec<u8>,
        guest_path: String,
    },
}

impl Clone for CredentialSpec {
    fn clone(&self) -> Self {
        match self {
            CredentialSpec::EnvVar { name, value } => CredentialSpec::EnvVar {
                name: name.clone(),
                value: value.clone(),
            },
            CredentialSpec::File {
                content,
                guest_path,
            } => CredentialSpec::File {
                content: content.clone(),
                guest_path: guest_path.clone(),
            },
        }
    }
}

/// Manual `Debug` implementation — NEVER derive `Debug` on `CredentialSpec`.
///
/// Redacts `value` (env var secret) and `content` (file secret bytes) so they
/// cannot leak via `{:?}` formatting in logs, tracing spans, or error messages.
impl std::fmt::Debug for CredentialSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CredentialSpec::EnvVar { name, .. } => f
                .debug_struct("EnvVar")
                .field("name", name)
                .field("value", &"[REDACTED]")
                .finish(),
            CredentialSpec::File { guest_path, .. } => f
                .debug_struct("File")
                .field("guest_path", guest_path)
                .field("content", &"[REDACTED]")
                .finish(),
        }
    }
}

/// Request to provision one Upper workspace.
///
/// # Security notes
///
/// - `host_repo_path` is used only on the host side for git-archive export; it
///   is **never** passed to exec argv or into any SDK builder chain visible to
///   the model (WS-02, T-33-01).
/// - `baseline_oid` must be validated by [`validate_baseline_oid`] before use
///   to prevent OID-based argv injection (T-33-05).
#[derive(Clone, Debug)]
pub struct UpperSpec {
    /// Rootfs source: fresh OCI image or a named warm SDK snapshot.
    pub image_or_template: ImageOrTemplate,
    /// 40-character hex Git OID of the baseline commit to check out.
    /// Validated by [`validate_baseline_oid`] before first use.
    pub baseline_oid: String,
    /// Host path to the git repository containing `baseline_oid`.
    /// Used only for git-archive export on the host; never passed into exec.
    pub host_repo_path: PathBuf,
    /// Network egress policy applied at creation time.
    pub egress: EgressPolicy,
    /// Credentials injected at creation time (env vars) or post-boot (files).
    pub credentials: Vec<CredentialSpec>,
    /// Wall-clock budget in seconds for the Upper's lifetime.
    pub timeout_secs: u64,
    /// vCPU count for the microVM.
    pub cpus: u32,
    /// Memory in MiB for the microVM.
    pub memory_mb: u32,
}

/// Handle to a live Upper workspace.
///
/// # Security (WS-02, T-33-01)
///
/// `UpperHandle` carries only `id` + `backend`.  No host paths, no live SDK
/// `Sandbox` fields.  The live SDK object is held inside the workspace
/// implementation's internal map, keyed by `id`.  This keeps `UpperHandle`
/// `Clone + Send + Sync` without wrapping assumptions about `Sandbox`'s own
/// thread-safety.
#[derive(Clone, Debug)]
pub struct UpperHandle {
    /// Backend-assigned unique identifier for this Upper.
    pub id: String,
    pub backend: Backend,
}

/// Result of running a command in a sandbox.
#[derive(Clone, Debug)]
pub struct ExecOutput {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
}

impl ExecOutput {
    pub fn success(&self) -> bool {
        self.exit_code == 0
    }
}

/// The result of `git status --porcelain=v1` inside an Upper.
#[derive(Clone, Debug, Default)]
pub struct GitStatus {
    /// Raw `git status --porcelain=v1` output.
    pub porcelain: String,
    /// `true` when `porcelain` is empty (working tree is clean).
    pub is_clean: bool,
}

// ── Errors ────────────────────────────────────────────────────────────────────

#[derive(Debug, thiserror::Error)]
pub enum WorkspaceError {
    #[error("{0} is not wired in the MVP: {1}")]
    Unsupported(Backend, String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    /// Wraps a `microsandbox` SDK error (converted to `String` to avoid SDK
    /// types leaking into the public error type).
    #[error("microsandbox SDK error: {0}")]
    Sdk(String),
    /// A `git2` operation failed.
    #[error("git operation failed: {0}")]
    Git(String),
    /// The provided OID string is not a valid 40-character hex SHA-1.
    #[error("invalid baseline OID `{0}`: must be 40 hex characters")]
    InvalidOid(String),
    #[error("invalid workspace path: {0}")]
    InvalidPath(String),
    #[error("path not found: {path}")]
    NotFound { path: String },
    #[error("wrong path kind for {path}: expected {expected}")]
    WrongKind { path: String, expected: String },
    #[error("path escapes workspace root: {path}")]
    PathEscape { path: String },
    #[error("content too large: {actual} bytes exceeds limit of {max}")]
    ContentTooLarge { actual: usize, max: usize },
    #[error("adapter unavailable: {0}")]
    Unavailable(String),
}

pub type Result<T> = std::result::Result<T, WorkspaceError>;

// ── OID validation ────────────────────────────────────────────────────────────

/// Validate that `oid` is exactly 40 lowercase/uppercase hex characters.
///
/// Returns [`WorkspaceError::InvalidOid`] if not.  Call this before using
/// `baseline_oid` in any exec argv to prevent injection (T-33-05).
pub fn validate_baseline_oid(oid: &str) -> Result<()> {
    if oid.len() == 40 && oid.chars().all(|c| c.is_ascii_hexdigit()) {
        Ok(())
    } else {
        Err(WorkspaceError::InvalidOid(oid.to_string()))
    }
}

// ── Egress plan + SDK translation ─────────────────────────────────────────────

/// Deterministically-inspectable egress decision.
///
/// [`plan_egress`] maps an [`EgressPolicy`] to this enum.  Because `NetPlan`
/// uses only standard types, the mapping can be unit-tested with `assert_eq!`
/// without any SDK types in scope.  `apply_egress` (in `microsandbox.rs`)
/// translates `NetPlan` into a concrete `microsandbox::sandbox::SandboxBuilder`
/// call.
///
/// See V-06 / V-07 for the det-tier tests.
// allow(dead_code): consumed in #[cfg(test)] egress_tests and in
// microsandbox::apply_egress; both are valid callers suppressed by the linter
// in non-test builds before 33-05 wires the full create_upper path.
#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum NetPlan {
    /// Use the SDK's default network (public internet, private IPs blocked).
    Open,
    /// Apply `NetworkPolicy::none()` — no network interfaces created.
    Blocked,
    /// Apply `NetworkPolicy::none().allow_domains(domains)?`.
    Allow(Vec<String>),
}

/// Map an [`EgressPolicy`] to an inspectable [`NetPlan`].
///
/// Pure function with no SDK types; suitable for `assert_eq!` unit tests
/// (V-06, V-07).  The SDK call is in `microsandbox::apply_egress`.
// allow(dead_code): called in egress_tests (#[cfg(test)]) and in
// microsandbox::apply_egress; suppressed before 33-05 full wiring.
#[allow(dead_code)]
pub(crate) fn plan_egress(policy: &EgressPolicy) -> NetPlan {
    match policy {
        EgressPolicy::Open => NetPlan::Open,
        EgressPolicy::Blocked => NetPlan::Blocked,
        EgressPolicy::Allowlist(entries) => {
            NetPlan::Allow(entries.iter().map(|e| e.target.clone()).collect())
        }
    }
}

// Note: apply_egress (the SDK-coupled builder translation) lives in
// crates/lattice-workspace/src/microsandbox.rs to avoid importing
// microsandbox::sandbox::SandboxBuilder here (which requires the microsandbox
// `net` feature to be unified into this compilation unit).  This keeps lib.rs
// free of SDK types while still exporting the inspectable plan_egress/NetPlan
// pair for deterministic unit tests.

// ── Workspace trait ───────────────────────────────────────────────────────────

/// The adapter seam: provision, execute in, snapshot, and tear down a sandbox.
///
/// Object-safe via `async_trait`, so the host holds a `Box<dyn Workspace>` and
/// stays ignorant of the concrete backend.
///
/// # Methods
///
/// The Upper workspace lifecycle: `create_upper`, `exec_upper`, `git_status`,
/// `template_create`, `restore_upper`, `destroy_upper`.
///
/// # Safety (exec_upper, T-33-05)
///
/// **Never call `sandbox.shell()` for CEP operations.**  Always use
/// `exec_upper(handle, prog, args_slice)` — the typed argv slice eliminates
/// shell injection surface.
#[async_trait]
pub trait Workspace: Send + Sync {
    /// Provision an Upper workspace from `spec`.
    ///
    /// Validates `spec.baseline_oid` before any use (T-33-05).
    /// Applies `spec.egress` via `NetworkPolicy` (EGRESS-01).
    /// Injects `spec.credentials` (CRED-01).
    async fn create_upper(&self, spec: &UpperSpec) -> Result<UpperHandle>;

    /// Run `prog args` inside the Upper.
    ///
    /// **Use an argv slice — never `shell()`** (T-33-05).
    async fn exec_upper(
        &self,
        handle: &UpperHandle,
        prog: &str,
        args: &[&str],
    ) -> Result<ExecOutput>;

    /// Return `git status --porcelain=v1` of the `/workspace` root.
    async fn git_status(&self, handle: &UpperHandle) -> Result<GitStatus>;

    /// Stop the Upper, snapshot it as a warm template, return the snapshot name.
    ///
    /// **T-33-04:** this path must NEVER inject credentials into the snapshot;
    /// credentials are applied only on `create_upper` / `restore_upper`.
    async fn template_create(&self, source: &UpperHandle, name: &str) -> Result<String>;

    /// Boot a fresh CoW Upper from an existing template snapshot.
    ///
    /// Per-turn credentials (env vars, files) are applied here, not in the
    /// snapshot (T-33-04).
    async fn restore_upper(
        &self,
        template_name: &str,
        new_name: &str,
        spec: &UpperSpec,
    ) -> Result<UpperHandle>;

    /// Destroy an Upper. Idempotent.
    async fn destroy_upper(&self, handle: &UpperHandle) -> Result<()>;

    /// Resolve normalized path facts for the given intent.
    async fn resolve_path(
        &self,
        handle: &UpperHandle,
        path: &WorkspacePath,
        intent: PathIntent,
    ) -> Result<PathFacts> {
        let _ = (handle, path, intent);
        Err(WorkspaceError::Unsupported(
            Backend::Microsandbox,
            "resolve_path".into(),
        ))
    }

    async fn read_file(&self, handle: &UpperHandle, path: &WorkspacePath) -> Result<Vec<u8>> {
        let _ = (handle, path);
        Err(WorkspaceError::Unsupported(
            Backend::Microsandbox,
            "read_file".into(),
        ))
    }

    async fn write_file(
        &self,
        handle: &UpperHandle,
        path: &WorkspacePath,
        content: &[u8],
    ) -> Result<()> {
        let _ = (handle, path, content);
        Err(WorkspaceError::Unsupported(
            Backend::Microsandbox,
            "write_file".into(),
        ))
    }

    async fn remove_file(&self, handle: &UpperHandle, path: &WorkspacePath) -> Result<()> {
        let _ = (handle, path);
        Err(WorkspaceError::Unsupported(
            Backend::Microsandbox,
            "remove_file".into(),
        ))
    }

    async fn list_dir(
        &self,
        handle: &UpperHandle,
        path: &WorkspacePath,
    ) -> Result<Vec<DirectoryEntry>> {
        let _ = (handle, path);
        Err(WorkspaceError::Unsupported(
            Backend::Microsandbox,
            "list_dir".into(),
        ))
    }

    async fn grep(&self, handle: &UpperHandle, request: &GrepRequest) -> Result<Vec<GrepMatch>> {
        let _ = (handle, request);
        Err(WorkspaceError::Unsupported(
            Backend::Microsandbox,
            "grep".into(),
        ))
    }

    async fn find_files(
        &self,
        handle: &UpperHandle,
        request: &FindRequest,
    ) -> Result<Vec<WorkspacePath>> {
        let _ = (handle, request);
        Err(WorkspaceError::Unsupported(
            Backend::Microsandbox,
            "find_files".into(),
        ))
    }

    /// Typed argv execution; default delegates to [`exec_upper`].
    async fn exec(&self, handle: &UpperHandle, request: &ExecRequest) -> Result<ExecOutput> {
        let prog = request
            .argv
            .first()
            .map(String::as_str)
            .ok_or_else(|| WorkspaceError::InvalidPath("empty exec argv".into()))?;
        let args: Vec<&str> = request.argv[1..].iter().map(String::as_str).collect();
        let _ = (&request.env, request.timeout_ms, &request.cwd);
        self.exec_upper(handle, prog, &args).await
    }
}

// ── Det-tier egress tests (V-06 / V-07) ──────────────────────────────────────

#[cfg(test)]
mod egress_tests {
    use super::*;

    #[test]
    fn blocked_maps_to_blocked_plan() {
        // V-06: plan_egress(Blocked) == NetPlan::Blocked
        assert_eq!(plan_egress(&EgressPolicy::Blocked), NetPlan::Blocked);
    }

    #[test]
    fn open_maps_to_open_plan() {
        assert_eq!(plan_egress(&EgressPolicy::Open), NetPlan::Open);
    }

    #[test]
    fn allowlist_propagates_domains_in_order() {
        // V-07: plan_egress(Allowlist([a, b])) == NetPlan::Allow([a, b])
        let policy = EgressPolicy::Allowlist(vec![
            AllowlistEntry {
                target: "api.openai.com".into(),
            },
            AllowlistEntry {
                target: "pypi.org".into(),
            },
        ]);
        assert_eq!(
            plan_egress(&policy),
            NetPlan::Allow(vec!["api.openai.com".into(), "pypi.org".into()]),
        );
    }

    #[test]
    fn allowlist_empty_is_empty_allow() {
        assert_eq!(
            plan_egress(&EgressPolicy::Allowlist(vec![])),
            NetPlan::Allow(vec![]),
        );
    }
}
