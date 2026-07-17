// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Concrete Local Workspace vocabulary and error normalization.
//!
//! These are the Local operation types the one concrete
//! [`super::LocalWorkspace`] and the native handlers/edit engine speak:
//! normalized project-relative paths, observed path facts, and bounded
//! observation requests. Errors raised by `LocalWorkspace` are normalized
//! into a small vocabulary so native CEP handlers can map them to stable CEP
//! error codes without leaking host paths or raw OS strings.

use std::fmt;
use std::path::{Component, Path, PathBuf};

/// Normalized project-relative UTF-8 path for Local Workspace operations.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct WorkspacePath {
    normalized: String,
}

impl WorkspacePath {
    /// Sole `"."` root sentinel for execution cwd/directory intents.
    ///
    /// Used by observation tests and by #39's Policy-Visible Workspace
    /// visibility checks; production handlers currently parse `"."` instead.
    #[allow(dead_code)]
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
        // `/workspace` was the retired Upper VM's guest root alias. Keep
        // rejecting it so stale agent prompts that still emit sandbox-shaped
        // absolute paths fail closed instead of resolving against the Local
        // Workspace root (#40).
        if lower == "/workspace" || lower.starts_with("/workspace/") {
            return Err(WorkspacePathError::SandboxRootAlias);
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
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum WorkspacePathError {
    #[error("workspace path must not be empty")]
    Empty,
    #[error("workspace path must not contain NUL")]
    NulByte,
    #[error("workspace path must not contain '..' traversal")]
    ParentTraversal,
    #[error("workspace path must be project-relative, not absolute")]
    Absolute,
    #[error("workspace path must not use the retired /workspace sandbox root alias")]
    SandboxRootAlias,
    #[error("workspace path is not valid UTF-8")]
    InvalidUtf8,
    #[error("project root sentinel is not valid for file read/write/remove intents")]
    ProjectRootNotAllowedForFileIntent,
}

/// How a path will be used by native handlers.
///
/// Resolution itself is intent-independent (per-segment symlink rejection +
/// root containment always apply); the intent is carried as operation
/// vocabulary for the guard/precondition layer and Policy-Visible Workspace
/// visibility checks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PathIntent {
    Read,
    Create,
    Overwrite,
    Delete,
    ListDirectory,
}

/// Kind of filesystem object observed in the Local Workspace.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PathKind {
    File,
    Directory,
    Symlink,
}

impl From<PathKind> for lancet_authority::guard::PathKind {
    fn from(kind: PathKind) -> Self {
        match kind {
            PathKind::File => lancet_authority::guard::PathKind::File,
            PathKind::Directory => lancet_authority::guard::PathKind::Directory,
            PathKind::Symlink => lancet_authority::guard::PathKind::Symlink,
        }
    }
}

/// Observed path facts produced by [`super::LocalWorkspace`] secure
/// resolution — no host paths.
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

impl PathFacts {
    /// Guard-layer view of these facts (drops the observation-only
    /// `byte_size` field). `LocalWorkspace` resolution is the single source
    /// of truth for both the handler flow and Guard L1 validation, so no
    /// per-call-site bridge is needed.
    pub fn to_guard(&self) -> lancet_authority::guard::PathFacts {
        lancet_authority::guard::PathFacts {
            normalized_path: self.normalized_path.clone(),
            exists: self.exists,
            kind: self.kind.map(Into::into),
            parent_exists: self.parent_exists,
            parent_kind: self.parent_kind.map(Into::into),
            canonical_under_workspace: self.canonical_under_workspace,
        }
    }
}

/// One directory entry from a Local Workspace listing.
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

/// One grep match within the Local Workspace.
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

/// Concrete Local Workspace error.
///
/// Variants follow ADR 0003: a rejected symlink, a root escape, a missing
/// target, an expected-hash conflict, and generic I/O. Host paths are kept
/// off the normalized surface where practical.
#[derive(Debug, thiserror::Error)]
pub enum LocalWorkspaceError {
    #[error("workspace path rejected: {0}")]
    Path(#[from] WorkspacePathError),
    #[error("symlink rejected at component: {component}")]
    SymlinkRejected { component: PathBuf },
    #[error("resolved path escapes the canonical project root")]
    RootEscape,
    #[error("path not found: {normalized}")]
    NotFound { normalized: String },
    #[error("target file already exists: {normalized}")]
    TargetExists { normalized: String },
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),
}

/// Record of an installed file mutation.
///
/// Carries the project-relative normalized path and the post-install content
/// hash, the Local Revision observed immediately after the atomic install.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstalledFile {
    pub path: WorkspacePath,
    pub content_hash: String,
    pub bytes_written: u64,
}

/// Record of a no-replace create.
///
/// Carries the project-relative normalized path, the post-install content
/// hash (Local Revision), and the ancestor directories the install created.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CreatedFile {
    pub path: WorkspacePath,
    pub content_hash: String,
    pub bytes_written: u64,
    pub created_dirs: Vec<String>,
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
    fn workspace_path_rejects_retired_sandbox_root_alias() {
        assert!(matches!(
            WorkspacePath::parse("/workspace"),
            Err(WorkspacePathError::SandboxRootAlias)
        ));
        assert!(matches!(
            WorkspacePath::parse("/workspace/src/main.rs"),
            Err(WorkspacePathError::SandboxRootAlias)
        ));
    }

    #[test]
    fn workspace_path_display_does_not_leak_host_paths() {
        let display_path = WorkspacePath::parse("src/main.rs").unwrap().to_string();
        assert_eq!(display_path, "src/main.rs");
        assert!(!display_path.contains("/home"));
        assert!(!display_path.contains("/workspace"));
    }

    #[test]
    fn path_facts_to_guard_view_drops_byte_size() {
        let facts = PathFacts {
            normalized_path: "src/main.rs".into(),
            exists: true,
            kind: Some(PathKind::File),
            parent_exists: true,
            parent_kind: Some(PathKind::Directory),
            byte_size: Some(42),
            canonical_under_workspace: true,
        };
        let guard = facts.to_guard();
        assert_eq!(guard.normalized_path, "src/main.rs");
        assert!(guard.exists);
        assert_eq!(guard.kind, Some(lancet_authority::guard::PathKind::File));
        assert!(guard.parent_exists);
        assert_eq!(
            guard.parent_kind,
            Some(lancet_authority::guard::PathKind::Directory)
        );
        assert!(guard.canonical_under_workspace);
    }
}
