// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Workspace-local relative-path DTOs mirroring the native execution contract
//! without importing `lancet-native`.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Component, Path};

use thiserror::Error;

/// Maximum bytes accepted by [`Workspace::write_file`] before adapter invocation.
pub const MAX_WORKSPACE_WRITE_BYTES: usize = 1024 * 1024;

/// Default cap for grep result rows when callers omit an explicit limit.
pub const DEFAULT_GREP_MAX_RESULTS: usize = 10_000;

/// Default cap for find-files results when callers omit an explicit limit.
pub const DEFAULT_FIND_MAX_RESULTS: usize = 10_000;

/// Normalized project-relative UTF-8 path for Upper workspace operations.
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

/// How a path will be used by workspace primitives.
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

/// Observed path facts returned by adapter preflight.
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
    pub max_results: usize,
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
    pub max_results: usize,
}

/// Typed argv execution request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExecRequest {
    pub argv: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub timeout_ms: u64,
    pub cwd: WorkspacePath,
}

impl ExecRequest {
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

/// Return the parent directory of a workspace-relative path, defaulting to `"."`.
///
/// Shared by the microsandbox production adapter and the deterministic test
/// adapter.  Pure function — no filesystem access.
pub(crate) fn parent_relative(path: &str) -> String {
    if path == "." {
        return ".".to_string();
    }
    match Path::new(path).parent() {
        None => ".".to_string(),
        Some(p) => {
            let s = p.to_string_lossy();
            if s.is_empty() || s == "." {
                ".".to_string()
            } else {
                s.into_owned()
            }
        }
    }
}
