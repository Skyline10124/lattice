// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Upper-workspace execution adapter seam (MVP).
//!
//! This crate is the small, swappable boundary the cube-upper-git-session
//! design calls `CubeWorkspace`: a single [`Workspace`] trait with concrete
//! backends behind it. The host drives a sandbox through this trait and never
//! depends on a specific VMM, so the production microVM can be swapped without
//! touching turn-execution code.
//!
//! Two backends ship in the MVP:
//!
//! - [`microsandbox::MicrosandboxWorkspace`] — libkrun microVMs via the `msb`
//!   CLI. Full lifecycle including command output capture and disk snapshots.
//! - [`cube::CubeWorkspace`] — CubeSandbox via its E2B-compatible HTTP API for
//!   lifecycle, plus best-effort `cubecli` exec.
//!
//! Backends differ in what they can do; [`Workspace::capabilities`] reports the
//! truth per backend rather than pretending they are identical.

pub mod cube;
pub mod microsandbox;

use async_trait::async_trait;

/// Which concrete sandbox technology backs a [`Workspace`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Backend {
    /// libkrun microVMs driven by the `msb` CLI.
    Microsandbox,
    /// CubeSandbox (RustVMM/KVM) driven by its E2B-compatible API.
    Cube,
}

impl Backend {
    pub fn as_str(self) -> &'static str {
        match self {
            Backend::Microsandbox => "microsandbox",
            Backend::Cube => "cube",
        }
    }
}

/// What a backend can actually do, stated honestly per backend.
///
/// The seam does not flatten real differences: a caller inspects this before
/// relying on, say, snapshot semantics for a verify-discard layer.
#[derive(Clone, Copy, Debug)]
pub struct Capabilities {
    /// Can run a command inside a live sandbox.
    pub exec: bool,
    /// `exec` returns the command's stdout/stderr, not just its exit code.
    pub stdout_capture: bool,
    /// Disk-only snapshot of a stopped sandbox (filesystem, not memory).
    pub disk_snapshot: bool,
    /// Live memory+filesystem snapshot of a running sandbox.
    pub memory_snapshot: bool,
    /// `exec` requires host root (e.g. a privileged control socket).
    pub needs_root_exec: bool,
}

/// Request to provision one sandbox.
#[derive(Clone, Debug)]
pub struct SandboxSpec {
    /// Backend-specific image reference: an OCI ref for microsandbox, a
    /// template id for Cube.
    pub image: String,
    /// Wall-clock budget in seconds before the backend may reap the sandbox.
    pub timeout_secs: u64,
}

/// A live sandbox returned by [`Workspace::create`].
#[derive(Clone, Debug)]
pub struct SandboxHandle {
    /// Backend-assigned sandbox id / name.
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

#[derive(Debug, thiserror::Error)]
pub enum WorkspaceError {
    #[error("{backend} backend process failed: {msg}")]
    Process { backend: Backend, msg: String },
    #[error("{backend} HTTP error: {msg}")]
    Http { backend: Backend, msg: String },
    #[error("{0} is not wired in the MVP: {1}")]
    Unsupported(Backend, String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

impl std::fmt::Display for Backend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

pub type Result<T> = std::result::Result<T, WorkspaceError>;

/// The adapter seam: provision, execute in, snapshot, and tear down a sandbox.
///
/// Object-safe via `async_trait`, so the host holds a `Box<dyn Workspace>` and
/// stays ignorant of the concrete backend.
#[async_trait]
pub trait Workspace: Send + Sync {
    fn backend(&self) -> Backend;
    fn capabilities(&self) -> Capabilities;

    /// Boot a sandbox and return a handle to it.
    async fn create(&self, spec: &SandboxSpec) -> Result<SandboxHandle>;

    /// Run `argv` inside the sandbox and collect its result.
    async fn exec(&self, handle: &SandboxHandle, argv: &[&str]) -> Result<ExecOutput>;

    /// Capture a snapshot of the sandbox under `name`, returning its reference.
    ///
    /// Backends without snapshot support return [`WorkspaceError::Unsupported`].
    async fn snapshot(&self, handle: &SandboxHandle, name: &str) -> Result<String>;

    /// Destroy the sandbox. Idempotent: destroying an already-gone sandbox is
    /// not an error.
    async fn destroy(&self, handle: &SandboxHandle) -> Result<()>;
}
