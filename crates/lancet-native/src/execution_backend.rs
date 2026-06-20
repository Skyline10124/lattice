// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Backend-neutral relative-path execution contracts for Upper routing.
//!
//! Defines the typed seam between native CEP handlers and an Upper workspace
//! backend. No host/Lower implementation lives here — a missing or failing
//! backend is an error, never a host fallback.

use std::collections::BTreeMap;
use std::fmt;
use std::future::Future;
use std::path::{Component, Path};
use std::pin::Pin;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use thiserror::Error;

/// Immutable routing identity for one Upper workspace backend.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UpperContext {
    /// Stable backend adapter name (never a host path).
    pub execution_backend: String,
    /// Backend-assigned Upper workspace identifier.
    pub upper_workspace_id: String,
}

/// Pairs one shared backend with immutable Upper identity for a turn.
#[derive(Clone)]
pub struct ExecutionRouting {
    pub backend: Arc<dyn ExecutionBackend>,
    pub upper: UpperContext,
}

/// Monotonic per-call routing invocation flags owned by [`HandlerContext`](crate::server::registry::HandlerContext).
#[derive(Clone, Debug, Default)]
pub struct RoutingInvocationState {
    adapter_preflight_invoked: Arc<AtomicBool>,
    backend_invoked: Arc<AtomicBool>,
}

impl RoutingInvocationState {
    /// Mark adapter preflight as invoked (monotonic — subsequent calls are no-ops).
    pub fn mark_adapter_preflight_invoked(&self) {
        self.adapter_preflight_invoked.store(true, Ordering::SeqCst);
    }

    /// Mark backend execution as invoked (monotonic).
    pub fn mark_backend_invoked(&self) {
        self.backend_invoked.store(true, Ordering::SeqCst);
    }

    pub fn adapter_preflight_invoked(&self) -> bool {
        self.adapter_preflight_invoked.load(Ordering::SeqCst)
    }

    pub fn backend_invoked(&self) -> bool {
        self.backend_invoked.load(Ordering::SeqCst)
    }
}

/// Normalized project-relative UTF-8 path for Upper execution.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct WorkspacePath {
    normalized: String,
}

impl WorkspacePath {
    /// Sole `"."` root sentinel for execution cwd/directory intents.
    pub fn project_root() -> Self {
        Self {
            normalized: ".".to_string(),
        }
    }

    /// Parse and normalize a project-relative path.
    ///
    /// Rejects absolute paths, empty strings, NUL bytes, `..` traversal,
    /// platform-specific prefixes, and the literal `/workspace` root alias.
    pub fn parse(raw: &str) -> Result<Self, WorkspacePathError> {
        if raw.is_empty() {
            return Err(WorkspacePathError::Empty);
        }
        if raw.contains('\0') {
            return Err(WorkspacePathError::NulByte);
        }
        if raw.contains("..") {
            return Err(WorkspacePathError::ParentTraversal);
        }
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Err(WorkspacePathError::Empty);
        }
        let lower = trimmed.to_ascii_lowercase();
        if lower == "/workspace" || lower.starts_with("/workspace/") {
            return Err(WorkspacePathError::UpperRootAlias);
        }
        if Path::new(trimmed).is_absolute() {
            return Err(WorkspacePathError::Absolute);
        }
        if trimmed.starts_with('/') || trimmed.starts_with('\\') {
            return Err(WorkspacePathError::Absolute);
        }
        #[cfg(windows)]
        if trimmed.len() >= 2 {
            let bytes = trimmed.as_bytes();
            if bytes[1] == b':' {
                return Err(WorkspacePathError::Absolute);
            }
        }
        let normalized = normalize_relative(trimmed)?;
        if normalized.is_empty() {
            return Err(WorkspacePathError::Empty);
        }
        Ok(Self { normalized })
    }

    pub fn as_str(&self) -> &str {
        &self.normalized
    }

    pub fn is_project_root(&self) -> bool {
        self.normalized == "."
    }

    /// Reject the project-root sentinel for file read/write/remove intents.
    pub fn require_file_path(&self) -> Result<(), WorkspacePathError> {
        if self.is_project_root() {
            Err(WorkspacePathError::ProjectRootNotAllowedForFileIntent)
        } else {
            Ok(())
        }
    }
}

impl fmt::Display for WorkspacePath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.normalized)
    }
}

fn normalize_relative(raw: &str) -> Result<String, WorkspacePathError> {
    let path = Path::new(raw);
    let mut parts = Vec::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => {
                let s = part.to_str().ok_or(WorkspacePathError::InvalidUtf8)?;
                parts.push(s.to_string());
            }
            Component::CurDir => {}
            Component::ParentDir => return Err(WorkspacePathError::ParentTraversal),
            Component::RootDir | Component::Prefix(_) => {
                return Err(WorkspacePathError::Absolute);
            }
        }
    }
    if parts.is_empty() {
        return Ok(".".to_string());
    }
    Ok(parts.join("/"))
}

/// Why a relative path was rejected at parse time.
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum WorkspacePathError {
    #[error("workspace path must not be empty")]
    Empty,
    #[error("workspace path must not contain NUL")]
    NulByte,
    #[error("workspace path must not contain '..' traversal")]
    ParentTraversal,
    #[error("workspace path must be project-relative, not absolute")]
    Absolute,
    #[error("workspace path must not use the /workspace root alias")]
    UpperRootAlias,
    #[error("workspace path is not valid UTF-8")]
    InvalidUtf8,
    #[error("project root sentinel is not valid for file read/write/remove intents")]
    ProjectRootNotAllowedForFileIntent,
}

/// How a path will be used by native handlers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PathIntent {
    Read,
    Create,
    Overwrite,
    Delete,
    ListDirectory,
    ExecCwd,
}

/// Kind of filesystem object observed in Upper.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PathKind {
    File,
    Directory,
    Symlink,
}

/// Observed path facts returned by adapter preflight — no host paths.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PathFacts {
    pub normalized_path: String,
    pub exists: bool,
    pub kind: Option<PathKind>,
    pub parent_exists: bool,
    pub parent_kind: Option<PathKind>,
    pub byte_size: Option<u64>,
    pub canonical_under_workspace: bool,
}

/// One directory entry from Upper listing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirectoryEntry {
    pub name: String,
    pub kind: PathKind,
}

/// Grep request over a relative directory tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GrepRequest {
    pub pattern: String,
    pub path: WorkspacePath,
    pub glob: Option<String>,
}

/// One grep match within Upper.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GrepMatch {
    pub path: WorkspacePath,
    pub line_number: u64,
    pub line: String,
}

/// Find-files request over a relative directory tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FindRequest {
    pub pattern: String,
    pub path: WorkspacePath,
}

/// Typed argv execution request — no shell command string.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExecRequest {
    pub argv: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub timeout_ms: u64,
    pub cwd: WorkspacePath,
}

impl ExecRequest {
    /// Build an exec request; rejects empty argv at construction.
    pub fn new(
        argv: Vec<String>,
        env: BTreeMap<String, String>,
        timeout_ms: u64,
        cwd: WorkspacePath,
    ) -> Result<Self, ExecRequestError> {
        if argv.is_empty() {
            return Err(ExecRequestError::EmptyArgv);
        }
        Ok(Self {
            argv,
            env,
            timeout_ms,
            cwd,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum ExecRequestError {
    #[error("exec argv must not be empty")]
    EmptyArgv,
}

/// Command output from Upper execution.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExecOutput {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
}

/// Immutable host configuration for Direct-mode post-mutation verification.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirectVerifyConfig {
    pub argv: Vec<String>,
    pub cwd: WorkspacePath,
    pub timeout_ms: Option<u64>,
    pub expected_exit_code: i32,
}

impl DirectVerifyConfig {
    pub fn validate(&self) -> Result<(), DirectVerifyConfigError> {
        if self.argv.is_empty() {
            return Err(DirectVerifyConfigError::EmptyArgv);
        }
        self.cwd
            .require_file_path()
            .map_err(|_| DirectVerifyConfigError::InvalidCwd)?;
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum DirectVerifyConfigError {
    #[error("direct verify argv must not be empty")]
    EmptyArgv,
    #[error("direct verify cwd must be a file-capable relative path")]
    InvalidCwd,
}

/// Errors crossing the Upper execution boundary.
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum ExecutionBackendError {
    #[error("path error: {0}")]
    Path(#[from] WorkspacePathError),
    #[error("backend unavailable: {message}")]
    Unavailable { message: String },
    #[error("backend execution failed: {message}")]
    ExecutionFailed { message: String },
    #[error("content too large")]
    ContentTooLarge,
    #[error("operation timed out")]
    Timeout,
}

/// Low-level Upper workspace operations invoked by native handlers.
pub trait ExecutionBackend: Send + Sync {
    fn backend_name(&self) -> &str;

    fn resolve_path(
        &self,
        path: &WorkspacePath,
        intent: PathIntent,
    ) -> Pin<Box<dyn Future<Output = Result<PathFacts, ExecutionBackendError>> + Send + '_>>;

    fn read_file(
        &self,
        path: &WorkspacePath,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<u8>, ExecutionBackendError>> + Send + '_>>;

    fn write_file(
        &self,
        path: &WorkspacePath,
        content: &[u8],
    ) -> Pin<Box<dyn Future<Output = Result<(), ExecutionBackendError>> + Send + '_>>;

    fn remove_file(
        &self,
        path: &WorkspacePath,
    ) -> Pin<Box<dyn Future<Output = Result<(), ExecutionBackendError>> + Send + '_>>;

    fn list_dir(
        &self,
        path: &WorkspacePath,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<DirectoryEntry>, ExecutionBackendError>> + Send + '_>>;

    fn grep(
        &self,
        request: &GrepRequest,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<GrepMatch>, ExecutionBackendError>> + Send + '_>>;

    fn find_files(
        &self,
        request: &FindRequest,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<WorkspacePath>, ExecutionBackendError>> + Send + '_>>;

    fn exec(
        &self,
        request: &ExecRequest,
    ) -> Pin<Box<dyn Future<Output = Result<ExecOutput, ExecutionBackendError>> + Send + '_>>;

    /// Returns Upper-relative paths changed since the baseline commit.
    ///
    /// Implementors typically derive this from `git status --porcelain` or equivalent.
    fn changed_paths(
        &self,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<WorkspacePath>, ExecutionBackendError>> + Send + '_>>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_path_rejects_absolute() {
        assert!(matches!(
            WorkspacePath::parse("/etc/passwd"),
            Err(WorkspacePathError::Absolute)
        ));
        assert!(matches!(
            WorkspacePath::parse("src/../secret"),
            Err(WorkspacePathError::ParentTraversal)
        ));
    }

    #[test]
    fn workspace_path_rejects_empty_and_nul() {
        assert!(matches!(
            WorkspacePath::parse(""),
            Err(WorkspacePathError::Empty)
        ));
        assert!(matches!(
            WorkspacePath::parse("src\0/main.rs"),
            Err(WorkspacePathError::NulByte)
        ));
    }

    #[test]
    fn workspace_path_normalizes_repeated_separators() {
        let path = WorkspacePath::parse("src//lib//main.rs").unwrap();
        assert_eq!(path.as_str(), "src/lib/main.rs");
    }

    #[test]
    fn workspace_path_project_root_sentinel() {
        let root = WorkspacePath::project_root();
        assert_eq!(root.as_str(), ".");
        assert!(root.is_project_root());
        assert!(matches!(
            root.require_file_path(),
            Err(WorkspacePathError::ProjectRootNotAllowedForFileIntent)
        ));
    }

    #[test]
    fn workspace_path_git_is_syntactically_valid() {
        let git = WorkspacePath::parse(".git/config").unwrap();
        assert_eq!(git.as_str(), ".git/config");
    }

    #[test]
    fn workspace_path_rejects_upper_root_alias() {
        assert!(matches!(
            WorkspacePath::parse("/workspace"),
            Err(WorkspacePathError::UpperRootAlias)
        ));
        assert!(matches!(
            WorkspacePath::parse("/workspace/src/main.rs"),
            Err(WorkspacePathError::UpperRootAlias)
        ));
    }

    #[test]
    fn exec_request_rejects_empty_argv() {
        let cwd = WorkspacePath::project_root();
        assert!(matches!(
            ExecRequest::new(vec![], BTreeMap::new(), 1000, cwd),
            Err(ExecRequestError::EmptyArgv)
        ));
    }

    #[test]
    fn direct_verify_config_rejects_invalid() {
        let bad = DirectVerifyConfig {
            argv: vec![],
            cwd: WorkspacePath::project_root(),
            timeout_ms: None,
            expected_exit_code: 0,
        };
        assert!(bad.validate().is_err());
    }

    #[test]
    fn debug_display_do_not_leak_host_paths() {
        let upper = UpperContext {
            execution_backend: "deterministic".into(),
            upper_workspace_id: "upper-abc".into(),
        };
        let debug = format!("{upper:?}");
        let display_path = WorkspacePath::parse("src/main.rs").unwrap().to_string();
        assert!(!debug.contains("/home"));
        assert!(!debug.contains("/workspace"));
        assert_eq!(display_path, "src/main.rs");
    }
}
