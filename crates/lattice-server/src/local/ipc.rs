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
    ResponseWriter, check_dir, check_peer, read_login_action, read_request, uid,
};

fn daemon_io_error(action: &str, path: &Path, error: std::io::Error) -> LocalError {
    let kind = if error.kind() == std::io::ErrorKind::PermissionDenied {
        LocalErrorKind::Permission
    } else {
        LocalErrorKind::Internal
    };
    LocalError::new(kind, &format!("{action}失败：{}：{error}", path.display()))
}

fn cleanup_socket_io_error(socket: &Path, error: std::io::Error) -> LocalError {
    let kind = if error.kind() == std::io::ErrorKind::PermissionDenied {
        LocalErrorKind::Permission
    } else {
        LocalErrorKind::Internal
    };
    LocalError::new(
        kind,
        &format!("清理 socket 失败：{}：{error}", socket.display()),
    )
}

fn daemon_dir_error(error: LocalError) -> LocalError {
    if error.kind == LocalErrorKind::NotRunning {
        LocalError::new(LocalErrorKind::Internal, &error.message)
    } else {
        error
    }
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
        let meta = fs::symlink_metadata(&self.socket)
            .map_err(|error| cleanup_socket_io_error(&self.socket, error))?;
        let current_uid = uid();
        if meta.uid() != current_uid {
            return Err(LocalError::new(
                LocalErrorKind::Permission,
                &format!(
                    "清理 socket 失败：套接字属主不匹配：{}（套接字 UID：{}，当前有效 UID：{current_uid}）",
                    self.socket.display(),
                    meta.uid()
                ),
            ));
        }
        if (meta.dev(), meta.ino()) != self.identity {
            return Err(LocalError::new(
                LocalErrorKind::Permission,
                &format!(
                    "清理 socket 失败：路径已不再指向本实例发布的套接字：{}",
                    self.socket.display()
                ),
            ));
        }
        fs::remove_file(&self.socket)
            .map_err(|error| cleanup_socket_io_error(&self.socket, error))?;
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
            return Err(LocalError::new(
                LocalErrorKind::Permission,
                &format!(
                    "状态根目录必须是绝对路径：{}",
                    layout.lattice_root().display()
                ),
            ));
        }
        if let Some(state_root) = layout.lattice_root().parent()
            && let Ok(sym_meta) = fs::symlink_metadata(state_root)
            && !sym_meta.is_dir()
        {
            if sym_meta.file_type().is_symlink() {
                let target_meta = fs::metadata(state_root)
                    .map_err(|error| daemon_io_error("解析状态根符号链接", state_root, error))?;
                if !target_meta.is_dir() {
                    return Err(LocalError::new(
                        LocalErrorKind::Permission,
                        &format!("状态根符号链接指向的路径不是目录：{}", state_root.display()),
                    ));
                }
            } else {
                return Err(LocalError::new(
                    LocalErrorKind::Permission,
                    &format!("状态根路径已存在但不是目录：{}", state_root.display()),
                ));
            }
        }
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(layout.lattice_root())
            .map_err(|error| match error.kind() {
                std::io::ErrorKind::AlreadyExists => LocalError::new(
                    LocalErrorKind::Permission,
                    &format!(
                        "状态根目录路径已存在但不是目录：{}：{error}",
                        layout.lattice_root().display()
                    ),
                ),
                std::io::ErrorKind::NotADirectory => LocalError::new(
                    LocalErrorKind::Permission,
                    &format!(
                        "无法创建状态根目录（父路径包含非目录项）：{}：{error}",
                        layout.lattice_root().display()
                    ),
                ),
                std::io::ErrorKind::PermissionDenied => LocalError::new(
                    LocalErrorKind::Permission,
                    &format!(
                        "创建状态根目录权限不足：{}：{error}",
                        layout.lattice_root().display()
                    ),
                ),
                _ => daemon_io_error("创建状态根目录", layout.lattice_root(), error),
            })?;
        let root = fs::canonicalize(layout.lattice_root())
            .map_err(|error| daemon_io_error("规范化状态根目录", layout.lattice_root(), error))?;
        check_dir(&root, false).map_err(daemon_dir_error)?;
        let directory = root.join("local");
        match fs::DirBuilder::new().mode(0o700).create(&directory) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(daemon_io_error("创建本地端点目录", &directory, error)),
        }
        check_dir(&directory, true).map_err(daemon_dir_error)?;
        let lock_path = directory.join("owner.lock");
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .mode(0o600)
            .custom_flags((OFlags::NOFOLLOW | OFlags::CLOEXEC).bits() as i32)
            .open(&lock_path)
            .map_err(|error| {
                if error.raw_os_error() == Some(rustix::io::Errno::LOOP.raw_os_error()) {
                    LocalError::new(
                        LocalErrorKind::Permission,
                        &format!("实例锁路径不得为符号链接：{}：{error}", lock_path.display()),
                    )
                } else {
                    daemon_io_error("打开实例锁文件", &lock_path, error)
                }
            })?;
        let meta = lock
            .metadata()
            .map_err(|error| daemon_io_error("读取实例锁元数据", &lock_path, error))?;
        let current_uid = uid();
        if meta.uid() != current_uid {
            return Err(LocalError::new(
                LocalErrorKind::Permission,
                &format!(
                    "实例锁属主不匹配：{}（文件 UID：{}，当前有效 UID：{current_uid}）",
                    lock_path.display(),
                    meta.uid()
                ),
            ));
        }
        // 持有文件描述符直至后台宿主退出；不按 PID、mtime 或客户端连接数释放或抢占。
        if let Err(errno) = rustix::fs::flock(&lock, FlockOperation::NonBlockingLockExclusive) {
            let error = std::io::Error::from(errno);
            return Err(if error.kind() == std::io::ErrorKind::WouldBlock {
                LocalError::new(
                    LocalErrorKind::AlreadyRunning,
                    "本地后台已持有该状态根的实例锁",
                )
            } else {
                daemon_io_error("加锁实例锁文件", &lock_path, error)
            });
        }
        let socket = directory.join("api.sock");
        remove_stale_socket(&socket)?;
        let staging = directory.join("bind.sock");
        remove_stale_socket(&staging)?;
        let listener = UnixListener::bind(&staging)
            .map_err(|error| daemon_io_error("绑定临时监听套接字", &staging, error))?;
        fs::set_permissions(&staging, fs::Permissions::from_mode(0o600))
            .map_err(|error| daemon_io_error("设置临时监听套接字权限", &staging, error))?;
        listener
            .set_nonblocking(true)
            .map_err(|error| daemon_io_error("设置监听套接字非阻塞模式", &staging, error))?;
        // 发布前设好权限，首次连接的客户端不会把 chmod 窗口误判为不安全后台。
        fs::rename(&staging, &socket)
            .map_err(|error| daemon_io_error("发布监听套接字", &socket, error))?;
        let meta = fs::symlink_metadata(&socket)
            .map_err(|error| daemon_io_error("核验已发布的监听套接字", &socket, error))?;
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
        Err(error) => return Err(daemon_io_error("检查残留套接字元数据", socket, error)),
    };
    if !meta.file_type().is_socket() {
        return Err(LocalError::new(
            LocalErrorKind::Permission,
            &format!("残留端点路径不是 Unix 套接字：{}", socket.display()),
        ));
    }
    let current_uid = uid();
    if meta.uid() != current_uid {
        return Err(LocalError::new(
            LocalErrorKind::Permission,
            &format!(
                "残留套接字属主不匹配：{}（套接字 UID：{}，当前有效 UID：{current_uid}）",
                socket.display(),
                meta.uid()
            ),
        ));
    }
    match std::os::unix::net::UnixStream::connect(socket) {
        Ok(_) => {
            return Err(LocalError::new(
                LocalErrorKind::Conflict,
                &format!("存在未被实例锁保护的存活监听套接字：{}", socket.display()),
            ));
        }
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::NotFound
            ) => {}
        Err(error) => return Err(daemon_io_error("探测残留套接字连接状态", socket, error)),
    }
    let current = fs::symlink_metadata(socket)
        .map_err(|error| daemon_io_error("复核残留套接字元数据", socket, error))?;
    if (current.dev(), current.ino()) != (meta.dev(), meta.ino()) {
        return Err(LocalError::new(
            LocalErrorKind::Permission,
            &format!("清理残留套接字时路径目标发生变化：{}", socket.display()),
        ));
    }
    fs::remove_file(socket).map_err(|error| daemon_io_error("删除残留套接字", socket, error))?;
    Ok(())
}
