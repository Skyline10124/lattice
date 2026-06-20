// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Shared test helpers for environment variable isolation.
//!
//! Only compiled in test mode. Provides a serialised `with_env_var`
//! that saves and restores the environment, so tests that manipulate
//! env vars can run concurrently without clobbering each other.

use std::path::Path;
use std::sync::{LazyLock, Mutex, MutexGuard};
use tempfile::TempDir;

static ENV_MUTEX: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

fn set_env_var(key: &str, value: Option<&str>) {
    // SAFETY: Every environment mutation in this test binary uses ENV_MUTEX,
    // and each mutated key is only observed inside the protected closure.
    unsafe {
        match value {
            Some(value) => std::env::set_var(key, value),
            None => std::env::remove_var(key),
        }
    }
}

/// Call `f` with `key` set to `value` in the environment.
///
/// The previous value of `key` is restored (or removed if it was unset)
/// after `f` returns.  A global mutex serialises all `with_env_var`
/// calls so that environment-aware tests can run in parallel suites.
pub(crate) fn with_env_var(key: &str, value: &str, f: impl FnOnce()) {
    let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    let prev = std::env::var(key).ok();
    set_env_var(key, Some(value));
    f();
    set_env_var(key, prev.as_deref());
}

/// Call `f` with the given env vars removed from the environment.
///
/// Previous values are restored after `f` returns.
pub(crate) fn without_env_vars(keys: &[&str], f: impl FnOnce()) {
    let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());

    let previous: Vec<(&str, Option<String>)> = keys
        .iter()
        .map(|key| (*key, std::env::var(key).ok()))
        .collect();

    for key in keys {
        set_env_var(key, None);
    }

    f();

    for (key, value) in previous {
        set_env_var(key, value.as_deref());
    }
}

/// Hold the global env lock with an isolated `XDG_STATE_HOME` temp directory.
///
/// Audit/status CLI tests seed and read the external project-keyed audit store
/// through `audit_db_path_for_project`, which resolves via `XDG_STATE_HOME`.
pub(crate) struct ScopedXdgStateHome {
    _lock: MutexGuard<'static, ()>,
    _state: TempDir,
}

impl ScopedXdgStateHome {
    pub(crate) fn new() -> Self {
        let guard = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let state = tempfile::tempdir().expect("xdg state tempdir");
        set_env_var(
            "XDG_STATE_HOME",
            Some(state.path().to_str().expect("utf8 xdg state path")),
        );
        Self {
            _lock: guard,
            _state: state,
        }
    }

    pub(crate) fn path(&self) -> &Path {
        self._state.path()
    }
}
