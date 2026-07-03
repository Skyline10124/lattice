// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Serialised environment helpers for tests that touch `XDG_STATE_HOME`.
//!
//! **Not shared with `lattice-cli`:** the CLI binary keeps a separate copy under
//! `lattice-cli/src/commands/test_env.rs` (`#[cfg(test)]` only) because crate
//! boundaries cannot reuse this module. Keep the two implementations in sync when
//! changing mutex/`unsafe` env semantics. A future `test-utils` / dev-dependency
//! crate would be the place to deduplicate if more crates need the same guard.

use std::sync::{LazyLock, Mutex, MutexGuard};

use tempfile::TempDir;

static ENV_MUTEX: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

fn set_env_var(key: &str, value: Option<&str>) {
    // SAFETY: All mutations go through ENV_MUTEX; restored before the lock drops.
    unsafe {
        match value {
            Some(value) => std::env::set_var(key, value),
            None => std::env::remove_var(key),
        }
    }
}

/// Hold the global env lock with an isolated `XDG_STATE_HOME` temp directory.
pub struct ScopedXdgStateHome {
    _lock: MutexGuard<'static, ()>,
    _state: TempDir,
    prev_xdg: Option<String>,
}

impl ScopedXdgStateHome {
    pub(crate) fn new() -> Self {
        let guard = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let prev_xdg = std::env::var("XDG_STATE_HOME").ok();
        let state = tempfile::tempdir().expect("xdg state tempdir");
        set_env_var(
            "XDG_STATE_HOME",
            Some(state.path().to_str().expect("utf8 xdg state path")),
        );
        Self {
            _lock: guard,
            _state: state,
            prev_xdg,
        }
    }
}

impl Drop for ScopedXdgStateHome {
    fn drop(&mut self) {
        set_env_var("XDG_STATE_HOME", self.prev_xdg.as_deref());
    }
}