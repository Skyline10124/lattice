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
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::UnixStream,
};
use zeroize::Zeroizing;

const MAGIC: &[u8; 8] = b"LTLOCAL2";
pub(super) const MAX_FRAME: usize = 16 * 1024 * 1024;
pub(super) const WRITE_TIMEOUT: Duration = Duration::from_secs(5);

fn permission() -> LocalError {
    LocalError::new(
        LocalErrorKind::Permission,
        "local endpoint ownership or permissions rejected",
    )
}
fn protocol() -> LocalError {
    LocalError::new(
        LocalErrorKind::Protocol,
        "unsupported or malformed local protocol frame",
    )
}

pub(super) fn uid() -> u32 {
    // Linux geteuid 无参数、无内存访问；通信两端必须匹配实际有效 UID。
    unsafe { libc::geteuid() }
}

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

pub(super) struct Instance {
    _lock: File,
    socket: PathBuf,
    identity: (u64, u64),
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
        // 持有文件描述符直至 serve 排空结束；不按 PID、mtime 或客户端连接数释放或抢占。
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
                _lock: lock,
                socket,
                identity: (meta.dev(), meta.ino()),
            },
            listener,
        ))
    }

    pub(super) fn cleanup(self) -> Result<()> {
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

pub(super) fn check_peer(stream: &UnixStream) -> Result<()> {
    if stream.peer_cred()?.uid() != uid() {
        return Err(permission());
    }
    Ok(())
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

pub(super) async fn read<T: serde::de::DeserializeOwned>(stream: &mut UnixStream) -> Result<T> {
    let mut magic = [0; 8];
    stream.read_exact(&mut magic).await?;
    if &magic != MAGIC {
        return Err(protocol());
    }
    let len = stream.read_u32().await? as usize;
    if len > MAX_FRAME {
        return Err(protocol());
    }
    let mut bytes = Zeroizing::new(vec![0; len]);
    stream.read_exact(&mut bytes).await?;
    serde_json::from_slice(&bytes).map_err(|_| protocol())
}

struct Frame(Zeroizing<Vec<u8>>);
impl std::io::Write for Frame {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if self.0.len().saturating_add(bytes.len()) > MAX_FRAME {
            return Err(std::io::Error::other("frame exceeds limit"));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

pub(super) async fn write<T: serde::Serialize>(stream: &mut UnixStream, value: &T) -> Result<()> {
    let mut frame = Frame(Zeroizing::new(Vec::new()));
    serde_json::to_writer(&mut frame, value).map_err(|_| protocol())?;
    tokio::time::timeout(WRITE_TIMEOUT, async {
        stream.write_all(MAGIC).await?;
        stream.write_u32(frame.0.len() as u32).await?;
        stream.write_all(&frame.0).await
    })
    .await
    .map_err(|_| {
        LocalError::new(
            LocalErrorKind::Transport,
            "local write timed out; acknowledgement may be unknown",
        )
    })??;
    Ok(())
}
