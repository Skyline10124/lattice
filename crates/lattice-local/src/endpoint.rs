// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 客户端本地端点寻址、socket 属主核验与停止后的实例锁退场等待。

use std::{
    fs::{self, OpenOptions},
    os::{
        fd::AsRawFd,
        unix::fs::{MetadataExt, OpenOptionsExt},
    },
    path::PathBuf,
    time::Duration,
};

use lattice_ipc::{
    LocalError, LocalErrorKind, Result,
    ipc::{check_dir, check_peer, permission, uid},
};
use lattice_store::StateLayout;
use tokio::net::UnixStream;

/// 不创建运行资源的客户端寻址；使用规范路径，避免同一 STATE_ROOT 的锁别名。
pub(crate) fn endpoint(layout: &StateLayout) -> Result<PathBuf> {
    if !layout.lattice_root().is_absolute() {
        return Err(permission());
    }
    let root = fs::canonicalize(layout.lattice_root())?;
    check_dir(&root, false)?;
    let directory = root.join("local");
    check_dir(&directory, true)?;
    Ok(directory.join("api.sock"))
}

fn check_socket(meta: &fs::Metadata) -> Result<()> {
    if meta.uid() != uid() {
        return Err(permission());
    }
    Ok(())
}

/// stop 回执只说明后台已排空；回执之后它还要关闭 listener、删除 socket 并释放实例锁。
/// 停止方在回执之后等实例锁可取得，再交还给调用方；不等待后台进程退出。
pub struct Departure {
    layout: StateLayout,
}

impl Departure {
    pub(crate) fn new(layout: StateLayout) -> Self {
        Self { layout }
    }

    /// 在 `timeout` 内等旧后台释放实例锁；超时报 Stopping，后台可能仍在收尾。
    #[cfg(any(test, feature = "test-utils"))]
    pub async fn wait(self, timeout: Duration) -> Result<()> {
        self.wait_inner(timeout).await
    }

    #[cfg(not(any(test, feature = "test-utils")))]
    pub(crate) async fn wait(self, timeout: Duration) -> Result<()> {
        self.wait_inner(timeout).await
    }

    async fn wait_inner(self, timeout: Duration) -> Result<()> {
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

pub(crate) async fn connect(layout: StateLayout) -> Result<UnixStream> {
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
