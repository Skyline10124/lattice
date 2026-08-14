// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Linux `flock` primitives with a per-process registry (ADR 0005 §5).
//!
//! Repository write locks and both operation locks (`SessionExecutionLock`,
//! `WorkspaceOperationLock`) are ordinary `flock(2)` locks on files under the
//! XDG state layout. There is no PID file, heartbeat, timeout, mtime, lock
//! steal, or stale-owner protocol: process exit releases the lock in the
//! kernel.
//!
//! ## Two same-process semantics
//!
//! `flock` conflicts with itself across two open file descriptions *even in
//! one process*, so the registry mediates every acquisition:
//!
//! - [`acquire_flock`] is the **re-entrant operation lock**: real call paths
//!   nest (a full-Turn exclusive hold re-enters the same session's lock when
//!   the Turn commits). Same-mode nesting under an exclusive entry just
//!   increments a counter; a *non-blocking shared* request under an exclusive
//!   entry reports `Busy` so a coherent status read honestly reports the
//!   exclusive owner; an exclusive request over a shared entry is a typed
//!   `LockUpgrade` error (ADR 0005 forbids upgrading a shared lock).
//! - [`acquire_flock_strict`] is the **strict mutex**: first-create locks and
//!   repository write locks must serialize even two independent operations in
//!   one process, so a second acquisition waits on a condvar (blocking) or
//!   returns `Busy` (non-blocking) instead of nesting.
//!
//! In both modes the OS lock is taken once per process hold and released when
//! the last guard drops. The registry is keyed by the lock path as
//! constructed by [`crate::StateLayout`]; callers must not address one lock
//! file through multiple spellings (e.g. a symlinked state root).

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock};

use crate::StoreError;

/// Lock strength.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockMode {
    /// Observation / coherent read.
    Shared,
    /// Mutation.
    Exclusive,
}

/// Whether to wait for a conflicting hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Blocking {
    /// Block until the lock is grantable.
    Block,
    /// Return [`StoreError::Busy`] instead of waiting.
    NonBlocking,
}

struct LockEntry {
    file: File,
    mode: LockMode,
    holders: usize,
    strict: bool,
    notify: Arc<Condvar>,
}

fn registry() -> &'static Mutex<HashMap<PathBuf, LockEntry>> {
    static REGISTRY: OnceLock<Mutex<HashMap<PathBuf, LockEntry>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

fn registry_lock() -> MutexGuard<'static, HashMap<PathBuf, LockEntry>> {
    registry().lock().unwrap_or_else(|e| e.into_inner())
}

/// RAII flock guard. Releases the process-wide hold on drop; the final drop
/// unlocks the kernel lock and wakes any strict waiters.
#[derive(Debug)]
pub struct FlockGuard {
    path: PathBuf,
}

impl FlockGuard {
    /// The lock file this guard holds.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for FlockGuard {
    fn drop(&mut self) {
        let mut map = registry_lock();
        if let Some(entry) = map.get_mut(&self.path) {
            entry.holders -= 1;
            if entry.holders == 0 {
                // Best effort: process exit is the final release mechanism
                // anyway, and there is nothing useful to do with an unlock
                // error on the drop path.
                unsafe {
                    libc::flock(entry.file.as_raw_fd(), libc::LOCK_UN);
                }
                let entry = map.remove(&self.path).expect("entry present");
                entry.notify.notify_all();
            }
        }
    }
}

/// Acquire a re-entrant operation `flock` on `path`, creating the file (and
/// parent directories) on demand.
///
/// See the module docs for the nesting matrix. `Busy` is returned for a
/// non-blocking acquire that conflicts with another process, or for a
/// non-blocking shared request while this process holds the lock exclusively.
/// `LockUpgrade` is returned when an exclusive hold is requested over this
/// process's shared hold.
pub fn acquire_flock(
    path: &Path,
    mode: LockMode,
    blocking: Blocking,
) -> Result<FlockGuard, StoreError> {
    let mut map = registry_lock();
    if let Some(entry) = map.get_mut(path) {
        if entry.strict {
            return Err(StoreError::Lock {
                operation: "nesting a strict lock",
                path: path.to_path_buf(),
                source: std::io::Error::other("lock file used with mixed semantics"),
            });
        }
        return match (entry.mode, mode, blocking) {
            (LockMode::Shared, LockMode::Exclusive, _) => Err(StoreError::LockUpgrade {
                path: path.to_path_buf(),
            }),
            (LockMode::Exclusive, LockMode::Shared, Blocking::NonBlocking) => {
                Err(StoreError::Busy {
                    path: path.to_path_buf(),
                })
            }
            _ => {
                entry.holders += 1;
                Ok(FlockGuard {
                    path: path.to_path_buf(),
                })
            }
        };
    }
    let file = open_lock_file(path)?;
    let entry = syscall_flock(file, path, mode, blocking, false)?;
    map.insert(path.to_path_buf(), entry);
    Ok(FlockGuard {
        path: path.to_path_buf(),
    })
}

/// Acquire a strict exclusive `flock` on `path`: same-process acquisitions
/// serialize (wait / `Busy`) instead of nesting. Used for first-create locks
/// and repository write locks, where two independent operations in one
/// process must never overlap.
pub fn acquire_flock_strict(path: &Path, blocking: Blocking) -> Result<FlockGuard, StoreError> {
    let mut map = registry_lock();
    while let Some(entry) = map.get(path) {
        if blocking == Blocking::NonBlocking {
            return Err(StoreError::Busy {
                path: path.to_path_buf(),
            });
        }
        let notify = Arc::clone(&entry.notify);
        map = notify.wait(map).unwrap_or_else(|e| e.into_inner());
    }
    let file = open_lock_file(path)?;
    let entry = syscall_flock(file, path, LockMode::Exclusive, blocking, true)?;
    map.insert(path.to_path_buf(), entry);
    Ok(FlockGuard {
        path: path.to_path_buf(),
    })
}

fn open_lock_file(path: &Path) -> Result<File, StoreError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|source| StoreError::Io {
            operation: "creating lock directory",
            path: parent.to_path_buf(),
            source,
        })?;
    }
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .map_err(|source| StoreError::Io {
            operation: "opening lock file",
            path: path.to_path_buf(),
            source,
        })
}

fn syscall_flock(
    file: File,
    path: &Path,
    mode: LockMode,
    blocking: Blocking,
    strict: bool,
) -> Result<LockEntry, StoreError> {
    let mut flags = match mode {
        LockMode::Shared => libc::LOCK_SH,
        LockMode::Exclusive => libc::LOCK_EX,
    };
    if blocking == Blocking::NonBlocking {
        flags |= libc::LOCK_NB;
    }
    // The registry mutex is held across the syscall. A blocking flock here
    // can only wait on *other* processes: same-process holds returned (or
    // waited out) above, so this process can never queue behind itself.
    let rc = unsafe { libc::flock(file.as_raw_fd(), flags) };
    if rc != 0 {
        let err = std::io::Error::last_os_error();
        if blocking == Blocking::NonBlocking && err.kind() == std::io::ErrorKind::WouldBlock {
            return Err(StoreError::Busy {
                path: path.to_path_buf(),
            });
        }
        return Err(StoreError::Lock {
            operation: "flock",
            path: path.to_path_buf(),
            source: err,
        });
    }
    Ok(LockEntry {
        file,
        mode,
        holders: 1,
        strict,
        notify: Arc::new(Condvar::new()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exclusive_nests_and_releases() {
        let dir = tempfile::tempdir().unwrap();
        let lock = dir.path().join("a.lock");
        let g1 = acquire_flock(&lock, LockMode::Exclusive, Blocking::Block).unwrap();
        let g2 = acquire_flock(&lock, LockMode::Exclusive, Blocking::Block).unwrap();
        drop(g1);
        // Still held by g2: a non-blocking *shared* request must see Busy.
        let err = acquire_flock(&lock, LockMode::Shared, Blocking::NonBlocking).unwrap_err();
        assert!(matches!(err, StoreError::Busy { .. }));
        drop(g2);
        // Fully released: shared succeeds again.
        let g3 = acquire_flock(&lock, LockMode::Shared, Blocking::NonBlocking).unwrap();
        drop(g3);
    }

    #[test]
    fn shared_nests_and_upgrade_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let lock = dir.path().join("b.lock");
        let g1 = acquire_flock(&lock, LockMode::Shared, Blocking::Block).unwrap();
        let g2 = acquire_flock(&lock, LockMode::Shared, Blocking::NonBlocking).unwrap();
        let err = acquire_flock(&lock, LockMode::Exclusive, Blocking::Block).unwrap_err();
        assert!(matches!(err, StoreError::LockUpgrade { .. }));
        drop(g2);
        drop(g1);
        let g3 = acquire_flock(&lock, LockMode::Exclusive, Blocking::NonBlocking).unwrap();
        drop(g3);
    }

    #[test]
    fn shared_under_exclusive_blocking_nests() {
        let dir = tempfile::tempdir().unwrap();
        let lock = dir.path().join("c.lock");
        let g1 = acquire_flock(&lock, LockMode::Exclusive, Blocking::Block).unwrap();
        let g2 = acquire_flock(&lock, LockMode::Shared, Blocking::Block).unwrap();
        drop(g2);
        drop(g1);
    }

    #[test]
    fn strict_lock_serializes_same_process_threads() {
        let dir = tempfile::tempdir().unwrap();
        let lock = std::sync::Arc::new(dir.path().join("strict.lock"));
        let order = std::sync::Arc::new(Mutex::new(Vec::new()));

        let g1 = acquire_flock_strict(&lock, Blocking::Block).unwrap();
        let lock2 = std::sync::Arc::clone(&lock);
        let order2 = std::sync::Arc::clone(&order);
        let t = std::thread::spawn(move || {
            // Must wait until g1 drops; nesting would let this through
            // immediately.
            let g = acquire_flock_strict(&lock2, Blocking::Block).unwrap();
            order2.lock().unwrap().push("thread");
            drop(g);
        });
        std::thread::sleep(std::time::Duration::from_millis(100));
        order.lock().unwrap().push("main");
        drop(g1);
        t.join().unwrap();
        assert_eq!(
            *order.lock().unwrap(),
            vec!["main", "thread"],
            "strict lock must serialize same-process threads"
        );
    }

    #[test]
    fn strict_lock_nonblocking_reports_busy_same_process() {
        let dir = tempfile::tempdir().unwrap();
        let lock = dir.path().join("strict-nb.lock");
        let g1 = acquire_flock_strict(&lock, Blocking::Block).unwrap();
        let err = acquire_flock_strict(&lock, Blocking::NonBlocking).unwrap_err();
        assert!(matches!(err, StoreError::Busy { .. }));
        drop(g1);
        let g2 = acquire_flock_strict(&lock, Blocking::NonBlocking).unwrap();
        drop(g2);
    }

    #[test]
    fn nesting_a_strict_lock_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let lock = dir.path().join("mixed.lock");
        let g1 = acquire_flock_strict(&lock, Blocking::Block).unwrap();
        let err = acquire_flock(&lock, LockMode::Exclusive, Blocking::Block).unwrap_err();
        assert!(matches!(err, StoreError::Lock { .. }));
        drop(g1);
    }
}
