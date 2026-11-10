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
//! └── locks/
//!     └── sessions/<SessionId>.lock
//! ```
//!
//! 本模块维护状态根目录拓扑；会话库文件名、会话锁文件名与表结构由 `lattice-session` 定义。

use std::path::{Path, PathBuf};

use crate::StoreError;

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
        assert_eq!(
            layout.session_lock("abc.lock"),
            Path::new("/state/lattice/locks/sessions/abc.lock")
        );
    }
}
