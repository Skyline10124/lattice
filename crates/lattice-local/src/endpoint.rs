// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 客户端本地端点寻址、socket 属主核验与停止后的实例锁退场等待。

use std::{
    fs::{self, OpenOptions},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
    time::Duration,
};

use lattice_ipc::{
    LocalError, LocalErrorKind, Result,
    ipc::{check_dir, check_peer, uid},
};
use lattice_store::StateLayout;
use rustix::fs::{FlockOperation, OFlags};
use tokio::net::UnixStream;

/// 前端连不上后台时唯一生成的中文提示。
const NOT_RUNNING_MESSAGE: &str = "后台未运行；可执行 `systemctl --user start lattice` 启动，尚未安装用户服务时先执行 `lattice daemon install`（并请确认前端与后台的状态根一致）";

fn client_not_running() -> LocalError {
    LocalError::new(LocalErrorKind::NotRunning, NOT_RUNNING_MESSAGE)
}

fn with_client_not_running(error: LocalError) -> LocalError {
    if error.kind == LocalErrorKind::NotRunning {
        client_not_running()
    } else {
        error
    }
}

fn client_connect_io_error(action: &str, path: &Path, error: std::io::Error) -> LocalError {
    match error.kind() {
        std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused => {
            client_not_running()
        }
        std::io::ErrorKind::PermissionDenied => LocalError::new(
            LocalErrorKind::Permission,
            &format!("{action}权限不足：{}：{error}", path.display()),
        ),
        std::io::ErrorKind::NotADirectory => LocalError::new(
            LocalErrorKind::Permission,
            &format!("{action}路径不是目录：{}：{error}", path.display()),
        ),
        _ => LocalError::new(
            LocalErrorKind::Transport,
            &format!("{action}失败：{}：{error}", path.display()),
        ),
    }
}

/// 不创建运行资源的客户端寻址；使用规范路径，避免同一 STATE_ROOT 的锁别名。
pub(crate) fn endpoint(layout: &StateLayout) -> Result<PathBuf> {
    if !layout.lattice_root().is_absolute() {
        return Err(LocalError::new(
            LocalErrorKind::Permission,
            &format!(
                "状态根目录必须是绝对路径：{}",
                layout.lattice_root().display()
            ),
        ));
    }
    let root = fs::canonicalize(layout.lattice_root()).map_err(|error| match error.kind() {
        std::io::ErrorKind::NotFound => LocalError::new(
            LocalErrorKind::NotRunning,
            &format!(
                "本地状态根目录不存在：{}：{error}",
                layout.lattice_root().display()
            ),
        ),
        std::io::ErrorKind::PermissionDenied => LocalError::new(
            LocalErrorKind::Permission,
            &format!(
                "访问状态根目录权限不足：{}：{error}",
                layout.lattice_root().display()
            ),
        ),
        std::io::ErrorKind::NotADirectory => LocalError::new(
            LocalErrorKind::Permission,
            &format!(
                "状态根路径不是目录：{}：{error}",
                layout.lattice_root().display()
            ),
        ),
        _ => LocalError::new(
            LocalErrorKind::Transport,
            &format!(
                "解析状态根目录失败：{}：{error}",
                layout.lattice_root().display()
            ),
        ),
    })?;
    check_dir(&root, false)?;
    let directory = root.join("local");
    check_dir(&directory, true)?;
    Ok(directory.join("api.sock"))
}

fn check_socket(path: &Path, meta: &fs::Metadata) -> Result<()> {
    let current_uid = uid();
    if meta.uid() != current_uid {
        return Err(LocalError::new(
            LocalErrorKind::Permission,
            &format!(
                "本地连接套接字属主不匹配：{}（套接字 UID：{}，当前有效 UID：{current_uid}）",
                path.display(),
                meta.uid()
            ),
        ));
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
                    "后台已确认停机，但未在期限内释放状态根；请执行 `lattice daemon status` 复核",
                ));
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    /// 以非阻塞共享 flock 试探实例锁并立即放开，不创建锁文件。
    async fn released(&self) -> Result<bool> {
        let layout = self.layout.clone();
        tokio::task::spawn_blocking(move || {
            let lock = match endpoint(&layout) {
                Ok(sock) => sock.with_file_name("owner.lock"),
                Err(error) if error.kind == LocalErrorKind::NotRunning => return Ok(true),
                Err(error) => return Err(error),
            };
            let file = match OpenOptions::new()
                .read(true)
                .custom_flags((OFlags::NOFOLLOW | OFlags::CLOEXEC).bits() as i32)
                .open(&lock)
            {
                Ok(file) => file,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(true),
                Err(error) => return Err(error.into()),
            };
            match rustix::fs::flock(&file, FlockOperation::NonBlockingLockShared) {
                Ok(()) => Ok(true),
                Err(errno) => {
                    let error = std::io::Error::from(errno);
                    if error.kind() == std::io::ErrorKind::WouldBlock {
                        Ok(false)
                    } else {
                        Err(error.into())
                    }
                }
            }
        })
        .await
        .map_err(|_| LocalError::new(LocalErrorKind::Internal, "本地端点工作线程异常退出"))?
    }
}

pub(crate) async fn connect(layout: StateLayout) -> Result<UnixStream> {
    let path = tokio::task::spawn_blocking(move || {
        let path = endpoint(&layout).map_err(with_client_not_running)?;
        let meta = fs::symlink_metadata(&path)
            .map_err(|error| client_connect_io_error("检查本地连接套接字", &path, error))?;
        check_socket(&path, &meta)?;
        Ok::<_, LocalError>(path)
    })
    .await
    .map_err(|_| LocalError::new(LocalErrorKind::Internal, "本地端点工作线程异常退出"))??;
    let stream = UnixStream::connect(&path)
        .await
        .map_err(|error| client_connect_io_error("连接本地套接字", &path, error))?;
    check_peer(&stream)?;
    Ok(stream)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::{fs::PermissionsExt, net::UnixListener as StdUnixListener};

    #[tokio::test]
    async fn connect_generates_client_not_running_guidance_only_for_missing_or_refused_endpoint() {
        struct TempDir(PathBuf);
        impl Drop for TempDir {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }
        let dir = TempDir(PathBuf::from("/tmp").join(format!(
            "ltc-ep-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        )));
        fs::create_dir_all(&dir.0).unwrap();
        let layout = StateLayout::in_state_root(&dir.0);

        // 1. 状态根尚无 lattice/ 目录
        let missing_root = connect(layout.clone()).await.unwrap_err();
        assert_eq!(missing_root.kind, LocalErrorKind::NotRunning);
        for required in [
            "后台未运行",
            "systemctl --user start lattice",
            "lattice daemon install",
            "状态根",
        ] {
            assert!(
                missing_root.message.contains(required),
                "缺失状态根时客户端提示缺少 `{required}`：{}",
                missing_root.message
            );
        }

        // 2. local/ 已创建但无 api.sock
        let local_dir = layout.lattice_root().join("local");
        fs::create_dir_all(&local_dir).unwrap();
        fs::set_permissions(layout.lattice_root(), fs::Permissions::from_mode(0o700)).unwrap();
        fs::set_permissions(&local_dir, fs::Permissions::from_mode(0o700)).unwrap();
        let missing_sock = connect(layout.clone()).await.unwrap_err();
        assert_eq!(missing_sock.kind, LocalErrorKind::NotRunning);
        assert_eq!(missing_sock.message, missing_root.message);

        // 3. 残留失效 socket（ConnectionRefused）
        let sock_path = local_dir.join("api.sock");
        let listener = StdUnixListener::bind(&sock_path).unwrap();
        fs::set_permissions(&sock_path, fs::Permissions::from_mode(0o600)).unwrap();
        drop(listener);
        let refused = connect(layout.clone()).await.unwrap_err();
        assert_eq!(refused.kind, LocalErrorKind::NotRunning);
        assert_eq!(refused.message, missing_root.message);
        fs::remove_file(&sock_path).unwrap();

        // 4. local/ 权限 0777 时报中文 Permission，不伪装成 NotRunning
        fs::set_permissions(&local_dir, fs::Permissions::from_mode(0o777)).unwrap();
        let perm = connect(layout).await.unwrap_err();
        assert_eq!(perm.kind, LocalErrorKind::Permission);
        assert!(
            !perm.message.contains("后台未运行")
                && !perm
                    .message
                    .contains("local endpoint ownership or permissions rejected"),
            "权限错误不应带未运行提示或英文原文：{}",
            perm.message
        );
    }
}
