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
use std::cell::{Cell, RefCell};
#[cfg(any(test, feature = "test-fault-injection"))]
use std::path::{Path, PathBuf};

#[cfg(any(test, feature = "test-fault-injection"))]
thread_local! {
    static FAIL_BEFORE_RENAME: Cell<bool> = const { Cell::new(false) };
    static FAIL_PARENT_FSYNC: Cell<bool> = const { Cell::new(false) };
    static FAIL_OBJECT_FSYNC: Cell<bool> = const { Cell::new(false) };
    static FAIL_REF_FSYNC: Cell<bool> = const { Cell::new(false) };
    static OBJECT_BARRIER_COUNT: Cell<u32> = const { Cell::new(0) };
    static REF_BARRIER_COUNT: Cell<u32> = const { Cell::new(0) };
    static SYNCED_PATHS: RefCell<Vec<PathBuf>> = const { RefCell::new(Vec::new()) };
    static FAIL_FSYNC_PATH: RefCell<Option<PathBuf>> = const { RefCell::new(None) };
    static CREATED_REPOS: RefCell<Vec<PathBuf>> = const { RefCell::new(Vec::new()) };
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

/// Fail the next object durability barrier before a ref can be published.
#[cfg(any(test, feature = "test-fault-injection"))]
pub fn fail_object_fsync(on: bool) {
    FAIL_OBJECT_FSYNC.with(|flag| flag.set(on));
}

/// Fail the next ref durability barrier after the ref edit has been published.
#[cfg(any(test, feature = "test-fault-injection"))]
pub fn fail_ref_fsync(on: bool) {
    FAIL_REF_FSYNC.with(|flag| flag.set(on));
}

/// Reset the per-thread object/ref barrier counters used by structural tests.
#[cfg(any(test, feature = "test-fault-injection"))]
pub fn reset_barrier_counts() {
    OBJECT_BARRIER_COUNT.with(|count| count.set(0));
    REF_BARRIER_COUNT.with(|count| count.set(0));
}

/// How many object durability barriers this thread has entered since reset.
#[cfg(any(test, feature = "test-fault-injection"))]
pub fn object_barrier_count() -> u32 {
    OBJECT_BARRIER_COUNT.with(|count| count.get())
}

/// How many ref durability barriers this thread has entered since reset.
#[cfg(any(test, feature = "test-fault-injection"))]
pub fn ref_barrier_count() -> u32 {
    REF_BARRIER_COUNT.with(|count| count.get())
}

/// Clear recorded fsync paths and any pending path-specific injection.
#[cfg(any(test, feature = "test-fault-injection"))]
pub fn reset_fsync_trace() {
    SYNCED_PATHS.with(|paths| paths.borrow_mut().clear());
    FAIL_FSYNC_PATH.with(|path| *path.borrow_mut() = None);
}

/// Fail the next `fsync_file` / `fsync_dir` whose path equals `path`.
///
/// This is the narrow seam that distinguishes `objects/` from `objects/xx`
/// and object files from directories. It is not a pluggable filesystem.
#[cfg(any(test, feature = "test-fault-injection"))]
pub fn fail_fsync_path(path: impl AsRef<Path>) {
    FAIL_FSYNC_PATH.with(|slot| *slot.borrow_mut() = Some(path.as_ref().to_path_buf()));
}

/// Paths this thread has attempted to `fsync_file` or `fsync_dir` since the
/// last [`reset_fsync_trace`].
#[cfg(any(test, feature = "test-fault-injection"))]
pub fn synced_paths() -> Vec<PathBuf> {
    SYNCED_PATHS.with(|paths| paths.borrow().clone())
}

/// Clear recorded first-create publications.
#[cfg(any(test, feature = "test-fault-injection"))]
pub fn reset_repo_create_trace() {
    CREATED_REPOS.with(|paths| paths.borrow_mut().clear());
}

/// Published repository paths this thread first-created since the last
/// [`reset_repo_create_trace`]. The existing-store `create` reuse path is
/// not recorded.
#[cfg(any(test, feature = "test-fault-injection"))]
pub fn created_repo_paths() -> Vec<PathBuf> {
    CREATED_REPOS.with(|paths| paths.borrow().clone())
}

/// Record a file/directory fsync and optionally fail it.
pub(crate) fn on_fsync(path: &std::path::Path) -> Result<(), crate::StoreError> {
    #[cfg(any(test, feature = "test-fault-injection"))]
    {
        SYNCED_PATHS.with(|paths| paths.borrow_mut().push(path.to_path_buf()));
        let inject = FAIL_FSYNC_PATH.with(|slot| {
            let pending = slot.borrow();
            pending.as_ref().is_some_and(|want| want == path)
        });
        if inject {
            FAIL_FSYNC_PATH.with(|slot| *slot.borrow_mut() = None);
            return Err(crate::StoreError::Durability {
                operation: "injected fsync failure",
                path: path.to_path_buf(),
                source: std::io::Error::other("injected"),
            });
        }
    }
    let _ = path;
    Ok(())
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

/// Record a successful first-create publication. Called after rename
/// visibility and the parent fsync so the tracer cannot skip a durability
/// barrier. Existing-store reuse does not call this.
pub(crate) fn on_repo_first_create(path: &std::path::Path) {
    #[cfg(any(test, feature = "test-fault-injection"))]
    {
        CREATED_REPOS.with(|paths| paths.borrow_mut().push(path.to_path_buf()));
    }
    let _ = path;
}

/// Injection point before fsyncing newly-written objects.
pub(crate) fn object_fsync(path: &std::path::Path) -> Result<(), crate::StoreError> {
    #[cfg(any(test, feature = "test-fault-injection"))]
    {
        OBJECT_BARRIER_COUNT.with(|count| count.set(count.get() + 1));
        let inject = FAIL_OBJECT_FSYNC.with(|flag| {
            let on = flag.get();
            flag.set(false);
            on
        });
        if inject {
            return Err(crate::StoreError::Durability {
                operation: "injected object-fsync failure",
                path: path.to_path_buf(),
                source: std::io::Error::other("injected"),
            });
        }
    }
    let _ = path;
    Ok(())
}

/// Injection point before fsyncing newly-written refs.
pub(crate) fn ref_fsync(path: &std::path::Path) -> Result<(), crate::StoreError> {
    #[cfg(any(test, feature = "test-fault-injection"))]
    {
        REF_BARRIER_COUNT.with(|count| count.set(count.get() + 1));
        let inject = FAIL_REF_FSYNC.with(|flag| {
            let on = flag.get();
            flag.set(false);
            on
        });
        if inject {
            return Err(crate::StoreError::Durability {
                operation: "injected ref-fsync failure",
                path: path.to_path_buf(),
                source: std::io::Error::other("injected"),
            });
        }
    }
    let _ = path;
    Ok(())
}
