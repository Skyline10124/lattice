// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! The single Local Workspace root resolver (ADR 0005 §3, GitHub #143).
//!
//! CLI and TUI obtain the current Local Workspace root from this resolver —
//! neither has a separate binding heuristic, and there is no lazy first-Turn
//! binding path. The full runtime root is used: profile
//! `[lancet.project_root].root` is a policy subdirectory/scope and does not
//! participate in store addressing.

use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use lattice_session::ProjectStoreKey;

use crate::session::{ApiError, Result};

/// One explicitly resolved Local Workspace root: the canonical full root and
/// its derived `ProjectStoreKey` storage address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalWorkspaceRoot {
    canonical: PathBuf,
    store_key: ProjectStoreKey,
}

impl LocalWorkspaceRoot {
    /// Resolve the current Local Workspace root from the process working
    /// directory (the production resolution).
    pub fn resolve() -> Result<Self> {
        let cwd = std::env::current_dir().map_err(|source| {
            ApiError::Internal(format!("cannot read process working directory: {source}"))
        })?;
        Self::from_path(&cwd)
    }

    /// Resolve from an explicit path (tests, embedders).
    pub fn from_path(path: &Path) -> Result<Self> {
        let canonical = std::fs::canonicalize(path).map_err(|source| {
            ApiError::BadRequest(format!(
                "cannot canonicalize workspace root '{}': {source}",
                path.display()
            ))
        })?;
        let store_key =
            ProjectStoreKey::from_canonical_path_bytes(canonical.as_os_str().as_bytes());
        Ok(Self {
            canonical,
            store_key,
        })
    }

    /// The canonical root path.
    pub fn canonical_path(&self) -> &Path {
        &self.canonical
    }

    /// The derived storage address.
    pub fn store_key(&self) -> ProjectStoreKey {
        self.store_key
    }
}

/// Resolve the current Local Workspace root (see [`LocalWorkspaceRoot`]).
pub fn resolve_local_workspace_root() -> Result<LocalWorkspaceRoot> {
    LocalWorkspaceRoot::resolve()
}
