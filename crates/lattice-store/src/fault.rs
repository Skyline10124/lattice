// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Fault-injection hooks for first-create crash/durability tests.
//!
//! Compiled to no-ops in production builds; the setters exist only under
//! `cfg(test)` or the `test-fault-injection` feature. The flags are
//! thread-local so parallel tests never consume each other's injection.
//! Injected failures demonstrate call ordering and error classification —
//! they are not claimed to reproduce real hardware power loss (ADR 0005 §18).

#[cfg(any(test, feature = "test-fault-injection"))]
use std::cell::Cell;

#[cfg(any(test, feature = "test-fault-injection"))]
thread_local! {
    static FAIL_BEFORE_RENAME: Cell<bool> = const { Cell::new(false) };
    static FAIL_PARENT_FSYNC: Cell<bool> = const { Cell::new(false) };
}

/// Fail the next first-create on this thread just before the publication
/// rename.
#[cfg(any(test, feature = "test-fault-injection"))]
pub fn fail_before_rename(on: bool) {
    FAIL_BEFORE_RENAME.with(|flag| flag.set(on));
}

/// Fail the next first-create on this thread at the parent fsync after the
/// rename (the rename has already published the repository).
#[cfg(any(test, feature = "test-fault-injection"))]
pub fn fail_parent_fsync(on: bool) {
    FAIL_PARENT_FSYNC.with(|flag| flag.set(on));
}

/// Injection point: before the `renameat2` publication.
pub(crate) fn before_rename(path: &std::path::Path) -> Result<(), crate::StoreError> {
    #[cfg(any(test, feature = "test-fault-injection"))]
    {
        let inject = FAIL_BEFORE_RENAME.with(|flag| {
            let on = flag.get();
            flag.set(false);
            on
        });
        if inject {
            return Err(crate::StoreError::Durability {
                operation: "injected pre-rename failure",
                path: path.to_path_buf(),
                source: std::io::Error::other("injected"),
            });
        }
    }
    let _ = path;
    Ok(())
}

/// Injection point: at the parent fsync after the publication rename.
pub(crate) fn parent_fsync(path: &std::path::Path) -> Result<(), crate::StoreError> {
    #[cfg(any(test, feature = "test-fault-injection"))]
    {
        let inject = FAIL_PARENT_FSYNC.with(|flag| {
            let on = flag.get();
            flag.set(false);
            on
        });
        if inject {
            return Err(crate::StoreError::Durability {
                operation: "injected parent-fsync failure",
                path: path.to_path_buf(),
                source: std::io::Error::other("injected"),
            });
        }
    }
    crate::durability::fsync_dir(path)
}
