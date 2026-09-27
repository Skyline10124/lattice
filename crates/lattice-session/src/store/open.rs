// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use super::*;
use lattice_store::{
    Blocking, LockMode, acquire_flock,
    durability::{fsync_dir, fsync_file},
    lock::acquire_flock_strict,
    probe::probe_location,
};
use redb::ReadableDatabase;
use std::{
    fs,
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::Path,
};

#[cfg(feature = "test-utils")]
thread_local! {
    static CREATION_SYNC_PROBE: std::cell::RefCell<Option<Box<dyn Fn() -> bool>>> = const { std::cell::RefCell::new(None) };
}

/// 下游故障回归：仅在当前同步线程的创建父目录屏障前调用，true 注入失败。
#[cfg(feature = "test-utils")]
pub fn with_test_creation_sync_probe<T>(
    probe: impl Fn() -> bool + 'static,
    operation: impl FnOnce() -> T,
) -> T {
    struct Reset(Option<Box<dyn Fn() -> bool>>);
    impl Drop for Reset {
        fn drop(&mut self) {
            CREATION_SYNC_PROBE.with(|p| {
                p.replace(self.0.take());
            });
        }
    }
    let _reset = Reset(CREATION_SYNC_PROBE.with(|p| p.replace(Some(Box::new(probe)))));
    operation()
}

fn sync_creation_parent(parent: &Path) -> std::result::Result<(), lattice_store::StoreError> {
    #[cfg(feature = "test-utils")]
    if CREATION_SYNC_PROBE.with(|p| p.borrow().as_ref().is_some_and(|probe| probe())) {
        return Err(lattice_store::StoreError::Durability {
            operation: "fsync directory",
            path: parent.to_path_buf(),
            source: injected_io(),
        });
    }
    fsync_dir(parent)
}

fn io(operation: &'static str, path: &Path, source: std::io::Error) -> StoreError {
    StoreError::Io {
        operation,
        path: path.to_path_buf(),
        source,
    }
}
fn metadata(path: &Path) -> Result<Option<fs::Metadata>> {
    match fs::symlink_metadata(path) {
        Ok(m) => Ok(Some(m)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(io("inspect path", path, e)),
    }
}

/// 检查整个状态根，而不是只检查当前项目；不跟随项目目录链接。
pub fn detect_legacy_stores(layout: &StateLayout) -> Result<Vec<PathBuf>> {
    let mut paths = Vec::new();
    let sessions = layout.lattice_root().join("sessions.git");
    if metadata(&sessions)?.is_some() {
        paths.push(sessions);
    }
    let projects = layout.projects_dir();
    if let Some(m) = metadata(&projects)? {
        if !m.is_dir() || m.file_type().is_symlink() {
            return Err(corrupt("layout", "projects", "expected a real directory"));
        }
        for entry in fs::read_dir(&projects).map_err(|e| io("enumerate projects", &projects, e))? {
            let entry = entry.map_err(|e| io("enumerate project", &projects, e))?;
            let path = entry.path();
            let m = fs::symlink_metadata(&path).map_err(|e| io("inspect project", &path, e))?;
            if m.file_type().is_symlink() {
                return Err(corrupt("layout", "projects", "symbolic project directory"));
            }
            if m.is_dir() {
                let legacy = path.join("invocations.git");
                if metadata(&legacy)?.is_some() {
                    paths.push(legacy);
                }
            }
        }
    }
    paths.sort();
    Ok(paths)
}
fn reject_legacy(layout: &StateLayout) -> Result<()> {
    let paths = detect_legacy_stores(layout)?;
    if paths.is_empty() {
        Ok(())
    } else {
        Err(StoreError::LegacyStores { paths })
    }
}

fn prepare_parent(path: &Path) -> Result<()> {
    if !path.is_absolute() {
        return Err(invalid("layout", "state root must be absolute"));
    }
    if let Some(m) = metadata(path)? {
        if !m.is_dir() || m.file_type().is_symlink() {
            return Err(corrupt("layout", "parent", "expected real directory"));
        }
        return Ok(());
    }
    let parent = path
        .parent()
        .ok_or_else(|| invalid("layout", "missing parent"))?;
    prepare_parent(parent)?;
    fs::create_dir(path)
        .or_else(|e| {
            if e.kind() == std::io::ErrorKind::AlreadyExists {
                Ok(())
            } else {
                Err(e)
            }
        })
        .map_err(|e| io("create parent", path, e))?;
    fsync_dir(path)?;
    fsync_dir(parent)?;
    Ok(())
}
fn canonical_layout(layout: &StateLayout) -> Result<StateLayout> {
    let root = layout.lattice_root();
    let actual = fs::canonicalize(root).map_err(|e| io("canonicalize state layout", root, e))?;
    if actual.file_name() != Some(std::ffi::OsStr::new("lattice")) {
        return Err(invalid("layout", "unexpected canonical lattice directory"));
    }
    Ok(StateLayout::in_state_root(
        actual
            .parent()
            .ok_or_else(|| invalid("layout", "missing state root"))?,
    ))
}

impl SessionStore {
    /// 严格打开既有库；只允许 redb 自身恢复，不初始化或修复领域记录。
    pub fn open(layout: &StateLayout) -> Result<Self> {
        reject_legacy(layout)?;
        let path = layout.lattice_root().join("lattice.redb");
        let meta =
            metadata(&path)?.ok_or_else(|| StoreError::NotInitialized { path: path.clone() })?;
        if !meta.is_file() || meta.file_type().is_symlink() {
            return Err(corrupt("layout", "lattice.redb", "expected regular file"));
        }
        let layout = canonical_layout(layout)?;
        let path = layout.lattice_root().join("lattice.redb");
        let db = redb::Database::open(&path)?;
        let read = db.begin_read()?;
        let records = codec::load_read(&read)?;
        validate::records(&records)?;
        drop(read);
        Ok(Self {
            db,
            layout,
            path,
            write_disabled: Mutex::new(false),
        })
    }

    /// 仅供新 Session 接受路径首次创建；隔离调用方只能使用自有目录。
    pub fn create(layout: &StateLayout) -> Result<Self> {
        reject_legacy(layout)?;
        prepare_parent(layout.lattice_root())?;
        let layout = canonical_layout(layout)?;
        let parent = layout.lattice_root();
        let target = parent.join("lattice.redb");
        #[cfg(test)]
        if hit(FaultPoint::Probe) {
            return Err(io("probe", parent, injected_io()));
        }
        probe_location(parent)?;
        let _lock =
            acquire_flock_strict(&parent.join("lattice.redb.create.lock"), Blocking::Block)?;
        reject_legacy(&layout)?;
        if metadata(&target)?.is_some() {
            let store = Self::open(&layout)?;
            sync_creation_parent(parent).map_err(|source| {
                StoreError::CreationDurabilityUnknown {
                    path: target,
                    source,
                }
            })?;
            return Ok(store);
        }
        let owner = fs::metadata(parent)
            .map_err(|e| io("inspect parent", parent, e))?
            .uid();
        for entry in fs::read_dir(parent).map_err(|e| io("scan creation leftovers", parent, e))? {
            let entry = entry.map_err(|e| io("scan creation leftover", parent, e))?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            let Some(id) = name.strip_prefix(".lattice.redb-create-") else {
                continue;
            };
            if TurnId::parse(id).is_err() {
                continue;
            }
            let path = entry.path();
            let m = fs::symlink_metadata(&path)
                .map_err(|e| io("inspect creation leftover", &path, e))?;
            if !m.is_file() || m.file_type().is_symlink() || m.uid() != owner {
                return Err(corrupt("creation", "temporary", "unsafe creation leftover"));
            }
            #[cfg(test)]
            if hit(FaultPoint::Cleanup) {
                return Err(io("remove creation leftover", &path, injected_io()));
            }
            fs::remove_file(&path).map_err(|e| io("remove creation leftover", &path, e))?;
        }
        let temp = parent.join(format!(".lattice.redb-create-{}", uuid::Uuid::new_v4()));
        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temp)
            .map_err(|e| io("create temporary database", &temp, e))?;
        let db = redb::Database::builder().create_file(file)?;
        let mut tx = db.begin_write()?;
        tx.set_durability(redb::Durability::Immediate)?;
        codec::initialize(&tx)?;
        #[cfg(test)]
        if hit(FaultPoint::CreateCommit) {
            return Err(commit_error(redb::CommitError::TransactionPoisoned));
        }
        tx.commit().map_err(commit_error)?;
        drop(db);
        fsync_file(&temp)?;
        #[cfg(test)]
        if hit(FaultPoint::BeforeRename) {
            return Err(io("publish database", &target, injected_io()));
        }
        rustix::fs::renameat_with(
            rustix::fs::CWD,
            &temp,
            rustix::fs::CWD,
            &target,
            rustix::fs::RenameFlags::NOREPLACE,
        )
        .map_err(|e| io("publish database", &target, e.into()))?;
        #[cfg(test)]
        if hit(FaultPoint::AfterRename) {
            return Err(StoreError::CreationDurabilityUnknown {
                path: target.clone(),
                source: lattice_store::StoreError::Durability {
                    operation: "fsync directory",
                    path: parent.to_path_buf(),
                    source: injected_io(),
                },
            });
        }
        sync_creation_parent(parent).map_err(|source| StoreError::CreationDurabilityUnknown {
            path: target,
            source,
        })?;
        Self::open(&layout)
    }
    pub fn begin_read(&self) -> Result<SessionRead> {
        Ok(SessionRead {
            tx: self.db.begin_read()?,
        })
    }
    pub fn try_lock_session(&self, id: SessionId) -> Result<SessionExecutionLock> {
        let path = self.layout.session_lock(&format!("{id}.lock"));
        let guard = acquire_flock(&path, LockMode::Exclusive, Blocking::NonBlocking).map_err(
            |e| match e {
                lattice_store::StoreError::Busy { .. } => StoreError::Busy { session_id: id },
                other => StoreError::Kernel(other),
            },
        )?;
        Ok(SessionExecutionLock {
            guard,
            session_id: id,
        })
    }
    pub fn path(&self) -> &Path {
        &self.path
    }
    pub(super) fn check_lock(&self, lock: &SessionExecutionLock, id: SessionId) -> Result<()> {
        if lock.session_id != id
            || lock.guard.mode() != LockMode::Exclusive
            || lock.guard.path() != self.layout.session_lock(&format!("{id}.lock"))
        {
            Err(StoreError::InvalidLockCapability)
        } else {
            Ok(())
        }
    }
}
