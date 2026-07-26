// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Single workspace-wide XDG/env test isolation helper.
//!
//! This module is THE one shared implementation of serialised
//! `XDG_STATE_HOME` isolation for tests; `lattice-api` and `lattice-cli`
//! tests consume it through this crate's `test-support` feature instead of
//! keeping per-crate copies. Any change to the mutex/`unsafe` env semantics
//! happens here, once.
//!
//! Compiled only for this crate's own tests (`cfg(test)`) or when the
//! `test-support` feature is enabled by a downstream dev-dependency.

use std::path::Path;
use std::sync::{LazyLock, Mutex, MutexGuard};

use tempfile::TempDir;

/// Process-global lock serialising every environment mutation in a test binary.
///
/// Crate-internal tests that set env vars manually (rather than through
/// [`ScopedXdgStateHome`]) must hold this lock for the full mutate/observe/
/// restore window.
pub(crate) static ENV_MUTEX: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

/// Set or remove an environment variable under [`ENV_MUTEX`].
pub(crate) fn set_env_var(key: &str, value: Option<&str>) {
    // SAFETY: All mutations go through ENV_MUTEX; restored before the lock drops.
    unsafe {
        match value {
            Some(value) => std::env::set_var(key, value),
            None => std::env::remove_var(key),
        }
    }
}

/// Hold the global env lock with an isolated `XDG_STATE_HOME` temp directory.
///
/// Audit/status tests seed and read the external project-keyed audit store
/// through `audit_db_path_for_project`, which resolves via `XDG_STATE_HOME`;
/// pointing it at a fresh [`TempDir`] keeps them off the real user state
/// directory. The previous `XDG_STATE_HOME` value is restored on drop, before
/// the lock is released.
pub struct ScopedXdgStateHome {
    _lock: MutexGuard<'static, ()>,
    state: TempDir,
    prev_xdg: Option<String>,
}

impl ScopedXdgStateHome {
    pub fn new() -> Self {
        let guard = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let prev_xdg = std::env::var("XDG_STATE_HOME").ok();
        let state = tempfile::tempdir().expect("xdg state tempdir");
        set_env_var(
            "XDG_STATE_HOME",
            Some(state.path().to_str().expect("utf8 xdg state path")),
        );
        Self {
            _lock: guard,
            state,
            prev_xdg,
        }
    }

    /// The temp state root `XDG_STATE_HOME` currently points at.
    ///
    /// Tests that seed the external audit store directly (via
    /// `audit_db_path_for_project_in`) resolve paths under this root.
    pub fn path(&self) -> &Path {
        self.state.path()
    }
}

impl Default for ScopedXdgStateHome {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for ScopedXdgStateHome {
    fn drop(&mut self) {
        set_env_var("XDG_STATE_HOME", self.prev_xdg.as_deref());
    }
}
