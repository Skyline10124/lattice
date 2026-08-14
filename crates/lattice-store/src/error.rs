// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Typed kernel errors. Open/create never fall back to init, repair,
//! quarantine, a legacy path, or an empty replacement: every failure is one
//! of these typed values.

use std::path::PathBuf;

/// Failure of a kernel operation.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// Neither `$XDG_STATE_HOME` (absolute) nor `$HOME` is available.
    #[error("Linux XDG state directory unavailable")]
    MissingStateRoot,

    /// A `ProjectStoreKey` display string was not 64 lowercase hex chars.
    #[error("invalid project store key: '{0}'")]
    InvalidKey(String),

    /// The workspace root could not be canonicalized for key derivation.
    #[error("cannot canonicalize workspace root '{}': {source}", .path.display())]
    CanonicalizeRoot {
        /// The path that failed to canonicalize.
        path: PathBuf,
        /// The underlying I/O error.
        source: std::io::Error,
    },

    /// Plain I/O failure with operation context.
    #[error("io error while {operation} '{}': {source}", .path.display())]
    Io {
        /// What was being done.
        operation: &'static str,
        /// The path involved.
        path: PathBuf,
        /// The underlying I/O error.
        source: std::io::Error,
    },

    /// A durability barrier (fsync / atomic rename) failed.
    #[error("durability barrier failed while {operation} '{}': {source}", .path.display())]
    Durability {
        /// Which barrier.
        operation: &'static str,
        /// The path involved.
        path: PathBuf,
        /// The underlying I/O error.
        source: std::io::Error,
    },

    /// A lock syscall failed (other than a `Busy` conflict).
    #[error("lock error while {operation} '{}': {source}", .path.display())]
    Lock {
        /// What was being locked.
        operation: &'static str,
        /// The lock file.
        path: PathBuf,
        /// The underlying I/O error.
        source: std::io::Error,
    },

    /// A shared hold may not be upgraded to exclusive (ADR 0005 §5).
    #[error("lock upgrade forbidden: '{}'", .path.display())]
    LockUpgrade {
        /// The lock file.
        path: PathBuf,
    },

    /// A non-blocking acquire found the lock held.
    #[error("lock busy: '{}'", .path.display())]
    Busy {
        /// The lock file.
        path: PathBuf,
    },

    /// Strict-open target does not exist. Never an invitation to initialize.
    #[error("store not initialized at '{}'", .path.display())]
    NotInitialized {
        /// The expected repository path.
        path: PathBuf,
    },

    /// Existing state failed identity or invariant validation.
    #[error("store corruption: {detail}")]
    Corruption {
        /// What is broken.
        detail: String,
    },

    /// A ref compare-and-set lost the race or its precondition failed.
    #[error("ref CAS conflict on {ref_name}: {detail}")]
    CasConflict {
        /// The ref being updated.
        ref_name: String,
        /// Why it conflicted.
        detail: String,
    },

    /// A required Linux/filesystem primitive is missing at this location.
    /// Always fail-closed.
    #[error("missing required primitive {primitive}: {detail}")]
    MissingPrimitive {
        /// Which primitive (e.g. `renameat2(RENAME_NOREPLACE)`).
        primitive: &'static str,
        /// How the absence manifested.
        detail: String,
    },

    /// Git object/reference mechanics failure.
    #[error("git error: {0}")]
    Git(String),
}
