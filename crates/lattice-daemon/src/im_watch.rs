// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! `~/.lattice/im` 目录的异步 `inotify` 监视与代次失效恢复。

use std::{
    mem::MaybeUninit,
    os::{fd::OwnedFd, unix::fs::MetadataExt},
    path::Path,
    sync::{Arc, Mutex},
};

use lattice_ipc::{ImMountResult, LocalErrorKind};
use lattice_server::ComponentRemountHandle;
use rustix::{
    fs::inotify::{self, CreateFlags, ReadFlags, WatchFlags},
    io::Errno,
};
use tokio::{io::unix::AsyncFd, task::AbortHandle};

const CONFIG_FILE_BYTES: &[u8] = b"config.toml";

struct ActiveWatch {
    generation: u64,
    abort: AbortHandle,
}

#[derive(Default)]
struct WatchSlot {
    next_generation: u64,
    active: Option<ActiveWatch>,
}

/// 按单调递增代次维护 `im/` 的 `inotify` 监视任务，并在建立时复核目录身份 `(st_dev, st_ino)`。
#[derive(Default)]
pub(crate) struct ImDirectoryWatcher {
    slot: Mutex<WatchSlot>,
}

impl Drop for ImDirectoryWatcher {
    fn drop(&mut self) {
        if let Ok(mut slot) = self.slot.lock()
            && let Some(active) = slot.active.take()
        {
            active.abort.abort();
        }
    }
}

impl ImDirectoryWatcher {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// 立即停止并移除当前活动的目录监视任务。
    pub(crate) fn clear(&self) {
        let mut slot = self.slot.lock().expect("im 目录监视锁不应中毒");
        if let Some(active) = slot.active.take() {
            active.abort.abort();
        }
    }

    /// 为给定 `im/` 目录（由 `(st_dev, st_ino)` 标识）建立新代次的 `inotify` 监视并替换旧监视。
    ///
    /// 每次 `prepare` 均建立全新 `inotify` 描述符并推进代次（随后在同一轮挂载中读取 `config.toml`，无事件盲窗），
    /// 从而彻底隔离同 inode 目录被移走再移回时旧描述符队列中尚未被异步循环消费的迟到 `MOVE_SELF` 等失效事件。
    pub(crate) fn ensure_watching(
        self: &Arc<Self>,
        im_dir: &Path,
        identity: (u64, u64),
        remount_handle: &ComponentRemountHandle,
    ) -> Result<(), String> {
        let mut slot = self.slot.lock().expect("im 目录监视锁不应中毒");
        if let Some(previous) = slot.active.take() {
            previous.abort.abort();
        }

        let rt = tokio::runtime::Handle::try_current()
            .map_err(|_| "建立 im 配置目录监视失败：缺少异步运行时上下文".to_owned())?;
        let _enter = rt.enter();

        let fd = inotify::init(CreateFlags::NONBLOCK | CreateFlags::CLOEXEC).map_err(|err| {
            format!(
                "初始化 im 配置目录 inotify 失败：{}",
                std::io::Error::from(err)
            )
        })?;
        let watch_flags = WatchFlags::CLOSE_WRITE
            | WatchFlags::MOVED_TO
            | WatchFlags::DELETE_SELF
            | WatchFlags::MOVE_SELF
            | WatchFlags::DONT_FOLLOW
            | WatchFlags::ONLYDIR;
        inotify::add_watch(&fd, im_dir, watch_flags)
            .map_err(|err| format!("监听 im 配置目录失败：{}", std::io::Error::from(err)))?;

        // 建立内核 watch 后复核目录元数据，杜绝核验与 add_watch 之间的路径替换窗口。
        verify_watched_directory_identity(im_dir, identity)?;

        let async_fd =
            AsyncFd::new(fd).map_err(|err| format!("注册 im 配置目录异步监视失败：{err}"))?;

        slot.next_generation += 1;
        let generation = slot.next_generation;
        let watcher_weak = Arc::downgrade(self);
        let handle = remount_handle.clone();

        let join_handle = rt.spawn(lattice_server::scoped_component_future(async move {
            run_watch_loop(async_fd, generation, watcher_weak, handle).await;
        }));

        slot.active = Some(ActiveWatch {
            generation,
            abort: join_handle.abort_handle(),
        });
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn active_generation(&self) -> Option<u64> {
        let slot = self.slot.lock().expect("im 目录监视锁不应中毒");
        slot.active
            .as_ref()
            .filter(|w| !w.abort.is_finished())
            .map(|w| w.generation)
    }

    fn invalidate_generation(&self, generation: u64) {
        let mut slot = self.slot.lock().expect("im 目录监视锁不应中毒");
        if slot
            .active
            .as_ref()
            .is_some_and(|active| active.generation == generation)
        {
            slot.active = None;
        }
    }
}

fn verify_watched_directory_identity(im_dir: &Path, expected: (u64, u64)) -> Result<(), String> {
    let meta = std::fs::symlink_metadata(im_dir)
        .map_err(|err| format!("复核 im 配置目录元数据失败：{err}"))?;
    if meta.file_type().is_symlink() || !meta.is_dir() {
        return Err("im/config.toml：im 目录必须是真实目录且不得为符号链接".into());
    }
    if meta.uid() != rustix::process::geteuid().as_raw() {
        return Err("im/config.toml：配置文件及所属目录必须归当前用户所有".into());
    }
    if meta.mode() & 0o077 != 0 {
        return Err(
            "im/config.toml：im 目录权限过宽（不得向组或其他用户开放权限，须为 0700）".into(),
        );
    }
    if (meta.dev(), meta.ino()) != expected {
        return Err("im 配置目录在建立监视期间发生变化，请重试 reload".into());
    }
    Ok(())
}

async fn run_watch_loop(
    async_fd: AsyncFd<OwnedFd>,
    generation: u64,
    watcher: std::sync::Weak<ImDirectoryWatcher>,
    remount_handle: ComponentRemountHandle,
) {
    loop {
        let mut ready = match async_fd.readable().await {
            Ok(guard) => guard,
            Err(err) => {
                tracing::warn!(generation, %err, "im 配置目录监视等待可读事件失败，标记当前监视失效");
                mark_invalidated(generation, &watcher);
                break;
            }
        };

        let mut saw_self_invalidation = false;
        let mut saw_config_ready = false;
        let mut read_failed = None;

        {
            let mut buf = [MaybeUninit::<u8>::uninit(); 4096];
            let mut reader = inotify::Reader::new(ready.get_inner(), &mut buf);
            loop {
                match reader.next() {
                    Ok(event) => {
                        let flags = event.events();
                        if flags.intersects(
                            ReadFlags::DELETE_SELF
                                | ReadFlags::MOVE_SELF
                                | ReadFlags::IGNORED
                                | ReadFlags::UNMOUNT,
                        ) {
                            saw_self_invalidation = true;
                        }
                        let is_config_toml = event
                            .file_name()
                            .is_some_and(|name| name.to_bytes() == CONFIG_FILE_BYTES);
                        if is_config_toml
                            && flags.intersects(ReadFlags::CLOSE_WRITE | ReadFlags::MOVED_TO)
                        {
                            saw_config_ready = true;
                        }
                    }
                    Err(Errno::AGAIN) => {
                        ready.clear_ready();
                        break;
                    }
                    Err(Errno::INTR) => continue,
                    Err(errno) => {
                        read_failed = Some(std::io::Error::from(errno));
                        break;
                    }
                }
            }
        }
        drop(ready);

        if let Some(err) = read_failed {
            tracing::warn!(generation, %err, "读取 im 配置目录 inotify 事件失败，标记当前监视失效");
            mark_invalidated(generation, &watcher);
            break;
        }

        if saw_self_invalidation {
            // 目录被删除或移走时按代次标记失效，不自动重建 `im/`，也不触发重新挂载。
            tracing::debug!(
                generation,
                "im 配置目录已被移除或替换，当前 inotify 监视代次失效"
            );
            mark_invalidated(generation, &watcher);
            break;
        }

        if saw_config_ready {
            let handle = remount_handle.clone();
            tokio::spawn(lattice_server::scoped_component_future(async move {
                trigger_config_remount(handle).await;
            }));
        }
    }
}

fn mark_invalidated(generation: u64, watcher: &std::sync::Weak<ImDirectoryWatcher>) {
    if let Some(strong) = watcher.upgrade() {
        strong.invalidate_generation(generation);
    }
}

async fn trigger_config_remount(handle: ComponentRemountHandle) {
    match handle.remount().await {
        Ok(ImMountResult::Mounted) => {
            tracing::info!(
                component = handle.name(),
                "检测到 im/config.toml 更新，组件已重新挂载"
            );
        }
        Ok(ImMountResult::Disabled) => {
            tracing::info!(
                component = handle.name(),
                "检测到 im/config.toml 更新，组件已停用"
            );
        }
        Ok(ImMountResult::UsingPreviousConfiguration { reason }) => {
            tracing::warn!(
                component = handle.name(),
                %reason,
                "检测到 im/config.toml 更新，但新配置无效，继续使用上一份配置运行"
            );
        }
        Ok(ImMountResult::Unmounted { reason }) => {
            tracing::warn!(
                component = handle.name(),
                %reason,
                "检测到 im/config.toml 更新，组件当前未挂载"
            );
        }
        Err(err) if err.kind == LocalErrorKind::Stopping => {
            tracing::debug!(
                component = handle.name(),
                "后台正在停机，跳过 im/config.toml 触发的重新挂载"
            );
        }
        Err(err) => {
            tracing::warn!(
                component = handle.name(),
                error = %err.message,
                "im/config.toml 触发重新挂载失败"
            );
        }
    }
}
