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
//! - [`acquire_flock`] is the **independent operation lock**: an acquisition
//!   made without an existing guard is a new operation, even in this process.
//!   Shared operations may coexist; an exclusive operation conflicts with any
//!   existing operation. Ownership is never inferred from a thread or task identity.
//! - [`acquire_flock_strict`] is the **strict mutex**: first-create locks and
//!   repository write locks must serialize even two independent operations in
//!   one process, so a second acquisition waits on a condvar (blocking) or
//!   returns `Busy` (non-blocking) instead of nesting. Blocking strict waiters
//!   are served **FIFO** by ticket, so a hot writer cannot barge back in ahead
//!   of the peers already queued behind it.
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
    /// Strict FIFO queue: threads waiting on a ticket. The entry is retained
    /// while this is non-zero even with no holder, so the queue survives the
    /// gap between one release and the next acquisition.
    waiters: usize,
    /// Next ticket to hand out. The creating holder implicitly owns ticket 0.
    next_ticket: u64,
    /// The only ticket that may acquire right now.
    serving: u64,
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
    mode: LockMode,
}

impl FlockGuard {
    /// The lock file this guard holds.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The strength of the lock this guard holds.
    pub fn mode(&self) -> LockMode {
        self.mode
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
                // Hand the turn to the next ticket. A queued waiter keeps the
                // entry alive so the ticket counters survive this gap; with no
                // waiter the path returns to fully unregistered.
                entry.serving += 1;
                let notify = Arc::clone(&entry.notify);
                if entry.waiters == 0 {
                    map.remove(&self.path);
                }
                notify.notify_all();
            }
        }
    }
}

/// Acquire an independent operation `flock` on `path`, creating the file (and
/// parent directories) on demand.
///
/// See the module docs for the conflict rules. `Busy` is returned for a
/// non-blocking acquire that conflicts with another operation or process.
/// Blocking acquisitions wait for a conflicting same-process operation to
/// release instead of treating it as an implicit re-entry.
pub fn acquire_flock(
    path: &Path,
    mode: LockMode,
    blocking: Blocking,
) -> Result<FlockGuard, StoreError> {
    let mut map = registry_lock();
    while let Some(entry) = map.get(path) {
        if entry.strict {
            return Err(StoreError::Lock {
                operation: "acquiring an operation lock over a strict lock",
                path: path.to_path_buf(),
                source: std::io::Error::other("lock file used with mixed semantics"),
            });
        }
        // Independent shared operations can coexist. Every other same-process
        // acquisition is a real conflict and must wait or report Busy.
        if entry.mode == LockMode::Shared && mode == LockMode::Shared {
            let entry = map.get_mut(path).expect("entry remains registered");
            entry.holders += 1;
            return Ok(FlockGuard {
                path: path.to_path_buf(),
                mode,
            });
        }
        if blocking == Blocking::NonBlocking {
            return Err(StoreError::Busy {
                path: path.to_path_buf(),
            });
        }
        let notify = Arc::clone(&entry.notify);
        map = notify.wait(map).unwrap_or_else(|e| e.into_inner());
    }
    let file = open_lock_file(path)?;
    let entry = syscall_flock(file, path, mode, blocking, false)?;
    map.insert(path.to_path_buf(), entry);
    Ok(FlockGuard {
        path: path.to_path_buf(),
        mode,
    })
}

/// Acquire a strict exclusive `flock` on `path`: same-process acquisitions
/// serialize (wait / `Busy`) instead of nesting. Used for first-create locks
/// and repository write locks, where two independent operations in one
/// process must never overlap.
///
/// Waiting is **FIFO**. A blocking acquisition takes a ticket and is granted
/// strictly in arrival order, so a thread that just released cannot barge back
/// in ahead of threads already queued behind it. Without this, a condvar
/// broadcast lets whichever thread first wins the registry mutex proceed, and
/// a hot writer starves its peers: #139-A's first qualification measured 313
/// consecutive losses and a 42.69 s worst single acquisition under eight
/// concurrent writers. Cross-process exclusion is still the kernel `flock`;
/// the queue only orders this process's contenders.
///
/// A non-blocking acquisition never joins the queue — it would jump it — so it
/// reports `Busy` whenever the lock is held or anyone is already waiting.
pub fn acquire_flock_strict(path: &Path, blocking: Blocking) -> Result<FlockGuard, StoreError> {
    let mut map = registry_lock();

    if map.contains_key(path) {
        if blocking == Blocking::NonBlocking {
            return Err(StoreError::Busy {
                path: path.to_path_buf(),
            });
        }
        let ticket = {
            let entry = map.get_mut(path).expect("entry present");
            // Marking the entry strict at enqueue time keeps a retained,
            // holder-less entry unusable by `acquire_flock`, which rejects
            // strict paths outright.
            entry.strict = true;
            let ticket = entry.next_ticket;
            entry.next_ticket += 1;
            entry.waiters += 1;
            ticket
        };
        loop {
            let entry = map.get(path).expect("entry retained while waiters > 0");
            if entry.holders == 0 && entry.serving == ticket {
                break;
            }
            let notify = Arc::clone(&entry.notify);
            map = notify.wait(map).unwrap_or_else(|e| e.into_inner());
        }
        let entry = map.get_mut(path).expect("entry retained while waiters > 0");
        entry.waiters -= 1;
        // The registry mutex is held across the syscall, exactly as on the
        // first-acquisition path: only the served ticket reaches it, so this
        // process can never queue behind itself here.
        let rc = unsafe { libc::flock(entry.file.as_raw_fd(), libc::LOCK_EX) };
        if rc != 0 {
            let source = std::io::Error::last_os_error();
            // Never strand the queue on a failed acquisition.
            entry.serving += 1;
            let notify = Arc::clone(&entry.notify);
            if entry.waiters == 0 {
                map.remove(path);
            }
            notify.notify_all();
            return Err(StoreError::Lock {
                operation: "flock",
                path: path.to_path_buf(),
                source,
            });
        }
        entry.holders = 1;
        entry.mode = LockMode::Exclusive;
        return Ok(FlockGuard {
            path: path.to_path_buf(),
            mode: LockMode::Exclusive,
        });
    }

    let file = open_lock_file(path)?;
    let entry = syscall_flock(file, path, LockMode::Exclusive, blocking, true)?;
    map.insert(path.to_path_buf(), entry);
    Ok(FlockGuard {
        path: path.to_path_buf(),
        mode: LockMode::Exclusive,
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
        waiters: 0,
        // The creating holder implicitly owns ticket 0, so the first waiter
        // gets ticket 1 and its release advances `serving` to match.
        next_ticket: 1,
        serving: 0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn independent_exclusive_acquisition_is_busy_and_releases() {
        let dir = tempfile::tempdir().unwrap();
        let lock = dir.path().join("a.lock");
        let g1 = acquire_flock(&lock, LockMode::Exclusive, Blocking::Block).unwrap();
        // An independent acquisition must not be inferred as a nested hold.
        let err = acquire_flock(&lock, LockMode::Shared, Blocking::NonBlocking).unwrap_err();
        assert!(matches!(err, StoreError::Busy { .. }));
        drop(g1);
        // Fully released: shared succeeds again.
        let g3 = acquire_flock(&lock, LockMode::Shared, Blocking::NonBlocking).unwrap();
        drop(g3);
    }

    #[test]
    fn shared_nests_and_upgrade_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let lock = dir.path().join("b.lock");
        let g1 = acquire_flock(&lock, LockMode::Shared, Blocking::Block).unwrap();
        assert_eq!(g1.mode(), LockMode::Shared);
        let g2 = acquire_flock(&lock, LockMode::Shared, Blocking::NonBlocking).unwrap();
        assert_eq!(g2.mode(), LockMode::Shared);
        let err = acquire_flock(&lock, LockMode::Exclusive, Blocking::NonBlocking).unwrap_err();
        assert!(matches!(err, StoreError::Busy { .. }));
        drop(g2);
        drop(g1);
        let g3 = acquire_flock(&lock, LockMode::Exclusive, Blocking::NonBlocking).unwrap();
        drop(g3);
    }

    #[test]
    fn independent_same_process_exclusive_acquisition_cannot_pass() {
        let dir = tempfile::tempdir().unwrap();
        let lock = std::sync::Arc::new(dir.path().join("independent-exclusive.lock"));
        let first = acquire_flock(&lock, LockMode::Exclusive, Blocking::Block).unwrap();
        let second_lock = std::sync::Arc::clone(&lock);
        let result = std::thread::spawn(move || {
            acquire_flock(&second_lock, LockMode::Exclusive, Blocking::NonBlocking)
                .map(|_| ())
                .map_err(|err| matches!(err, StoreError::Busy { .. }))
        })
        .join()
        .unwrap();
        assert_eq!(result, Err(true));
        drop(first);
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
    fn strict_lock_serves_waiters_in_arrival_order() {
        // Barging is the failure this queue exists to prevent: with a plain
        // broadcast, whichever thread first wins the registry mutex proceeds,
        // and a writer that just released can re-take the lock ahead of peers
        // that have been waiting far longer.
        let dir = tempfile::tempdir().unwrap();
        let lock = std::sync::Arc::new(dir.path().join("fifo.lock"));
        let order = std::sync::Arc::new(Mutex::new(Vec::new()));
        let held = acquire_flock_strict(&lock, Blocking::Block).unwrap();

        let mut threads = Vec::new();
        for id in 0..8u32 {
            let lock = std::sync::Arc::clone(&lock);
            let order = std::sync::Arc::clone(&order);
            threads.push(std::thread::spawn(move || {
                let guard = acquire_flock_strict(&lock, Blocking::Block).unwrap();
                order.lock().unwrap().push(id);
                drop(guard);
            }));
            // Let this thread reach the queue before the next one arrives, so
            // arrival order is the spawn order.
            std::thread::sleep(std::time::Duration::from_millis(30));
        }

        drop(held);
        for thread in threads {
            thread.join().unwrap();
        }
        assert_eq!(
            *order.lock().unwrap(),
            (0..8).collect::<Vec<u32>>(),
            "strict waiters must be served in arrival order"
        );
    }

    #[test]
    fn strict_nonblocking_does_not_jump_a_queued_waiter() {
        let dir = tempfile::tempdir().unwrap();
        let lock = std::sync::Arc::new(dir.path().join("fifo-nb.lock"));
        let held = acquire_flock_strict(&lock, Blocking::Block).unwrap();

        let waiter_lock = std::sync::Arc::clone(&lock);
        let waiter = std::thread::spawn(move || {
            let guard = acquire_flock_strict(&waiter_lock, Blocking::Block).unwrap();
            std::thread::sleep(std::time::Duration::from_millis(50));
            drop(guard);
        });
        std::thread::sleep(std::time::Duration::from_millis(50));

        // Released, but the queued waiter owns the next turn.
        drop(held);
        assert!(matches!(
            acquire_flock_strict(&lock, Blocking::NonBlocking),
            Err(StoreError::Busy { .. })
        ));
        waiter.join().unwrap();

        // Queue drained: the path is fully unregistered again.
        let guard = acquire_flock_strict(&lock, Blocking::NonBlocking).unwrap();
        drop(guard);
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
