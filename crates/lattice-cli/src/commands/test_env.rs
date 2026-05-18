//! Shared test helpers for environment variable isolation.
//!
//! Only compiled in test mode. Provides a serialised `with_env_var`
//! that saves and restores the environment, so tests that manipulate
//! env vars can run concurrently without clobbering each other.

use std::sync::{LazyLock, Mutex};

static ENV_MUTEX: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

/// Call `f` with `key` set to `value` in the environment.
///
/// The previous value of `key` is restored (or removed if it was unset)
/// after `f` returns.  A global mutex serialises all `with_env_var`
/// calls so that environment-aware tests can run in parallel suites.
pub(crate) fn with_env_var(key: &str, value: &str, f: impl FnOnce()) {
    let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    let prev = std::env::var(key).ok();
    std::env::set_var(key, value);
    f();
    match prev {
        Some(v) => std::env::set_var(key, v),
        None => std::env::remove_var(key),
    }
}
