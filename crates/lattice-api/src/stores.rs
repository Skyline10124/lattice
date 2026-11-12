// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Backend 的当前库代管理；租约随真正的阻塞/执行 worker 存活。
use crate::session::{ApiError, Result, SessionApi};
use lattice_session::{
    StateLayout,
    store::{SessionStore, StoreError},
};
use std::sync::{
    Arc, Mutex, Weak,
    atomic::{AtomicBool, Ordering},
};
use tokio::sync::{OwnedRwLockReadGuard, RwLock, watch};

#[derive(Clone, Copy)]
enum RecoveryReason {
    Commit,
    CreationDurabilityUnknown,
}

enum StoreState {
    Unopened,
    Ready {
        generation: u64,
        store: Arc<SessionStore>,
        shutdown: Arc<AtomicBool>,
    },
    Quiescing {
        generation: u64,
        reason: RecoveryReason,
        old: Option<Arc<SessionStore>>,
    },
    Unavailable(ApiError),
    /// 停机排空超时后的终态；停写的库句柄留在这里，随 `StoreManager` 最终释放时关闭会话库。
    Stopped {
        _store: Option<Arc<SessionStore>>,
    },
}

pub(crate) struct StoreLease {
    pub(crate) generation: u64,
    pub(crate) manager: Weak<StoreManager>,
    pub(crate) shutdown: Arc<AtomicBool>,
    _read: OwnedRwLockReadGuard<()>,
}

pub(crate) struct StoreManager {
    layout: StateLayout,
    state: Mutex<StoreState>,
    leases: Arc<RwLock<()>>,
    opening: Arc<tokio::sync::Mutex<()>>,
    changed: watch::Sender<u64>,
    #[cfg(test)]
    pub(crate) creation_sync_probe: Mutex<Option<CreationSyncProbe>>,
}

impl StoreManager {
    pub(crate) fn new(layout: StateLayout) -> Arc<Self> {
        Arc::new(Self {
            layout,
            state: Mutex::new(StoreState::Unopened),
            leases: Arc::default(),
            opening: Arc::default(),
            changed: watch::channel(0).0,
            #[cfg(test)]
            creation_sync_probe: Mutex::default(),
        })
    }

    pub(crate) fn subscribe(&self) -> watch::Receiver<u64> {
        self.changed.subscribe()
    }

    pub(crate) fn is_stopped(&self) -> bool {
        matches!(
            *self.state.lock().expect("store manager poisoned"),
            StoreState::Stopped { .. }
        )
    }

    #[cfg(test)]
    pub(crate) fn ready_generation(&self) -> Option<u64> {
        match &*self.state.lock().unwrap() {
            StoreState::Ready { generation, .. } => Some(*generation),
            _ => None,
        }
    }

    fn open_store(&self, create: bool) -> lattice_session::store::Result<SessionStore> {
        let open = || {
            if create {
                SessionStore::create(&self.layout)
            } else {
                SessionStore::open(&self.layout)
            }
        };
        #[cfg(test)]
        if let Some(probe) = self.creation_sync_probe.lock().unwrap().clone() {
            return lattice_session::store::with_test_creation_sync_probe(move || probe(), open);
        }
        open()
    }

    fn notify(&self) {
        self.changed.send_modify(|version| *version += 1);
    }

    pub(crate) async fn acquire(self: &Arc<Self>, create: bool) -> Result<SessionApi> {
        let read = self
            .leases
            .clone()
            .try_read_owned()
            .map_err(|_| ApiError::Store(StoreError::WriteDisabled.into()))?;
        {
            let state = self.state.lock().expect("store manager poisoned");
            match &*state {
                StoreState::Ready {
                    generation,
                    store,
                    shutdown,
                } => return Ok(self.api(*generation, store.clone(), shutdown.clone(), read)),
                StoreState::Quiescing { .. } | StoreState::Stopped { .. } => {
                    return Err(StoreError::WriteDisabled.into());
                }
                StoreState::Unavailable(error) => return Err(error.clone()),
                StoreState::Unopened => {}
            }
        }
        let opening = self.opening.clone().lock_owned().await;
        let manager = self.clone();
        // 锁与租约移入真正 worker；调用者被取消不能提前释放它们或丢失已打开的库。
        tokio::task::spawn_blocking(move || {
            let _opening = opening;
            {
                let state = manager.state.lock().expect("store manager poisoned");
                match &*state {
                    StoreState::Ready {
                        generation,
                        store,
                        shutdown,
                    } => {
                        return Ok(manager.api(*generation, store.clone(), shutdown.clone(), read));
                    }
                    StoreState::Quiescing { .. } | StoreState::Stopped { .. } => {
                        return Err(StoreError::WriteDisabled.into());
                    }
                    StoreState::Unavailable(error) => return Err(error.clone()),
                    StoreState::Unopened => {}
                }
            }
            let opened = manager.open_store(create);
            match opened {
                Ok(store) => {
                    let shutdown = Arc::default();
                    let published = {
                        let mut state = manager.state.lock().expect("store manager poisoned");
                        if matches!(*state, StoreState::Stopped { .. }) {
                            Err(store)
                        } else {
                            let store = Arc::new(store);
                            *state = StoreState::Ready {
                                generation: 0,
                                store: store.clone(),
                                shutdown: Arc::clone(&shutdown),
                            };
                            Ok(store)
                        }
                    };
                    let store = match published {
                        Ok(store) => store,
                        Err(discarded) => {
                            drop(discarded);
                            return Err(StoreError::WriteDisabled.into());
                        }
                    };
                    manager.notify();
                    Ok(manager.api(0, store, shutdown, read))
                }
                Err(error) => {
                    let missing = matches!(error, StoreError::NotInitialized { .. });
                    let unknown = matches!(error, StoreError::CreationDurabilityUnknown { .. });
                    let error = ApiError::from(error);
                    if unknown {
                        let should_recover = {
                            let mut state = manager.state.lock().expect("store manager poisoned");
                            if matches!(*state, StoreState::Stopped { .. }) {
                                false
                            } else {
                                *state = StoreState::Quiescing {
                                    generation: 0,
                                    reason: RecoveryReason::CreationDurabilityUnknown,
                                    old: None,
                                };
                                true
                            }
                        };
                        if should_recover {
                            manager.recover(0);
                        }
                    } else if !missing {
                        let mut state = manager.state.lock().expect("store manager poisoned");
                        if !matches!(*state, StoreState::Stopped { .. }) {
                            *state = StoreState::Unavailable(error.clone());
                        }
                    }
                    Err(error)
                }
            }
        })
        .await
        .map_err(|_| ApiError::WorkerFailed)?
    }

    fn api(
        self: &Arc<Self>,
        generation: u64,
        store: Arc<SessionStore>,
        shutdown: Arc<AtomicBool>,
        read: OwnedRwLockReadGuard<()>,
    ) -> SessionApi {
        SessionApi::leased(
            store,
            Arc::new(StoreLease {
                generation,
                manager: Arc::downgrade(self),
                shutdown,
                _read: read,
            }),
        )
    }

    /// 停机排空超时后置位租约停机标志与会话库写闸，进入不再重开的终态。
    /// 停写的库句柄留在 `Stopped` 中随 `StoreManager` 最终释放，不在停写等待点顺带关库。
    pub(crate) async fn disable_writes(self: &Arc<Self>) {
        let store = {
            let mut state = self.state.lock().expect("store manager poisoned");
            let store = match &mut *state {
                StoreState::Stopped { .. } => return,
                StoreState::Ready {
                    store, shutdown, ..
                } => {
                    shutdown.store(true, Ordering::Release);
                    Some(store.clone())
                }
                StoreState::Quiescing { old, .. } => old.take(),
                StoreState::Unopened | StoreState::Unavailable(_) => None,
            };
            *state = StoreState::Stopped {
                _store: store.clone(),
            };
            store
        };
        self.notify();
        if let Some(store) = store {
            let _ = tokio::task::spawn_blocking(move || {
                store.disable_writes();
            })
            .await;
        }
    }

    pub(crate) fn fail(self: &Arc<Self>, generation: u64) {
        let should_recover = {
            let mut state = self.state.lock().expect("store manager poisoned");
            let store = match &*state {
                StoreState::Ready {
                    generation: current,
                    store,
                    shutdown,
                } if *current == generation => {
                    shutdown.store(true, Ordering::Release);
                    store.clone()
                }
                _ => return,
            };
            *state = StoreState::Quiescing {
                generation,
                reason: RecoveryReason::Commit,
                old: Some(store),
            };
            true
        };
        if should_recover {
            self.notify();
            self.recover(generation);
        }
    }

    fn recover(self: &Arc<Self>, generation: u64) {
        let manager = self.clone();
        tokio::spawn(async move {
            // 排他租约等所有真实 worker/Owner 释放；不依赖 Arc 计数、sleep 或 abort。
            let drained = manager.leases.clone().write_owned().await;
            if manager.is_stopped() {
                drop(drained);
                manager.notify();
                return;
            }
            #[cfg(feature = "test-crash-barriers")]
            crate::turn::crash_barrier::schedule(
                "store_reopen",
                serde_json::json!({"generation":generation}),
            )
            .await;
            let worker_manager = manager.clone();
            let result = tokio::task::spawn_blocking(move || {
                let _drained = drained;
                let (reason, old) = {
                    let mut state = worker_manager.state.lock().expect("store manager poisoned");
                    match &mut *state {
                        StoreState::Stopped { .. } => return,
                        StoreState::Quiescing {
                            generation: current,
                            reason,
                            old,
                        } if *current == generation => (*reason, old.take()),
                        _ => panic!("reopen must replace its own quiescing generation"),
                    }
                };
                drop(old);
                let opened = worker_manager
                    .open_store(matches!(reason, RecoveryReason::CreationDurabilityUnknown))
                    .map_err(ApiError::from);
                let discarded = {
                    let mut state = worker_manager.state.lock().expect("store manager poisoned");
                    if matches!(*state, StoreState::Stopped { .. }) {
                        opened.ok()
                    } else {
                        *state = match opened {
                            Ok(store) => StoreState::Ready {
                                generation: generation + 1,
                                store: Arc::new(store),
                                shutdown: Arc::default(),
                            },
                            Err(error) => StoreState::Unavailable(error),
                        };
                        None
                    }
                };
                drop(discarded);
            })
            .await;
            if result.is_err() {
                let mut state = manager.state.lock().expect("store manager poisoned");
                if !matches!(*state, StoreState::Stopped { .. }) {
                    *state = StoreState::Unavailable(ApiError::WorkerFailed);
                }
            }
            manager.notify();
        });
    }
}

#[cfg(test)]
type CreationSyncProbe = Arc<dyn Fn() -> bool + Send + Sync>;

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;

    #[tokio::test]
    async fn creation_unknown_reconfirms_parent_before_ready_and_preserves_failed_target() {
        use std::{os::unix::fs::MetadataExt, sync::atomic::AtomicUsize};
        for failures in [1, 2] {
            let tmp = tempfile::tempdir().unwrap();
            let layout = StateLayout::in_state_root(tmp.path());
            let manager = StoreManager::new(layout.clone());
            let attempts = Arc::new(AtomicUsize::new(0));
            *manager.creation_sync_probe.lock().unwrap() = Some(Arc::new({
                let attempts = attempts.clone();
                move || attempts.fetch_add(1, Ordering::SeqCst) < failures
            }));
            // 真实旧代租约未排空前恢复不能重开。
            let lease = manager.leases.clone().read_owned().await;
            let error = manager
                .acquire(true)
                .await
                .err()
                .expect("injected creation failure");
            assert!(
                matches!(error, ApiError::Store(ref error) if matches!(error.as_ref(), StoreError::CreationDurabilityUnknown { .. }))
            );
            let target = layout.lattice_root().join("lattice.redb");
            let inode = std::fs::metadata(&target).unwrap().ino();
            assert_eq!(attempts.load(Ordering::SeqCst), 1);
            assert!(matches!(
                *manager.state.lock().unwrap(),
                StoreState::Quiescing { .. }
            ));
            let mut updates = manager.subscribe();
            drop(lease);
            tokio::time::timeout(std::time::Duration::from_secs(3), async {
                loop {
                    if matches!(
                        *manager.state.lock().unwrap(),
                        StoreState::Ready { .. } | StoreState::Unavailable(_)
                    ) {
                        break;
                    }
                    updates.changed().await.unwrap();
                }
            })
            .await
            .unwrap();
            assert_eq!(std::fs::metadata(&target).unwrap().ino(), inode);
            assert_eq!(
                attempts.load(Ordering::SeqCst),
                2,
                "Ready must not bypass the parent directory barrier"
            );
            if failures == 1 {
                let api = manager.acquire(false).await.unwrap();
                assert_eq!(manager.ready_generation(), Some(1));
                assert!(
                    api.read(|r| r.list(None, 10))
                        .await
                        .unwrap()
                        .items
                        .is_empty()
                );
            } else {
                assert!(manager.acquire(false).await.is_err());
                assert!(matches!(
                    *manager.state.lock().unwrap(),
                    StoreState::Unavailable(_)
                ));
            }
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn abandoned_async_reader_retains_lease_until_blocking_worker_really_exits() {
        let tmp = tempfile::tempdir().unwrap();
        let manager = StoreManager::new(StateLayout::in_state_root(tmp.path()));
        let api = manager.acquire(true).await.unwrap();
        let reader = api.clone();
        let (entered_tx, entered) = tokio::sync::oneshot::channel();
        let (release, wait) = std::sync::mpsc::channel();
        let task = tokio::spawn(async move {
            reader
                .read(move |read| {
                    entered_tx.send(()).unwrap();
                    wait.recv_timeout(std::time::Duration::from_secs(3))
                        .unwrap();
                    read.list(None, 10)
                })
                .await
        });
        entered.await.unwrap();
        api.report_failure(&ApiError::WorkerFailed);
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        drop(api);
        assert!(
            manager.leases.clone().try_write_owned().is_err(),
            "actual reader still owns the generation"
        );
        assert!(matches!(
            *manager.state.lock().unwrap(),
            StoreState::Quiescing { .. }
        ));
        let mut updates = manager.subscribe();
        release.send(()).unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            loop {
                if manager.acquire(false).await.is_ok() {
                    break;
                }
                updates.changed().await.unwrap();
            }
        })
        .await
        .unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn abandoned_worker_panic_still_requests_reopen() {
        let tmp = tempfile::tempdir().unwrap();
        let manager = StoreManager::new(StateLayout::in_state_root(tmp.path()));
        let api = manager.acquire(true).await.unwrap();
        let reader = api.clone();
        let (entered_tx, entered) = tokio::sync::oneshot::channel();
        let (release, wait) = std::sync::mpsc::channel();
        let task = tokio::spawn(async move {
            reader
                .read::<()>(move |_| {
                    entered_tx.send(()).unwrap();
                    wait.recv_timeout(std::time::Duration::from_secs(3))
                        .unwrap();
                    panic!("controlled worker failure");
                })
                .await
        });
        entered.await.unwrap();
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        let mut updates = manager.subscribe();
        release.send(()).unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(3), updates.changed())
            .await
            .expect("abandoned worker must report failure")
            .unwrap();
        assert!(api.shutdown().load(Ordering::Acquire));
        assert!(matches!(
            *manager.state.lock().unwrap(),
            StoreState::Quiescing { .. }
        ));
        drop(api);
    }

    #[tokio::test]
    async fn disable_writes_from_ready_marks_shutdown_blocks_store_and_stops_acquire() {
        let tmp = tempfile::tempdir().unwrap();
        let manager = StoreManager::new(StateLayout::in_state_root(tmp.path()));
        let api = manager.acquire(true).await.expect("首次创建会话库");
        let shutdown = api.shutdown().clone();
        let store_weak = Arc::downgrade(&api.store);
        assert!(!shutdown.load(Ordering::Acquire));
        drop(api);
        assert_eq!(
            store_weak.strong_count(),
            1,
            "释放租约后应只剩 StoreManager 持有会话库句柄"
        );

        manager.disable_writes().await;

        assert!(
            shutdown.load(Ordering::Acquire),
            "Ready 停写后租约 shutdown 应已置位"
        );
        let retained = store_weak
            .upgrade()
            .expect("停写阻塞任务结束时不应顺带丢掉最后一个库引用");
        assert!(
            matches!(
                retained.with_execution_start(|| ()),
                Err(StoreError::WriteDisabled)
            ),
            "Ready 停写后底层会话库的写闸应已置位"
        );
        drop(retained);
        assert_eq!(
            store_weak.strong_count(),
            1,
            "Stopped 状态应保留库句柄直至 StoreManager 最终释放"
        );
        let err = manager
            .acquire(false)
            .await
            .err()
            .expect("Stopped 状态下 acquire 应失败");
        assert!(
            matches!(err, ApiError::Store(ref e) if matches!(e.as_ref(), StoreError::WriteDisabled)),
            "Stopped 状态下 acquire 应返回 WriteDisabled"
        );
        drop(manager);
        assert_eq!(
            store_weak.strong_count(),
            0,
            "最终释放 StoreManager 后会话库句柄应一并释放"
        );
    }

    #[tokio::test]
    async fn disable_writes_while_quiescing_disables_old_store_and_prevents_reopen() {
        let tmp = tempfile::tempdir().unwrap();
        let manager = StoreManager::new(StateLayout::in_state_root(tmp.path()));
        let api = manager.acquire(true).await.expect("首次创建会话库");

        // WorkerFailed 触发 Quiescing 且不会自行置位旧库写闸；api 持有租约阻止恢复立即重开。
        api.report_failure(&ApiError::WorkerFailed);
        assert!(
            matches!(*manager.state.lock().unwrap(), StoreState::Quiescing { .. }),
            "汇报 WorkerFailed 后应进入 Quiescing"
        );
        assert!(
            api.store.with_execution_start(|| ()).is_ok(),
            "停写前 WorkerFailed 旧库的写闸尚未置位"
        );

        manager.disable_writes().await;

        assert!(
            matches!(
                api.store.with_execution_start(|| ()),
                Err(StoreError::WriteDisabled)
            ),
            "Quiescing 停写应置位旧库的写闸"
        );
        assert!(
            matches!(
                *manager.state.lock().unwrap(),
                StoreState::Stopped { _store: Some(_) }
            ),
            "停写后状态应为保留旧库句柄的 Stopped"
        );

        let mut updates = manager.subscribe();
        drop(api);
        tokio::time::timeout(std::time::Duration::from_secs(3), updates.changed())
            .await
            .expect("释放租约后等待中的恢复任务应退出并发送通知")
            .unwrap();

        assert!(
            matches!(
                *manager.state.lock().unwrap(),
                StoreState::Stopped { _store: Some(_) }
            ),
            "放开租约后状态仍应保持 Stopped，不能重开"
        );
        assert_eq!(manager.ready_generation(), None);
        let err = manager
            .acquire(false)
            .await
            .err()
            .expect("恢复退出后 acquire 仍应拒绝");
        assert!(
            matches!(err, ApiError::Store(ref e) if matches!(e.as_ref(), StoreError::WriteDisabled)),
            "Stopped 状态下 acquire 应返回 WriteDisabled"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn disable_writes_during_initial_open_keeps_stopped_and_discards_opened_store() {
        let tmp = tempfile::tempdir().unwrap();
        let layout = StateLayout::in_state_root(tmp.path());
        let manager = StoreManager::new(layout.clone());
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let gate = Mutex::new(Some((entered_tx, release_rx)));
        *manager.creation_sync_probe.lock().unwrap() = Some(Arc::new(move || {
            if let Some((entered_tx, release_rx)) = gate.lock().unwrap().take() {
                entered_tx.send(()).unwrap();
                release_rx
                    .recv_timeout(std::time::Duration::from_secs(3))
                    .unwrap();
            }
            false
        }));

        let open_task = tokio::spawn({
            let manager = manager.clone();
            async move { manager.acquire(true).await }
        });

        tokio::task::spawn_blocking(move || {
            entered_rx
                .recv_timeout(std::time::Duration::from_secs(3))
                .expect("首次打开应进入 creation_sync_probe");
        })
        .await
        .unwrap();

        manager.disable_writes().await;
        assert!(
            matches!(
                *manager.state.lock().unwrap(),
                StoreState::Stopped { _store: None }
            ),
            "首次打开进行中停写应将状态置为 Stopped"
        );

        release_tx.send(()).unwrap();
        let err = open_task
            .await
            .unwrap()
            .err()
            .expect("停写后首次打开应返回错误");
        assert!(
            matches!(err, ApiError::Store(ref e) if matches!(e.as_ref(), StoreError::WriteDisabled)),
            "首次打开遇到 Stopped 应返回 WriteDisabled"
        );
        assert!(
            matches!(
                *manager.state.lock().unwrap(),
                StoreState::Stopped { _store: None }
            ),
            "打开结束后状态仍应保持 Stopped"
        );

        let reopened =
            SessionStore::open(&layout).expect("刚打开的库句柄应已被丢弃并释放底层文件锁");
        drop(reopened);
    }
}
