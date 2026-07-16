// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Concrete Local Workspace error normalization.
//!
//! Errors raised by [`super::LocalWorkspace`] are normalized into a small
//! vocabulary so native CEP handlers can map them to stable CEP error codes
//! without leaking host paths or raw OS strings.

use std::path::PathBuf;

use crate::execution_backend::{ExecutionBackendError, WorkspacePath, WorkspacePathError};

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
    #[error("expected hash mismatch for {normalized}: expected {expected}, actual {actual}")]
    HashMismatch {
        normalized: String,
        expected: String,
        actual: String,
    },
    #[error("content too large: {actual} bytes exceeds {max}")]
    ContentTooLarge { actual: u64, max: u64 },
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),
}

impl From<LocalWorkspaceError> for ExecutionBackendError {
    fn from(err: LocalWorkspaceError) -> Self {
        match err {
            LocalWorkspaceError::Path(p) => ExecutionBackendError::Path(p),
            LocalWorkspaceError::ContentTooLarge { .. } => ExecutionBackendError::ContentTooLarge,
            other => ExecutionBackendError::ExecutionFailed {
                message: other.to_string(),
            },
        }
    }
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
/// Currently constructed only by `LocalWorkspace::create_excl`, which the
/// production `mutate.create` handler does not yet route through (it uses
/// `install_atomic` via `EditStorage`). Retained as the no-replace seam for
/// #38's `lancet-runtime` convergence and the module's ADR-step-1 tests.
#[allow(dead_code)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CreatedFile {
    pub path: WorkspacePath,
    pub content_hash: String,
    pub bytes_written: u64,
    pub created_dirs: Vec<String>,
}
