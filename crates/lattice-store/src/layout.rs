// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! XDG state-root resolution and the fixed private-state directory topology
//! (ADR 0005 §3).
//!
//! All mutable private state lives under the XDG *state* root (never the
//! configuration root, never inside a workspace):
//!
//! ```text
//! $XDG_STATE_HOME/lattice/
//! ├── lattice.redb                              (owned by lattice-session)
//! ├── projects/<ProjectStoreKey>/workspace-history.git
//! └── locks/
//!     ├── sessions/<SessionId>.lock
//!     └── projects/<ProjectStoreKey>/workspace.lock
//! ```
//!
//! The kernel owns the topology; domain wrappers own their repository file
//! names, lock file names, kinds, and schema versions.

use std::path::{Path, PathBuf};

use crate::{ProjectStoreKey, StoreError};

/// Directory name directly under the XDG state root.
pub const LATTICE_STATE_DIR: &str = "lattice";

/// Resolve the XDG state root.
///
/// Resolution order (Linux-only per the platform contract):
///
/// 1. `$XDG_STATE_HOME` when set to an absolute path (relative values are
///    ignored per the XDG specification);
/// 2. `$HOME/.local/state` — the standard fallback.
///
/// Returns [`StoreError::MissingStateRoot`] when neither is available.
pub fn xdg_state_root() -> Result<PathBuf, StoreError> {
    if let Some(explicit) = std::env::var_os("XDG_STATE_HOME") {
        let path = PathBuf::from(explicit);
        if path.is_absolute() {
            return Ok(path);
        }
    }
    let home = std::env::var_os("HOME").ok_or(StoreError::MissingStateRoot)?;
    let home = PathBuf::from(home);
    if !home.is_absolute() {
        return Err(StoreError::MissingStateRoot);
    }
    Ok(home.join(".local").join("state"))
}

/// The fixed private-state layout under one state root.
///
/// Construct with [`StateLayout::resolve`] (production, XDG) or
/// [`StateLayout::in_state_root`] (tests/embedders with an explicit root).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateLayout {
    /// `<state-root>/lattice`
    lattice_root: PathBuf,
}

impl StateLayout {
    /// Resolve the production layout from the XDG state root.
    pub fn resolve() -> Result<Self, StoreError> {
        Ok(Self::in_state_root(&xdg_state_root()?))
    }

    /// The layout under an explicit state root.
    pub fn in_state_root(state_root: &Path) -> Self {
        Self {
            lattice_root: state_root.join(LATTICE_STATE_DIR),
        }
    }

    /// `<state>/lattice`
    pub fn lattice_root(&self) -> &Path {
        &self.lattice_root
    }

    /// `<state>/lattice/projects`
    pub fn projects_dir(&self) -> PathBuf {
        self.lattice_root.join("projects")
    }

    /// `<state>/lattice/projects/<ProjectStoreKey>`
    pub fn project_dir(&self, key: &ProjectStoreKey) -> PathBuf {
        self.projects_dir().join(key.to_string())
    }

    /// `<state>/lattice/projects/<ProjectStoreKey>/<store>` — the domain
    /// supplies its own repository file name.
    pub fn project_store(&self, key: &ProjectStoreKey, store_name: &str) -> PathBuf {
        self.project_dir(key).join(store_name)
    }

    /// `<state>/lattice/locks/sessions` — session lock files live one per
    /// SessionId under this directory.
    pub fn session_locks_dir(&self) -> PathBuf {
        self.lattice_root.join("locks").join("sessions")
    }

    /// `<state>/lattice/locks/sessions/<name>` — the domain validates and
    /// supplies the SessionId-derived file name.
    pub fn session_lock(&self, name: &str) -> PathBuf {
        self.session_locks_dir().join(name)
    }

    /// `<state>/lattice/locks/projects/<ProjectStoreKey>`
    pub fn project_locks_dir(&self, key: &ProjectStoreKey) -> PathBuf {
        self.lattice_root
            .join("locks")
            .join("projects")
            .join(key.to_string())
    }

    /// `<state>/lattice/locks/projects/<ProjectStoreKey>/<name>` — the domain
    /// supplies the lock file name (e.g. `workspace.lock`).
    pub fn project_lock(&self, key: &ProjectStoreKey, name: &str) -> PathBuf {
        self.project_locks_dir(key).join(name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_matches_adr_topology() {
        let layout = StateLayout::in_state_root(Path::new("/state"));
        assert_eq!(layout.lattice_root(), Path::new("/state/lattice"));
        assert_eq!(layout.projects_dir(), Path::new("/state/lattice/projects"));
        assert_eq!(
            layout.session_locks_dir(),
            Path::new("/state/lattice/locks/sessions")
        );
        let key = ProjectStoreKey::from_canonical_path_bytes(b"/ws");
        let hex = key.to_string();
        assert_eq!(
            layout.project_dir(&key),
            PathBuf::from(format!("/state/lattice/projects/{hex}"))
        );
        assert_eq!(
            layout.project_store(&key, "workspace-history.git"),
            PathBuf::from(format!(
                "/state/lattice/projects/{hex}/workspace-history.git"
            ))
        );
        assert_eq!(
            layout.session_lock("abc.lock"),
            Path::new("/state/lattice/locks/sessions/abc.lock")
        );
        assert_eq!(
            layout.project_lock(&key, "workspace.lock"),
            PathBuf::from(format!(
                "/state/lattice/locks/projects/{hex}/workspace.lock"
            ))
        );
    }
}
