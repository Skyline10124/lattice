// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Concrete Local Workspace vocabulary and error normalization.
//!
//! These are the Local operation types shared by [`super::LocalWorkspace`] and
//! the Tool Host: normalized project-relative paths, observed path facts, and
//! bounded observation requests. Errors are normalized into a small vocabulary
//! so the runtime can return stable typed errors without leaking host paths or
//! raw OS strings.

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
    /// Used by observation tests and by the #61 (Codeberg #39) Policy-Visible
    /// Workspace visibility checks; production handlers currently parse `"."`
    /// instead.
    #[allow(dead_code)]
    pub fn project_root() -> Self {
        Self {
            normalized: ".".to_string(),
        }
    }

    /// Parse and normalize a project-relative path.
    ///
    /// Rejects absolute paths, empty strings, NUL bytes, `..` *components*,
    /// foreign drive prefixes, and the literal `/workspace` root alias. A
    /// literal `..` inside a single filename is not traversal and is kept.
    pub fn parse(raw: &str) -> Result<Self, WorkspacePathError> {
        if raw.is_empty() {
            return Err(WorkspacePathError::Empty);
        }
        if raw.contains('\0') {
            return Err(WorkspacePathError::NulByte);
        }
        // Traversal is rejected per component by `normalize_relative` below.
        // A substring test also caught legal names that merely contain the
        // two characters (`foo..bar.rs`, `..gitignore`), and because parse
        // failure aborts the whole call, one such file made an entire
        // grep/find return an error instead of skipping nothing (#98).
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

/// How a path will be used by a Tool Host operation.
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
    #[error("workspace operation cancelled")]
    Cancelled,
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
/// Carries the project-relative normalized path and post-install content hash
/// (Local Revision).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CreatedFile {
    pub path: WorkspacePath,
    pub content_hash: String,
    pub bytes_written: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_paths_reject_invalid_components_and_preserve_legal_names() {
        for (name, raw, expected) in [
            (
                "workspace_path_rejects_absolute/absolute",
                "/etc/passwd",
                Err(WorkspacePathError::Absolute),
            ),
            (
                "workspace_path_rejects_absolute/traversal",
                "src/../secret",
                Err(WorkspacePathError::ParentTraversal),
            ),
            (
                "workspace_path_rejects_empty_and_nul/empty",
                "",
                Err(WorkspacePathError::Empty),
            ),
            (
                "workspace_path_rejects_empty_and_nul/nul",
                "src\0/main.rs",
                Err(WorkspacePathError::NulByte),
            ),
            // Literal .. is not traversal: rejecting these names used to break entire grep/find searches.
            (
                "literal_dot_dot_in_a_filename_is_not_traversal/foo..bar.rs",
                "foo..bar.rs",
                Ok("foo..bar.rs"),
            ),
            (
                "literal_dot_dot_in_a_filename_is_not_traversal/..gitignore",
                "..gitignore",
                Ok("..gitignore"),
            ),
            (
                "literal_dot_dot_in_a_filename_is_not_traversal/src/a..b/c.rs",
                "src/a..b/c.rs",
                Ok("src/a..b/c.rs"),
            ),
            (
                "literal_dot_dot_in_a_filename_is_not_traversal/release..2026",
                "release..2026",
                Ok("release..2026"),
            ),
            (
                "literal_dot_dot_in_a_filename_is_not_traversal/x..",
                "x..",
                Ok("x.."),
            ),
            (
                "parent_dir_components_are_still_rejected/..",
                "..",
                Err(WorkspacePathError::ParentTraversal),
            ),
            (
                "parent_dir_components_are_still_rejected/../x",
                "../x",
                Err(WorkspacePathError::ParentTraversal),
            ),
            (
                "parent_dir_components_are_still_rejected/a/../b",
                "a/../b",
                Err(WorkspacePathError::ParentTraversal),
            ),
            (
                "parent_dir_components_are_still_rejected/a/..",
                "a/..",
                Err(WorkspacePathError::ParentTraversal),
            ),
            (
                "parent_dir_components_are_still_rejected/./../x",
                "./../x",
                Err(WorkspacePathError::ParentTraversal),
            ),
            (
                "parent_dir_components_are_still_rejected/a/b/../../..",
                "a/b/../../..",
                Err(WorkspacePathError::ParentTraversal),
            ),
        ] {
            assert_eq!(
                WorkspacePath::parse(raw).map(|path| path.as_str().to_owned()),
                expected.map(str::to_owned),
                "{name}: {raw:?}"
            );
        }
    }
}
