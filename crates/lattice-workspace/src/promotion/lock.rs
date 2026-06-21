// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Exclusive Lower lock with PID-reuse-safe holder identity.

use std::collections::{HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::lower::PromotionError;

/// On-disk lock metadata written atomically at acquisition time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LockMetadata {
    pub lock_id: String,
    pub pid: u32,
    pub boot_id: String,
    pub process_start_ticks: u64,
    pub acquired_unix_ms: u64,
}

/// Process identity tuple used to detect PID reuse.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LockOwnerIdentity {
    pub pid: u32,
    pub boot_id: String,
    pub process_start_ticks: u64,
}

/// Outcome of stale-lock inspection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StaleLockDisposition {
    Live,
    Stale { metadata: LockMetadata },
    Malformed,
    IdentityUnavailable,
}

/// Injectable hook for reading live process identity (production uses `/proc`).
pub trait ProcessIdentityReader: Send + Sync {
    fn read_boot_id(&self) -> Result<String, PromotionError>;
    fn read_process_start_ticks(&self, pid: u32) -> Result<u64, PromotionError>;
    fn process_alive(&self, pid: u32) -> bool;
}

/// Records parent-directory fsync calls for durability tests.
pub trait DurabilityRecorder: Send + Sync {
    fn record_parent_fsync(&self, path: &Path);
}

#[allow(dead_code)]
#[derive(Default)]
struct NoopDurability;

impl DurabilityRecorder for NoopDurability {
    fn record_parent_fsync(&self, _path: &Path) {}
}

/// Production `/proc`-backed identity reader.
#[derive(Debug, Default, Clone, Copy)]
pub struct ProcIdentityReader;

impl ProcessIdentityReader for ProcIdentityReader {
    fn read_boot_id(&self) -> Result<String, PromotionError> {
        fs::read_to_string("/proc/sys/kernel/random/boot_id").map(|s| s.trim().to_owned()).map_err(PromotionError::Io)
    }

    fn read_process_start_ticks(&self, pid: u32) -> Result<u64, PromotionError> {
        let stat = fs::read_to_string(format!("/proc/{pid}/stat"))?;
        parse_proc_start_ticks(&stat).ok_or_else(|| PromotionError::GitCommand {
            context: "parse /proc stat starttime".into(),
            stderr: format!("pid {pid}"),
        })
    }

    fn process_alive(&self, pid: u32) -> bool {
        Path::new(&format!("/proc/{pid}")).exists()
    }
}

fn parse_proc_start_ticks(stat: &str) -> Option<u64> {
    let close = stat.rfind(')')?;
    let rest = stat[close + 2..].split_whitespace().collect::<Vec<_>>();
    rest.get(19).and_then(|s| s.parse().ok())
}

/// RAII guard holding an exclusive Lower promotion lock.
pub struct LowerLockGuard {
    lock_path: PathBuf,
    metadata: LockMetadata,
    durability: Option<Arc<dyn DurabilityRecorder>>,
    #[allow(dead_code)]
    file: File,
}

impl LowerLockGuard {
    /// Acquire the exclusive lock at `lock_path`, creating parent directories.
    pub fn acquire(
        lock_path: &Path,
        identity_reader: &dyn ProcessIdentityReader,
        durability: Option<Arc<dyn DurabilityRecorder>>,
    ) -> Result<Self, PromotionError> {
        if let Some(parent) = lock_path.parent() {
            fs::create_dir_all(parent)?;
        }

        let boot_id = identity_reader.read_boot_id()?;
        let pid = std::process::id();
        let process_start_ticks = identity_reader.read_process_start_ticks(pid)?;

        let metadata = LockMetadata {
            lock_id: Uuid::new_v4().to_string(),
            pid,
            boot_id,
            process_start_ticks,
            acquired_unix_ms: unix_ms_now(),
        };

        let durability_handle = durability;

        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(lock_path)?;

        let json = serde_json::to_string(&metadata).map_err(|e| PromotionError::GitCommand {
            context: "serialize lock metadata".into(),
            stderr: e.to_string(),
        })?;
        file.write_all(json.as_bytes())?;
        file.sync_all()?;
        fsync_parent(lock_path, durability_handle.as_deref());

        Ok(Self {
            lock_path: lock_path.to_path_buf(),
            metadata,
            durability: durability_handle,
            file,
        })
    }

    /// Read and classify an existing lock without breaking it.
    pub fn inspect_existing(
        lock_path: &Path,
        identity_reader: &dyn ProcessIdentityReader,
    ) -> Result<Option<StaleLockDisposition>, PromotionError> {
        if !lock_path.exists() {
            return Ok(None);
        }
        let metadata = match read_lock_metadata(lock_path) {
            Ok(meta) => meta,
            Err(PromotionError::GitCommand { .. }) => return Ok(Some(StaleLockDisposition::Malformed)),
            Err(other) => return Err(other),
        };

        let live = holder_is_live(&metadata, identity_reader)?;
        Ok(Some(if live {
            StaleLockDisposition::Live
        } else {
            StaleLockDisposition::Stale { metadata }
        }))
    }

    pub fn metadata(&self) -> &LockMetadata {
        &self.metadata
    }
}

impl Drop for LowerLockGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.lock_path);
        fsync_parent(&self.lock_path, self.durability.as_deref());
    }
}

/// Recover a stale lock after journal replay succeeds.
pub fn recover_stale_lock(
    lock_path: &Path,
    expected_lock_id: &str,
    identity_reader: &dyn ProcessIdentityReader,
    durability: Option<&dyn DurabilityRecorder>,
    recover: impl FnOnce() -> Result<(), PromotionError>,
) -> Result<(), PromotionError> {
    let recovery_guard_path = lock_path.with_extension("recovery");
    if recovery_guard_path.exists() {
        return Err(PromotionError::GitCommand {
            context: "recovery guard exists".into(),
            stderr: recovery_guard_path.display().to_string(),
        });
    }

    let metadata = read_lock_metadata(lock_path)?;

    if metadata.lock_id != expected_lock_id {
        return Err(PromotionError::GitCommand {
            context: "lock_id mismatch".into(),
            stderr: format!("expected {expected_lock_id}, got {}", metadata.lock_id),
        });
    }

    if holder_is_live(&metadata, identity_reader)? {
        return Err(PromotionError::GitCommand {
            context: "lock holder still live".into(),
            stderr: metadata.lock_id.clone(),
        });
    }

    let _recovery = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&recovery_guard_path)?;

    if let Err(err) = recover() {
        let _ = fs::remove_file(&recovery_guard_path);
        return Err(err);
    }

    fs::remove_file(lock_path)?;
    fsync_parent(lock_path, durability);
    let _ = fs::remove_file(recovery_guard_path);
    Ok(())
}

fn read_lock_metadata(lock_path: &Path) -> Result<LockMetadata, PromotionError> {
    let mut file = File::open(lock_path)?;
    let mut buf = String::new();
    file.read_to_string(&mut buf)?;
    serde_json::from_str(&buf).map_err(|e| PromotionError::GitCommand {
        context: "parse lock metadata".into(),
        stderr: e.to_string(),
    })
}

fn holder_is_live(
    metadata: &LockMetadata,
    identity_reader: &dyn ProcessIdentityReader,
) -> Result<bool, PromotionError> {
    if !identity_reader.process_alive(metadata.pid) {
        return Ok(false);
    }
    let boot_id = identity_reader.read_boot_id()?;
    let start_ticks = identity_reader.read_process_start_ticks(metadata.pid)?;
    if boot_id != metadata.boot_id || start_ticks != metadata.process_start_ticks {
        return Ok(false);
    }
    Ok(true)
}

fn fsync_parent(path: &Path, durability: Option<&dyn DurabilityRecorder>) {
    if let Some(parent) = path.parent()
        && let Ok(dir) = File::open(parent)
    {
        let _ = dir.sync_all();
        if let Some(rec) = durability {
            rec.record_parent_fsync(parent);
        }
    }
}

fn unix_ms_now() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// In-memory identity reader for tests.
#[derive(Clone)]
pub struct TestIdentityReader {
    boot_id: String,
    start_ticks_by_pid: Arc<Mutex<HashMap<u32, u64>>>,
    alive: Arc<Mutex<HashSet<u32>>>,
}

impl TestIdentityReader {
    pub fn new(boot_id: impl Into<String>) -> Self {
        Self {
            boot_id: boot_id.into(),
            start_ticks_by_pid: Arc::new(Mutex::new(HashMap::new())),
            alive: Arc::new(Mutex::new(HashSet::new())),
        }
    }

    pub fn register_process(&self, pid: u32, start_ticks: u64) {
        self.start_ticks_by_pid.lock().unwrap().insert(pid, start_ticks);
        self.alive.lock().unwrap().insert(pid);
    }

    pub fn kill_process(&self, pid: u32) {
        self.alive.lock().unwrap().remove(&pid);
    }

    pub fn reuse_pid(&self, pid: u32, new_start_ticks: u64) {
        self.start_ticks_by_pid.lock().unwrap().insert(pid, new_start_ticks);
        self.alive.lock().unwrap().insert(pid);
    }
}

impl ProcessIdentityReader for TestIdentityReader {
    fn read_boot_id(&self) -> Result<String, PromotionError> {
        Ok(self.boot_id.clone())
    }

    fn read_process_start_ticks(&self, pid: u32) -> Result<u64, PromotionError> {
        self.start_ticks_by_pid
            .lock()
            .unwrap()
            .get(&pid)
            .copied()
            .ok_or_else(|| PromotionError::GitCommand {
                context: "test pid unknown".into(),
                stderr: pid.to_string(),
            })
    }

    fn process_alive(&self, pid: u32) -> bool {
        self.alive.lock().unwrap().contains(&pid)
    }
}
