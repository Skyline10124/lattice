// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use super::types::{LocalError, LocalErrorKind, Result};
use std::{
    fs::{self, File, OpenOptions},
    os::{
        fd::AsRawFd,
        unix::{
            fs::{DirBuilderExt, FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt},
            net::UnixListener,
        },
    },
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::net::UnixStream;

#[cfg(any(test, feature = "test-utils"))]
pub(crate) use lattice_ipc::ipc::MAGIC;
#[cfg(test)]
pub(crate) use lattice_ipc::ipc::{MAX_FRAME, PROTOCOL_VERSION, magic_for_version, read};
pub(crate) use lattice_ipc::ipc::{
    ResponseReader, ResponseWriter, check_peer, permission, read_login_action, read_request, uid,
    write, write_request,
};

fn check_dir(path: &Path, private: bool) -> Result<()> {
    let meta = fs::symlink_metadata(path)?;
    if !meta.is_dir()
        || meta.uid() != uid()
        || meta.mode() & if private { 0o077 } else { 0o022 } != 0
    {
        return Err(permission());
    }
    Ok(())
}

/// 不创建运行资源的客户端寻址；使用规范路径，避免同一 STATE_ROOT 的锁别名。
pub(super) fn endpoint(layout: &crate::StateLayout) -> Result<PathBuf> {
    if !layout.lattice_root().is_absolute() {
        return Err(permission());
    }
    let root = fs::canonicalize(layout.lattice_root())?;
    check_dir(&root, false)?;
    let directory = root.join("local");
    check_dir(&directory, true)?;
    Ok(directory.join("api.sock"))
}

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
    pub(super) fn bind(layout: &crate::StateLayout) -> Result<(Self, UnixListener)> {
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
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(directory.join("owner.lock"))?;
        let meta = lock.metadata()?;
        if meta.uid() != uid() {
            return Err(permission());
        }
        // 持有文件描述符直至后台宿主退出；不按 PID、mtime 或客户端连接数释放或抢占。
        if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            let error = std::io::Error::last_os_error();
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

fn check_socket(meta: &fs::Metadata) -> Result<()> {
    if meta.uid() != uid() {
        return Err(permission());
    }
    Ok(())
}

/// stop 回执只说明后台已排空；回执之后它还要关闭 listener、删除 socket 并释放实例锁。
/// 停止方在回执之后等实例锁可取得，再交还给调用方；不等待后台进程退出。
pub(super) struct Departure {
    layout: crate::StateLayout,
}

impl Departure {
    pub(super) fn new(layout: crate::StateLayout) -> Self {
        Self { layout }
    }

    /// 在 `timeout` 内等旧后台释放实例锁；超时报 Stopping，后台可能仍在收尾。
    pub(super) async fn wait(self, timeout: Duration) -> Result<()> {
        // 用真实时钟计上限：测试运行时的暂停时钟不应让等待提前判为超时。
        let deadline = std::time::Instant::now() + timeout;
        loop {
            if self.released().await? {
                return Ok(());
            }
            if std::time::Instant::now() >= deadline {
                return Err(LocalError::new(
                    LocalErrorKind::Stopping,
                    "the backend acknowledged stop but has not released STATE_ROOT in time; check again with `lattice daemon status`",
                ));
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    /// 以非阻塞共享 flock 试探实例锁并立即放开，不创建锁文件。
    async fn released(&self) -> Result<bool> {
        let layout = self.layout.clone();
        tokio::task::spawn_blocking(move || {
            let lock = endpoint(&layout)?.with_file_name("owner.lock");
            let file = match OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
                .open(lock)
            {
                Ok(file) => file,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(true),
                Err(error) => return Err(error.into()),
            };
            // SAFETY: flock 只作用于本函数打开的 fd；关闭 fd 即释放试探取得的锁。
            if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_SH | libc::LOCK_NB) } == 0 {
                return Ok(true);
            }
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::WouldBlock {
                Ok(false)
            } else {
                Err(error.into())
            }
        })
        .await
        .map_err(|_| LocalError::new(LocalErrorKind::Internal, "endpoint worker failed"))?
    }
}

pub(super) async fn connect(layout: crate::StateLayout) -> Result<UnixStream> {
    let path = tokio::task::spawn_blocking(move || {
        let path = endpoint(&layout)?;
        check_socket(&fs::symlink_metadata(&path)?)?;
        Ok::<_, LocalError>(path)
    })
    .await
    .map_err(|_| LocalError::new(LocalErrorKind::Internal, "endpoint worker failed"))??;
    let stream = UnixStream::connect(path).await?;
    check_peer(&stream)?;
    Ok(stream)
}
