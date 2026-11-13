// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use super::types::{LocalError, LocalErrorKind, Result};
use rustix::fs::{FlockOperation, OFlags};
use std::{
    fs::{self, File, OpenOptions},
    os::unix::{
        fs::{DirBuilderExt, FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt},
        net::UnixListener,
    },
    path::{Path, PathBuf},
};

#[cfg(test)]
pub(crate) use lattice_ipc::ipc::{
    MAGIC, MAX_FRAME, PROTOCOL_VERSION, ResponseReader, magic_for_version, read, write,
};
pub(crate) use lattice_ipc::ipc::{
    ResponseWriter, check_dir, check_peer, permission, read_login_action, read_request, uid,
};

/// 实例锁的持有对象；不暴露底层文件描述符，供后台宿主保持锁直至进程退出。
#[derive(Debug)]
pub struct InstanceLock {
    _lock: File,
}

pub(super) struct SocketEndpoint {
    socket: PathBuf,
    identity: (u64, u64),
}

impl SocketEndpoint {
    /// 只删除本实例发布的 socket；实例锁由 `InstanceLock` 独立持有。
    pub(super) fn cleanup(&self) -> Result<()> {
        let meta = fs::symlink_metadata(&self.socket)?;
        if meta.uid() != uid() {
            return Err(permission());
        }
        if (meta.dev(), meta.ino()) != self.identity {
            return Err(permission());
        }
        fs::remove_file(&self.socket)?;
        Ok(())
    }
}

pub(super) struct Instance {
    lock: InstanceLock,
    endpoint: SocketEndpoint,
}

impl Instance {
    pub(super) fn bind(layout: &lattice_session::StateLayout) -> Result<(Self, UnixListener)> {
        if !layout.lattice_root().is_absolute() {
            return Err(permission());
        }
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(layout.lattice_root())?;
        let root = fs::canonicalize(layout.lattice_root())?;
        check_dir(&root, false)?;
        let directory = root.join("local");
        match fs::DirBuilder::new().mode(0o700).create(&directory) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        }
        check_dir(&directory, true)?;
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .mode(0o600)
            .custom_flags((OFlags::NOFOLLOW | OFlags::CLOEXEC).bits() as i32)
            .open(directory.join("owner.lock"))?;
        let meta = lock.metadata()?;
        if meta.uid() != uid() {
            return Err(permission());
        }
        // 持有文件描述符直至后台宿主退出；不按 PID、mtime 或客户端连接数释放或抢占。
        if let Err(errno) = rustix::fs::flock(&lock, FlockOperation::NonBlockingLockExclusive) {
            let error = std::io::Error::from(errno);
            return Err(if error.kind() == std::io::ErrorKind::WouldBlock {
                LocalError::new(
                    LocalErrorKind::AlreadyRunning,
                    "local backend already owns STATE_ROOT",
                )
            } else {
                error.into()
            });
        }
        let socket = directory.join("api.sock");
        remove_stale_socket(&socket)?;
        let staging = directory.join("bind.sock");
        remove_stale_socket(&staging)?;
        let listener = UnixListener::bind(&staging)?;
        fs::set_permissions(&staging, fs::Permissions::from_mode(0o600))?;
        listener.set_nonblocking(true)?;
        // 发布前设好权限，首次自启客户端不会把 chmod 窗口误判为不安全后台。
        fs::rename(&staging, &socket)?;
        let meta = fs::symlink_metadata(&socket)?;
        Ok((
            Self {
                lock: InstanceLock { _lock: lock },
                endpoint: SocketEndpoint {
                    socket,
                    identity: (meta.dev(), meta.ino()),
                },
            },
            listener,
        ))
    }

    pub(super) fn into_parts(self) -> (InstanceLock, SocketEndpoint) {
        (self.lock, self.endpoint)
    }

    /// 只删除本实例发布的 socket；实例锁随 Instance 释放，调用方决定释放的时机。
    #[cfg(test)]
    pub(super) fn cleanup(&self) -> Result<()> {
        self.endpoint.cleanup()
    }
}

// 仅在持有实例锁时调用；失效名称仍须是本用户的 socket，不能按文件名删除任意对象。
fn remove_stale_socket(socket: &Path) -> Result<()> {
    let meta = match fs::symlink_metadata(socket) {
        Ok(meta) => meta,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    if !meta.file_type().is_socket() || meta.uid() != uid() {
        return Err(permission());
    }
    match std::os::unix::net::UnixStream::connect(socket) {
        Ok(_) => {
            return Err(LocalError::new(
                LocalErrorKind::Conflict,
                "live socket exists without matching owner lock",
            ));
        }
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::NotFound
            ) => {}
        Err(error) => return Err(error.into()),
    }
    let current = fs::symlink_metadata(socket)?;
    if (current.dev(), current.ino()) != (meta.dev(), meta.ino()) {
        return Err(permission());
    }
    fs::remove_file(socket)?;
    Ok(())
}
