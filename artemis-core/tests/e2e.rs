//! End-to-end integration tests for the model-centric architecture.
//!
//! Each test exercises a full pipeline scenario:
//!   resolve model → get provider → call with ResolvedModel → process response
//!
//! All tests use pure Rust types — no Python runtime required.

/// Global mutex for env var isolation across all e2e tests.
/// Any test that sets/removes env vars MUST acquire this lock first
/// to prevent race conditions with concurrent tests in the same binary.
pub mod env_lock {
    use std::sync::{LazyLock, Mutex};

    static GLOBAL_ENV_MUTEX: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

    pub fn lock() -> std::sync::MutexGuard<'static, ()> {
        GLOBAL_ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner())
    }
}

#[path = "e2e/unknown_model.rs"]
mod unknown_model;

#[path = "e2e/credential_resolution_characterization.rs"]
mod credential_resolution_characterization;

#[path = "e2e/error_classification_characterization.rs"]
mod error_classification_characterization;

#[path = "e2e/regression_wave4_5.rs"]
mod regression_wave4_5;

#[path = "e2e/regression_wave1.rs"]
mod regression_wave1;
