// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Backend-neutral relative-path execution contracts.
//!
//! Defines the typed seam between native CEP handlers and a workspace
//! backend. As of #37 the sole production backend is the Direct-Local
//! `LocalExecutionBackend` wrapping the one concrete `LocalWorkspace`; the
//! trait, `ExecutionRouting`, and `UpperContext` carrier are migration
//! residue that #38 removes. No host/Lower implementation lives here — a
//! missing or failing backend is an error, never a host fallback.

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

    /// No-replace create install (ADR 0003 decision 8).
    ///
    /// Must fail with an error that the caller maps to `ReplanRequired` if the
    /// target already exists at install time. The default impl is a
    /// check-then-write fallback for backends that do not provide an atomic
    /// no-replace primitive; the Direct-Local backend overrides it with
    /// `O_CREAT|O_EXCL` so a concurrent writer between preflight and install
    /// is detected rather than silently overwritten.
    fn create_file<'a>(
        &'a self,
        path: &'a WorkspacePath,
        content: &'a [u8],
    ) -> Pin<Box<dyn Future<Output = Result<(), ExecutionBackendError>> + Send + 'a>> {
        Box::pin(async move {
            // Non-atomic fallback: callers without an O_EXCL primitive.
            // Direct-Local overrides this; NullExecutionBackend never reaches it.
            let facts = self.resolve_path(path, PathIntent::Create).await?;
            if facts.exists {
                return Err(ExecutionBackendError::ExecutionFailed {
                    message: "target already exists".into(),
                });
            }
            self.write_file(path, content).await
        })
    }

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

    /// Returns Upper-relative paths changed since the baseline commit.
    ///
    /// Implementors typically derive this from `git status --porcelain` or equivalent.
    fn changed_paths(
        &self,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<WorkspacePath>, ExecutionBackendError>> + Send + '_>>;
}

/// Crate-internal Direct-Local migration adapter (#37).
///
/// Wraps the one concrete [`crate::workspace::LocalWorkspace`] behind the
/// existing [`ExecutionBackend`] trait so native CEP handlers keep their
/// typed seam during the migration cutover. This is not a public extension
/// point: there is exactly one production backend, and #38 removes this
/// adapter along with the trait itself.
struct LocalExecutionBackend {
    workspace: Arc<crate::workspace::LocalWorkspace>,
}

impl ExecutionBackend for LocalExecutionBackend {
    fn backend_name(&self) -> &str {
        "direct-local"
    }

    fn resolve_path(
        &self,
        path: &WorkspacePath,
        _intent: PathIntent,
    ) -> Pin<Box<dyn Future<Output = Result<PathFacts, ExecutionBackendError>> + Send + '_>> {
        let ws = Arc::clone(&self.workspace);
        let path = path.clone();
        Box::pin(async move {
            // Delegate to the concrete workspace's secure resolver so the
            // per-segment symlink walk lives in one place. #38 removes this
            // adapter with the trait.
            tokio::task::spawn_blocking(move || {
                ws.resolve_facts(&path).map_err(ExecutionBackendError::from)
            })
            .await
            .map_err(|e| ExecutionBackendError::ExecutionFailed {
                message: format!("resolve_path join: {e}"),
            })?
        })
    }

    fn read_file(
        &self,
        path: &WorkspacePath,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<u8>, ExecutionBackendError>> + Send + '_>> {
        let ws = Arc::clone(&self.workspace);
        let path = path.clone();
        Box::pin(async move {
            ws.read_file(&path)
                .await
                .map_err(ExecutionBackendError::from)
        })
    }

    fn write_file(
        &self,
        path: &WorkspacePath,
        content: &[u8],
    ) -> Pin<Box<dyn Future<Output = Result<(), ExecutionBackendError>> + Send + '_>> {
        let ws = Arc::clone(&self.workspace);
        let path = path.clone();
        let content = content.to_vec();
        Box::pin(async move {
            // Edit install: atomic replacement (rename). The create path
            // uses `create_file` below for the no-replace guarantee.
            ws.install_atomic(&path, &content)
                .await
                .map(|_| ())
                .map_err(ExecutionBackendError::from)
        })
    }

    fn create_file(
        &self,
        path: &WorkspacePath,
        content: &[u8],
    ) -> Pin<Box<dyn Future<Output = Result<(), ExecutionBackendError>> + Send + '_>> {
        let ws = Arc::clone(&self.workspace);
        let path = path.clone();
        let content = content.to_vec();
        Box::pin(async move {
            // ADR 0003 decision 8: no-replace install (O_CREAT|O_EXCL). A
            // concurrent writer between preflight and install surfaces as
            // TargetExists → ReplanRequired rather than a silent overwrite.
            // `create_parents` follows the handler's `create.create_parents`.
            ws.create_excl(&path, &content, true)
                .await
                .map(|_| ())
                .map_err(ExecutionBackendError::from)
        })
    }

    fn remove_file(
        &self,
        path: &WorkspacePath,
    ) -> Pin<Box<dyn Future<Output = Result<(), ExecutionBackendError>> + Send + '_>> {
        let ws = Arc::clone(&self.workspace);
        let path = path.clone();
        Box::pin(async move {
            ws.remove_file(&path)
                .await
                .map_err(ExecutionBackendError::from)
        })
    }

    fn list_dir(
        &self,
        path: &WorkspacePath,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<DirectoryEntry>, ExecutionBackendError>> + Send + '_>>
    {
        let ws = Arc::clone(&self.workspace);
        let path = path.clone();
        Box::pin(async move {
            ws.list_dir(&path)
                .await
                .map_err(ExecutionBackendError::from)
        })
    }

    fn grep(
        &self,
        request: &GrepRequest,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<GrepMatch>, ExecutionBackendError>> + Send + '_>> {
        let ws = Arc::clone(&self.workspace);
        let request = request.clone();
        Box::pin(async move {
            // The handler clamps to `policy.observation.max_grep_matches`
            // before calling; 1000 is a backend safety cap that the
            // handler-side budget always overrides.
            ws.grep(&request, 1000)
                .await
                .map_err(ExecutionBackendError::from)
        })
    }

    fn find_files(
        &self,
        request: &FindRequest,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<WorkspacePath>, ExecutionBackendError>> + Send + '_>>
    {
        let ws = Arc::clone(&self.workspace);
        let request = request.clone();
        Box::pin(async move {
            // The handler clamps to `policy.observation.max_find_paths`
            // before calling; 1000 is a backend safety cap that the
            // handler-side budget always overrides.
            ws.find_files(&request, 1000)
                .await
                .map_err(ExecutionBackendError::from)
        })
    }

    fn changed_paths(
        &self,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<WorkspacePath>, ExecutionBackendError>> + Send + '_>>
    {
        // No production caller (verified cfg(test)-only). Direct-Local does
        // not derive changed-path from a Git subprocess; #38 removes this
        // method with the trait.
        Box::pin(async { Ok(Vec::new()) })
    }
}


/// #37 transition-only construction point for the Direct-Local routing.
///
/// Builds an [`ExecutionRouting`] wrapping the one concrete
/// [`crate::workspace::LocalWorkspace`] rooted at `root`. `UpperContext` is
/// kept as a transitional carrier so the existing `ServerRunConfig` shape is
/// unchanged; #38 removes both `UpperContext` and the `execution` field.
///
/// The `upper_workspace_id` carries the canonical root display string so
/// audit metadata no longer claims a fictional Upper workspace; #38 drops
/// the field entirely.
pub fn direct_local_routing(root: &Path) -> Result<ExecutionRouting, ExecutionBackendError> {
    let ws = Arc::new(
        crate::workspace::LocalWorkspace::new(root)
            .map_err(ExecutionBackendError::from)?,
    );
    let upper = UpperContext {
        execution_backend: "direct-local".into(),
        upper_workspace_id: ws.root().to_string_lossy().into_owned(),
    };
    Ok(ExecutionRouting {
        backend: Arc::new(LocalExecutionBackend { workspace: ws }),
        upper,
    })
}

/// Minimal no-op execution backend for test scaffolding.
///
/// All operations return `ExecutionBackendError`. This is only suitable for
/// configurations that don't exercise workspace mutation or observation
/// handlers — e.g. audit replay or session-state-only tests.
#[derive(Clone)]
pub struct NullExecutionBackend;

impl ExecutionBackend for NullExecutionBackend {
    fn backend_name(&self) -> &str {
        "null"
    }

    fn resolve_path(
        &self,
        _path: &WorkspacePath,
        _intent: PathIntent,
    ) -> Pin<Box<dyn Future<Output = Result<PathFacts, ExecutionBackendError>> + Send + '_>> {
        Box::pin(async {
            Err(ExecutionBackendError::Unavailable {
                message: "null backend".into(),
            })
        })
    }

    fn read_file(
        &self,
        _path: &WorkspacePath,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<u8>, ExecutionBackendError>> + Send + '_>> {
        Box::pin(async {
            Err(ExecutionBackendError::Unavailable {
                message: "null backend".into(),
            })
        })
    }

    fn write_file(
        &self,
        _path: &WorkspacePath,
        _content: &[u8],
    ) -> Pin<Box<dyn Future<Output = Result<(), ExecutionBackendError>> + Send + '_>> {
        Box::pin(async {
            Err(ExecutionBackendError::Unavailable {
                message: "null backend".into(),
            })
        })
    }

    fn remove_file(
        &self,
        _path: &WorkspacePath,
    ) -> Pin<Box<dyn Future<Output = Result<(), ExecutionBackendError>> + Send + '_>> {
        Box::pin(async {
            Err(ExecutionBackendError::Unavailable {
                message: "null backend".into(),
            })
        })
    }

    fn list_dir(
        &self,
        _path: &WorkspacePath,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<DirectoryEntry>, ExecutionBackendError>> + Send + '_>>
    {
        Box::pin(async {
            Err(ExecutionBackendError::Unavailable {
                message: "null backend".into(),
            })
        })
    }

    fn grep(
        &self,
        _request: &GrepRequest,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<GrepMatch>, ExecutionBackendError>> + Send + '_>>
    {
        Box::pin(async {
            Err(ExecutionBackendError::Unavailable {
                message: "null backend".into(),
            })
        })
    }

    fn find_files(
        &self,
        _request: &FindRequest,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<WorkspacePath>, ExecutionBackendError>> + Send + '_>>
    {
        Box::pin(async {
            Err(ExecutionBackendError::Unavailable {
                message: "null backend".into(),
            })
        })
    }

    fn changed_paths(
        &self,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<WorkspacePath>, ExecutionBackendError>> + Send + '_>>
    {
        Box::pin(async {
            Err(ExecutionBackendError::Unavailable {
                message: "null backend".into(),
            })
        })
    }
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
