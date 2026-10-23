// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 唯一会话库的阻塞适配。操作锁覆盖整个 Turn；每次数据库事务只在阻塞线程内存在。

use std::{future::Future, sync::Arc};

use lancet_protocol::{InvocationBinding, InvocationId, ModelAttemptKey, SessionId};
use lattice_session::store::*;

#[derive(Debug, thiserror::Error)]
pub(crate) enum CoordinatorIoError {
    #[error("session store operation failed: {0}")]
    Store(#[from] StoreError),
    #[error("store worker failed: {0}")]
    Worker(#[from] tokio::task::JoinError),
}

type Result<T> = std::result::Result<T, CoordinatorIoError>;

pub(crate) trait IdSource: Send + Sync {
    fn new_invocation_id(&self) -> InvocationId;
}
pub(crate) struct RandomIdSource;
impl IdSource for RandomIdSource {
    fn new_invocation_id(&self) -> InvocationId {
        InvocationId::parse(&uuid::Uuid::new_v4().to_string()).expect("canonical UUID v4")
    }
}

pub(crate) struct CoordinatorStoreIo {
    api: crate::session::SessionApi,
    lock: Arc<SessionExecutionLock>,
    #[cfg(test)]
    pub(super) handoff_probe: Option<Arc<dyn Fn() + Send + Sync>>,
    #[cfg(test)]
    pub(super) finish_probe: Option<Arc<dyn Fn() + Send + Sync>>,
    #[cfg(test)]
    pub(super) close_fault: Option<TestCommitFault>,
}

impl CoordinatorStoreIo {
    #[cfg(test)]
    pub(crate) async fn acquire(store: Arc<SessionStore>, session_id: SessionId) -> Result<Self> {
        Self::acquire_api(crate::session::SessionApi::from_store(store), session_id).await
    }

    pub(crate) async fn acquire_api(
        api: crate::session::SessionApi,
        session_id: SessionId,
    ) -> Result<Self> {
        let locking = api.clone();
        let lock = tokio::task::spawn_blocking(move || locking.store.try_lock_session(session_id))
            .await??;
        Ok(Self {
            api,
            lock: Arc::new(lock),
            #[cfg(test)]
            handoff_probe: None,
            #[cfg(test)]
            finish_probe: None,
            #[cfg(test)]
            close_fault: None,
        })
    }

    pub(crate) fn session_id(&self) -> SessionId {
        self.lock.session_id()
    }

    async fn write<T: Send + 'static>(
        &self,
        operation: impl FnOnce(
            &SessionStore,
            &SessionExecutionLock,
        ) -> lattice_session::store::Result<T>
        + Send
        + 'static,
    ) -> Result<T> {
        let lock = self.lock.clone();
        self.blocking(move |api| operation(&api.store, &lock)).await
    }

    async fn blocking<T: Send + 'static>(
        &self,
        work: impl FnOnce(&crate::session::SessionApi) -> lattice_session::store::Result<T>
        + Send
        + 'static,
    ) -> Result<T> {
        let api = self.api.clone();
        let result = tokio::task::spawn_blocking(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| work(&api)));
            match result {
                Ok(result) => {
                    if matches!(
                        &result,
                        Err(StoreError::CommitFailed { .. }
                            | StoreError::WriteDisabled
                            | StoreError::CreationDurabilityUnknown { .. })
                    ) {
                        api.report_failure(&crate::session::ApiError::WorkerFailed);
                    }
                    result
                }
                Err(panic) => {
                    api.report_failure(&crate::session::ApiError::WorkerFailed);
                    std::panic::resume_unwind(panic)
                }
            }
        })
        .await;
        if result.is_err() {
            self.api
                .report_failure(&crate::session::ApiError::WorkerFailed);
        }
        Ok(result??)
    }

    pub(crate) async fn read<T: Send + 'static>(
        &self,
        operation: impl FnOnce(&SessionRead) -> lattice_session::store::Result<T> + Send + 'static,
    ) -> Result<T> {
        self.blocking(move |api| operation(&api.store.begin_read()?))
            .await
    }

    pub(crate) async fn accept(&self, input: AcceptTurn) -> Result<AcceptedTurn> {
        self.write(move |store, lock| store.accept_turn(lock, input))
            .await
    }
    pub(crate) async fn start_request(&self, key: ModelAttemptKey) -> Result<()> {
        self.write(move |store, lock| store.start_request(lock, key, chrono::Utc::now()))
            .await
    }
    pub(crate) async fn stop_attempt(&self, stopped: AttemptStopped) -> Result<AttemptStopResult> {
        self.write(move |store, lock| store.stop_attempt(lock, stopped, chrono::Utc::now()))
            .await
    }
    pub(crate) async fn register_calls(
        &self,
        reply: CompleteReply,
        candidates: Vec<InvocationId>,
    ) -> Result<RegisteredCalls> {
        self.write(move |store, lock| {
            store.register_calls(lock, reply, &candidates, chrono::Utc::now())
        })
        .await
    }
    pub(crate) async fn start_invocation(
        &self,
        binding: InvocationBinding,
    ) -> Result<StartedInvocation> {
        self.write(move |store, lock| store.start_invocation(lock, &binding, chrono::Utc::now()))
            .await
    }
    pub(crate) async fn finish_invocation(&self, terminal: TerminalInput) -> Result<()> {
        #[cfg(test)]
        let probe = self.finish_probe.clone();
        self.write(move |store, lock| {
            store.finish_invocation(lock, terminal, chrono::Utc::now())?;
            #[cfg(test)]
            if let Some(probe) = probe {
                probe();
            }
            Ok(())
        })
        .await
    }
    pub(crate) async fn close_step(&self, close: CloseStep) -> Result<ClosedStep> {
        #[cfg(test)]
        let fault = self.close_fault;
        self.write(move |store, lock| {
            #[cfg(test)]
            if let Some(fault) = fault {
                return with_test_commit_fault(fault, || {
                    store.close_step(lock, close, chrono::Utc::now())
                });
            }
            store.close_step(lock, close, chrono::Utc::now())
        })
        .await
    }

    /// 预构造 future 在健康锁内仅移交运行时。返回的 JoinHandle 必须由执行监督者真实排空。
    /// 不在该回调内取观察/准入锁、执行 I/O 或等待；Tool future 必须已经持有 StartedInvocation。
    pub(crate) async fn start_execution<F>(
        &self,
        execution: F,
        cancel: Arc<std::sync::atomic::AtomicBool>,
        deadline: tokio::time::Instant,
    ) -> Result<std::result::Result<tokio::task::JoinHandle<F::Output>, StopCause>>
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        let runtime = tokio::runtime::Handle::current();
        #[cfg(test)]
        let probe = self.handoff_probe.clone();
        self.blocking(move |api| {
            #[cfg(test)]
            if let Some(probe) = probe {
                probe();
            }
            api.with_execution_start(|| {
                // 健康锁等待结束后才检查预算；持久 Started 不等于已经交接。
                if cancel.load(std::sync::atomic::Ordering::Acquire) {
                    Err(StopCause::Cancelled)
                } else if tokio::time::Instant::now() >= deadline {
                    Err(StopCause::Deadline)
                } else {
                    Ok(runtime.spawn(execution))
                }
            })
        })
        .await
    }
}

pub(crate) struct TurnCoordinator {
    pub(crate) io: CoordinatorStoreIo,
    pub(crate) ids: Arc<dyn IdSource>,
}
impl TurnCoordinator {
    pub(crate) fn new(io: CoordinatorStoreIo, ids: Arc<dyn IdSource>) -> Self {
        Self { io, ids }
    }

    pub(crate) async fn register(&self, reply: CompleteReply) -> Result<RegisteredCalls> {
        let session = self.io.session_id();
        let turn = reply.key.turn_id;
        let catalog = self
            .io
            .read(move |read| Ok(read.turn_config(session, turn)?.catalog))
            .await?;
        if reply
            .message
            .tool_calls()
            .any(|(_, call)| !catalog.entries.iter().any(|entry| entry.name == call.name))
        {
            return Err(StoreError::InvalidInput {
                field: "reply",
                detail: "tool not in frozen catalog".into(),
            }
            .into());
        }
        loop {
            let candidates = reply
                .message
                .tool_calls()
                .map(|_| self.ids.new_invocation_id())
                .collect();
            match self.io.register_calls(reply.clone(), candidates).await {
                Err(CoordinatorIoError::Store(StoreError::InvocationCollision { .. })) => continue,
                result => return result,
            }
        }
    }

    pub(crate) async fn rewind(&self, target: NodeId) -> Result<SessionRecord> {
        let session = self
            .io
            .write(move |store, lock| store.rewind(lock, target, chrono::Utc::now()))
            .await?;
        #[cfg(feature = "test-crash-barriers")]
        super::crash_barrier::rewind_hit(&self.io, &session).await?;
        Ok(session)
    }
}

#[cfg(test)]
#[path = "coordinator_tests.rs"]
mod redb_tests;
