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

    /// A non-blocking acquire found the lock held.
    #[error("lock busy: '{}'", .path.display())]
    Busy {
        /// The lock file.
        path: PathBuf,
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
}
