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
    },
    Unavailable(ApiError),
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
    creation_sync_probe: Mutex<Option<CreationSyncProbe>>,
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
                StoreState::Quiescing { .. } => return Err(StoreError::WriteDisabled.into()),
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
                    StoreState::Quiescing { .. } => return Err(StoreError::WriteDisabled.into()),
                    StoreState::Unavailable(error) => return Err(error.clone()),
                    StoreState::Unopened => {}
                }
            }
            let opened = manager.open_store(create);
            match opened {
                Ok(store) => {
                    let store = Arc::new(store);
                    let shutdown = Arc::default();
                    *manager.state.lock().expect("store manager poisoned") = StoreState::Ready {
                        generation: 0,
                        store: store.clone(),
                        shutdown: Arc::clone(&shutdown),
                    };
                    manager.notify();
                    Ok(manager.api(0, store, shutdown, read))
                }
                Err(error) => {
                    let missing = matches!(error, StoreError::NotInitialized { .. });
                    let unknown = matches!(error, StoreError::CreationDurabilityUnknown { .. });
                    let error = ApiError::from(error);
                    if unknown {
                        *manager.state.lock().expect("store manager poisoned") =
                            StoreState::Quiescing {
                                generation: 0,
                                reason: RecoveryReason::CreationDurabilityUnknown,
                            };
                        manager.recover(0, None);
                    } else if !missing {
                        *manager.state.lock().expect("store manager poisoned") =
                            StoreState::Unavailable(error.clone());
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

    pub(crate) fn fail(self: &Arc<Self>, generation: u64) {
        let previous = {
            let mut state = self.state.lock().expect("store manager poisoned");
            match &*state {
                StoreState::Ready {
                    generation: current,
                    shutdown,
                    ..
                } if *current == generation => shutdown.store(true, Ordering::Release),
                _ => return,
            }
            std::mem::replace(
                &mut *state,
                StoreState::Quiescing {
                    generation,
                    reason: RecoveryReason::Commit,
                },
            )
        };
        if let StoreState::Ready { store, .. } = previous {
            self.notify();
            self.recover(generation, Some(store));
        }
    }

    fn recover(self: &Arc<Self>, generation: u64, old: Option<Arc<SessionStore>>) {
        let manager = self.clone();
        tokio::spawn(async move {
            // 排他租约等所有真实 worker/Owner 释放；不依赖 Arc 计数、sleep 或 abort。
            let drained = manager.leases.clone().write_owned().await;
            #[cfg(feature = "test-crash-barriers")]
            crate::turn::crash_barrier::schedule(
                "store_reopen",
                serde_json::json!({"generation":generation}),
            )
            .await;
            let worker_manager = manager.clone();
            let result = tokio::task::spawn_blocking(move || {
                let _drained = drained;
                drop(old);
                let reason = {
                    let state = worker_manager.state.lock().expect("store manager poisoned");
                    match &*state {
                        StoreState::Quiescing {
                            generation: current,
                            reason,
                        } if *current == generation => *reason,
                        _ => panic!("reopen must replace its own quiescing generation"),
                    }
                };
                let opened = worker_manager
                    .open_store(matches!(reason, RecoveryReason::CreationDurabilityUnknown))
                    .map_err(ApiError::from);
                let mut state = worker_manager.state.lock().expect("store manager poisoned");
                *state = match opened {
                    Ok(store) => StoreState::Ready {
                        generation: generation + 1,
                        store: Arc::new(store),
                        shutdown: Arc::default(),
                    },
                    Err(error) => StoreState::Unavailable(error),
                };
            })
            .await;
            if result.is_err() {
                *manager.state.lock().expect("store manager poisoned") =
                    StoreState::Unavailable(ApiError::WorkerFailed);
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

    #[tokio::test]
    async fn readonly_missing_does_not_create_and_recovery_waits_for_every_lease() {
        let tmp = tempfile::tempdir().unwrap();
        let layout = StateLayout::in_state_root(tmp.path());
        let manager = StoreManager::new(layout.clone());
        assert!(
            matches!(manager.acquire(false).await, Err(ApiError::Store(error)) if matches!(error.as_ref(), lattice_session::store::StoreError::NotInitialized { .. }))
        );
        assert!(!layout.lattice_root().join("lattice.redb").exists());
        let first = manager.acquire(true).await.unwrap();
        let second = manager.acquire(false).await.unwrap();
        let shutdown = first.shutdown();
        let mut updates = manager.subscribe();
        first.report_failure(&ApiError::WorkerFailed);
        second.report_failure(&ApiError::WorkerFailed);
        assert!(shutdown.load(Ordering::Acquire));
        assert!(manager.acquire(false).await.is_err());
        let starts = std::sync::atomic::AtomicUsize::new(0);
        assert!(
            second
                .with_execution_start(|| {
                    starts.fetch_add(1, Ordering::SeqCst);
                })
                .is_err()
        );
        assert_eq!(starts.load(Ordering::SeqCst), 0);
        drop(first);
        assert!(matches!(
            *manager.state.lock().unwrap(),
            StoreState::Quiescing { generation: 0, .. }
        ));
        drop(second);
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            loop {
                if let Ok(api) = manager.acquire(false).await {
                    assert!(!api.shutdown().load(Ordering::Acquire));
                    assert!(
                        api.read(|read| read.list(None, 10))
                            .await
                            .unwrap()
                            .items
                            .is_empty()
                    );
                    break;
                }
                updates.changed().await.unwrap();
            }
        })
        .await
        .unwrap();
        assert!(matches!(
            *manager.state.lock().unwrap(),
            StoreState::Ready { generation: 1, .. }
        ));
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
}
