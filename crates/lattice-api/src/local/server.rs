// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use super::{ipc, types::*};
use futures::{FutureExt, StreamExt};
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::{
    net::{UnixListener, UnixStream},
    sync::{broadcast, oneshot, watch},
    task::JoinSet,
};

type CoreCompletion = (
    oneshot::Receiver<crate::session::Result<crate::TurnCompletion>>,
    Arc<AtomicU64>,
);

// 只由核心的身份核验或恢复结果建立；请求自报的 TurnId 不能成为完成确认的归属。
struct OwnedTurn {
    id: TurnId,
    completion: Option<Result<()>>,
}

struct Owner {
    identity: Option<TurnRequest>,
    reply: watch::Sender<Option<Response>>,
    done: watch::Sender<Option<Result<()>>>,
    events: broadcast::Sender<crate::turn::SequencedTurnEvent>,
    sequence: AtomicU64,
    execution_cancel: Option<Arc<AtomicBool>>,
    // 即使转发观察或准备任务 panic，监督者也必须等待已经接管的核心执行结束。
    core: Mutex<Option<CoreCompletion>>,
    turn: Mutex<Option<OwnedTurn>>,
    verified_turn: Arc<std::sync::OnceLock<TurnId>>,
}

impl Owner {
    fn new(identity: Option<TurnRequest>) -> Self {
        Self {
            identity,
            reply: watch::channel(None).0,
            done: watch::channel(None).0,
            events: broadcast::channel(64).0,
            sequence: AtomicU64::new(0),
            execution_cancel: None,
            core: Mutex::new(None),
            turn: Mutex::new(None),
            verified_turn: Arc::default(),
        }
    }
    fn source_sequence(&self) -> u64 {
        let forwarded = self.sequence.load(Ordering::Acquire);
        lock(&self.core)
            .as_ref()
            .map_or(forwarded, |(_, sequence)| {
                forwarded.max(sequence.load(Ordering::Acquire))
            })
    }
    fn reply(&self, response: Response) {
        self.reply.send_replace(Some(response));
    }
}

#[derive(Default)]
struct Admission {
    stopping: bool,
    owners: HashMap<String, Arc<Owner>>,
    // 仅记录本进程的完成确认；缺失不证明可见 pin 已通过发布屏障。
    completions: HashMap<(String, TurnId), Result<()>>,
    unconfirmed_operations: usize,
}

struct Backend {
    layout: crate::StateLayout,
    epoch: String,
    admission: Mutex<Admission>,
    closed: watch::Sender<bool>,
    session: tokio::sync::OnceCell<crate::SessionApi>,
    subscriptions: tokio::sync::Semaphore,
    #[cfg(test)]
    snapshot_gate: Mutex<Option<(oneshot::Sender<()>, oneshot::Receiver<()>)>>,
    #[cfg(test)]
    gates: cancellation_tests::Gates,
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn internal() -> LocalError {
    LocalError::new(
        LocalErrorKind::Internal,
        "backend operation failed; inspect durable state",
    )
}
fn busy() -> LocalError {
    LocalError::new(LocalErrorKind::Busy, "session has another owned operation")
}

async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T> + Send + 'static,
) -> Result<T> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|_| internal())?
}

impl Backend {
    fn new(layout: crate::StateLayout) -> Arc<Self> {
        Arc::new(Self {
            layout,
            epoch: uuid::Uuid::new_v4().to_string(),
            admission: Mutex::default(),
            closed: watch::channel(false).0,
            session: tokio::sync::OnceCell::new(),
            subscriptions: tokio::sync::Semaphore::new(32),
            #[cfg(test)]
            snapshot_gate: Mutex::new(None),
            #[cfg(test)]
            gates: cancellation_tests::Gates::default(),
        })
    }
    fn status(&self) -> DaemonStatus {
        let state = lock(&self.admission);
        DaemonStatus {
            epoch: self.epoch.clone(),
            stopping: state.stopping,
            owners: state.owners.len(),
            unconfirmed_operations: state.unconfirmed_operations,
        }
    }
    async fn open(&self) -> Result<crate::SessionApi> {
        self.session
            .get_or_try_init(|| async {
                let layout = self.layout.clone();
                blocking(move || crate::SessionApi::git_open_layout(&layout).map_err(Into::into))
                    .await
            })
            .await
            .cloned()
    }
    async fn query(
        &self,
        session_id: String,
        turn_id: Option<TurnId>,
    ) -> Result<SessionObservation> {
        #[cfg(test)]
        {
            let gate = lock(&self.snapshot_gate).take();
            if let Some((entered, release)) = gate {
                let _ = entered.send(());
                let _ = release.await;
            }
        }
        let api = self.open().await?;
        blocking(move || {
            api.git_store_for_coordinator()?
                .observe(&session_id, turn_id)
                .map_err(crate::session::map_session_error)
                .map_err(Into::into)
        })
        .await
    }

    /// 观察端确认入口：按 session+turn 核验固定 pin 下的已关闭 TurnCommit
    /// 并补足 pending 引用 fsync 屏障。核验与屏障都在短仓库写锁内完成，
    /// 不取得整个 Session 执行锁，也不发布引用或生成候选；真实存储错误
    /// 经结构化映射原样上报，不用“缓存空/锁忙”顶替。
    async fn confirm_committed(&self, session_id: String, turn_id: TurnId) -> Result<()> {
        let api = self.open().await?;
        blocking(move || {
            api.git_store_for_coordinator()?
                .confirm_committed_turn(&session_id, &turn_id)
                .map_err(crate::session::map_session_error)
                .map_err(Into::into)
        })
        .await?;
        Ok(())
    }
    fn cursor(&self, sequence: u64) -> EventCursor {
        EventCursor {
            epoch: self.epoch.clone(),
            sequence,
        }
    }

    fn claim(
        self: &Arc<Self>,
        key: String,
        identity: Option<TurnRequest>,
        operation: impl FnOnce() -> Request,
    ) -> Result<Arc<Owner>> {
        let (owner, operation) = {
            // 只管理接管权；磁盘、Component 和 provider 工作全部在释放此锁之后进行。
            let mut state = lock(&self.admission);
            if let Some(owner) = state.owners.get(&key) {
                return if identity.is_some() && owner.identity == identity {
                    Ok(owner.clone())
                } else if identity
                    .as_ref()
                    .zip(owner.identity.as_ref())
                    .is_some_and(|(a, b)| a.turn_id == b.turn_id)
                {
                    Err(LocalError::new(
                        LocalErrorKind::Conflict,
                        "TurnId is bound to different input or workspace",
                    ))
                } else {
                    Err(busy())
                };
            }
            if state.stopping {
                return Err(LocalError::new(
                    LocalErrorKind::Stopping,
                    "backend is draining",
                ));
            }
            if state.owners.len() >= 32 {
                return Err(busy());
            }
            let operation = operation();
            let mut owner = Owner::new(identity);
            // 只为实际接管的配置建立取消载体；共享者保留原操作的资格和配置。
            if matches!(
                &operation,
                Request::Submit {
                    configuration: Some(_),
                    ..
                }
            ) {
                owner.execution_cancel = Some(Arc::default());
            }
            let owner = Arc::new(owner);
            state.owners.insert(key.clone(), owner.clone());
            (owner, operation)
        };
        let backend = self.clone();
        let task_owner = owner.clone();
        tokio::spawn(async move {
            let result = std::panic::AssertUnwindSafe(backend.perform(operation, &task_owner))
                .catch_unwind()
                .await
                .unwrap_or_else(|_| Err(internal()));
            #[cfg(test)]
            cancellation_tests::pause(&backend.gates.supervise).await;
            // core receiver 不属于上面的可展开 future，转发 panic 不会遗失排空凭据。
            let completion = lock(&task_owner.core).take();
            let mut final_result = result;
            if let Some((completion, sequence)) = completion {
                match completion
                    .await
                    .map_err(|_| internal())
                    .and_then(|result| result.map_err(Into::into))
                {
                    Err(error) => final_result = Err(error),
                    Ok(crate::TurnCompletion::Pending(_)) => {
                        final_result = Err(LocalError::new(
                            LocalErrorKind::RecoveryRequired,
                            "Turn remains pending",
                        ))
                    }
                    Ok(crate::TurnCompletion::Committed(commit)) => {
                        *lock(&task_owner.turn) = Some(OwnedTurn {
                            id: commit.turn_id,
                            completion: Some(Ok(())),
                        });
                    }
                }
                if let Some(turn) = lock(&task_owner.turn).as_mut()
                    && turn.completion.is_none()
                {
                    turn.completion = Some(final_result.clone());
                }
                task_owner
                    .sequence
                    .store(sequence.load(Ordering::Acquire), Ordering::Release);
            }
            if let Some(id) = task_owner.verified_turn.get() {
                let mut turn = lock(&task_owner.turn);
                let turn = turn.get_or_insert(OwnedTurn {
                    id: *id,
                    completion: None,
                });
                if turn.completion.is_none() {
                    turn.completion = Some(final_result.clone());
                }
            }
            if task_owner.reply.borrow().is_none() {
                task_owner.reply(Response::Error(
                    final_result.clone().err().unwrap_or_else(internal),
                ));
            }
            #[cfg(test)]
            cancellation_tests::pause(&backend.gates.retire).await;
            {
                let mut state = lock(&backend.admission);
                if final_result.as_ref().err().is_some_and(|error| {
                    matches!(
                        error.kind,
                        LocalErrorKind::RecoveryRequired
                            | LocalErrorKind::StoreUnavailable
                            | LocalErrorKind::Internal
                    )
                }) {
                    state.unconfirmed_operations += 1;
                }
                if let Some(turn) = lock(&task_owner.turn).take() {
                    state.completions.insert(
                        (key.clone(), turn.id),
                        turn.completion.unwrap_or_else(|| Err(internal())),
                    );
                }
                state.owners.remove(&key);
            }
            task_owner.done.send_replace(Some(final_result));
        });
        Ok(owner)
    }

    async fn close_pending(&self, api: crate::SessionApi, session_id: &str) -> Result<()> {
        if api.read_view(session_id, None).await?.active_turn.is_none() {
            return Ok(());
        }
        // 关闭诊断单独归属旧任务，不占用随后新提交的 Owner 身份或事件通道。
        let target = Arc::default();
        let result =
            crate::turn::recover_turn_with_target(api, session_id.to_owned(), Arc::clone(&target))
                .await
                .map_err(LocalError::from);
        if let Some(id) = target.get() {
            lock(&self.admission).completions.insert(
                (session_id.to_owned(), *id),
                result.as_ref().map(|_| ()).map_err(Clone::clone),
            );
        }
        result.map(|_| ())
    }

    async fn perform(&self, operation: Request, owner: &Owner) -> Result<()> {
        match operation {
            Request::Submit {
                request,
                configuration,
            } => self.submit(request, configuration, owner).await,
            Request::Create { request } => {
                let layout = self.layout.clone();
                let session = blocking(move || {
                    let api = crate::SessionApi::git_create_layout(&layout)?;
                    api.create_session(request).map_err(Into::into)
                })
                .await?;
                owner.reply(Response::Session(session));
                Ok(())
            }
            Request::Write { session_id, change } => {
                let api = self.open().await?;
                self.close_pending(api.clone(), &session_id).await?;
                let id = session_id.clone();
                let result = blocking(move || {
                    let (id, execution_lock) = api.try_lock_execution(&id)?;
                    Ok(match change {
                        SessionWrite::Checkout { target } => SessionWriteResult::Checkout(
                            api.checkout_locked(&id, &target, &execution_lock)?,
                        ),
                        SessionWrite::Branch { name, from } => SessionWriteResult::Branch(
                            api.branch_locked(&id, &name, from.as_deref(), &execution_lock)?,
                        ),
                        SessionWrite::Reset { target } => SessionWriteResult::Reset(
                            api.reset_locked(&id, &target, &execution_lock)?,
                        ),
                        SessionWrite::SetAgent { name } => SessionWriteResult::SetAgent(
                            api.git_store_for_coordinator()?
                                .set_active_agent_locked(&id, &name, &execution_lock)
                                .map_err(crate::session::map_session_error)?,
                        ),
                    })
                })
                .await?;
                owner.reply(Response::Write(result));
                Ok(())
            }
            Request::Rewind {
                session_id,
                target,
                workspace,
            } => {
                let api = self.open().await?;
                self.close_pending(api.clone(), &session_id).await?;
                let result = blocking(move || {
                    match crate::rewind_to_turn(&api, &session_id, &target, &workspace) {
                        Ok(crate::RewindOutcome::Success(value)) => {
                            Ok(LocalRewindResult::Success(value))
                        }
                        Ok(crate::RewindOutcome::Conflict(value)) => {
                            Ok(LocalRewindResult::Conflict(value))
                        }
                        Err(crate::RewindError::AuditPublicationFailed { partial, .. }) => {
                            Ok(LocalRewindResult::AuditPublicationFailed(partial))
                        }
                        Err(crate::RewindError::Api(error)) => Err(error.into()),
                    }
                })
                .await?;
                let unconfirmed = matches!(result, LocalRewindResult::AuditPublicationFailed(_));
                owner.reply(Response::Rewind(result));
                if unconfirmed { Err(internal()) } else { Ok(()) }
            }
            _ => Err(internal()),
        }
    }

    async fn submit(
        &self,
        request: TurnRequest,
        configuration: Option<Box<super::FrozenConfiguration>>,
        owner: &Owner,
    ) -> Result<()> {
        #[cfg(test)]
        cancellation_tests::pause(&self.gates.prepare).await;
        let api = self.open().await?;
        let path = request.workspace.clone();
        let workspace = blocking(move || {
            let root = crate::LocalWorkspaceRoot::from_path(&path)?;
            if root.canonical_path() != path {
                return Err(LocalError::new(
                    LocalErrorKind::BadRequest,
                    "workspace must be canonical",
                ));
            }
            Ok(root)
        })
        .await?;
        let view = self
            .query(request.session_id.clone(), Some(request.turn_id))
            .await?;
        if view.session.project_store_key != workspace.store_key_hex() {
            return Err(LocalError::new(
                LocalErrorKind::Conflict,
                "workspace differs from session binding",
            ));
        }
        // 同请求只确认原事实；不同旧任务先关闭，再继续本次配置握手。
        if view.requested_turn.is_none() && view.active_turn.is_some() {
            self.close_pending(api.clone(), &request.session_id).await?;
        }
        let (loaded_profile, connection) = if view.requested_turn.is_some() {
            (None, None)
        } else if let Some(configuration) = configuration {
            let (profile, connection) = blocking(move || configuration.into_execution()).await?;
            (Some(profile), Some(connection))
        } else {
            owner.reply(Response::Acceptance(Acceptance::NeedsConfiguration));
            return Ok(());
        };
        let execution = crate::turn::start_execution(
            api,
            request.turn_id,
            workspace,
            crate::RunTurnRequest {
                input: request.input,
                session_id: request.session_id.clone(),
                loaded_profile,
                cancel_flag: owner.execution_cancel.clone(),
                #[cfg(any(test, feature = "test-utils"))]
                test_llm: None,
                #[cfg(any(test, feature = "test-utils"))]
                test_project_root: None,
            },
            connection,
            false,
            owner.verified_turn.clone(),
        );
        let crate::TurnExecution {
            acceptance,
            completion,
            mut events,
            event_sequence,
            ..
        } = execution;
        *lock(&owner.core) = Some((completion, event_sequence.clone()));
        #[cfg(test)]
        cancellation_tests::fail_forwarding(&self.gates.forward_panic).await;
        let accepted = acceptance.await.map_err(|_| internal())??;
        let id = match &accepted {
            crate::TurnAcceptance::Accepted { turn_id }
            | crate::TurnAcceptance::ConfirmationUnknown { turn_id } => *turn_id,
            crate::TurnAcceptance::Existing(turn) => turn.started().turn_id,
        };
        *lock(&owner.turn) = Some(OwnedTurn {
            id,
            completion: None,
        });
        let response = match accepted {
            crate::TurnAcceptance::Accepted { turn_id } => Acceptance::Accepted { turn_id },
            crate::TurnAcceptance::ConfirmationUnknown { turn_id } => {
                Acceptance::ConfirmationUnknown { turn_id }
            }
            crate::TurnAcceptance::Existing(_) => Acceptance::Existing {
                turn: self
                    .query(request.session_id.clone(), Some(request.turn_id))
                    .await?
                    .requested_turn
                    .ok_or_else(internal)?,
            },
        };
        owner.reply(Response::Acceptance(response));
        while let Some(event) = events.next().await {
            owner.sequence.store(event.sequence, Ordering::Release);
            // 大事件不挤占订阅者队列，缺失序号与最终序号会明确报告 Gap。
            if serde_json::to_vec(&event).map_err(|_| internal())?.len() <= 65_536 {
                let _ = owner.events.send(event);
            }
        }
        owner
            .sequence
            .store(event_sequence.load(Ordering::Acquire), Ordering::Release);
        Ok(())
    }

    async fn dispatch(self: &Arc<Self>, request: Request) -> Result<Response> {
        match request {
            Request::Status => Ok(Response::Status(self.status())),
            Request::History {
                session_id,
                head,
                offset,
                limit,
            } => {
                let api = self.open().await?;
                Ok(Response::History(
                    blocking(move || {
                        api.git_store_for_coordinator()?
                            .history_page(&session_id, &head, offset, limit)
                            .map_err(crate::session::map_session_error)
                            .map_err(Into::into)
                    })
                    .await?,
                ))
            }
            Request::List => {
                let api = self.open().await?;
                Ok(Response::Sessions(
                    blocking(move || api.list_sessions().map_err(Into::into)).await?,
                ))
            }
            Request::SessionStatus { session_id } => {
                let api = self.open().await?;
                let id = blocking(move || {
                    api.open_session(crate::OpenSessionRequest { id: session_id })
                        .map(|session| session.session_id)
                        .map_err(Into::into)
                })
                .await?;
                let view = self.query(id, None).await?;
                Ok(Response::SessionStatus(crate::SessionStatus {
                    session_id: view.session.session_id,
                    head: view.head,
                    current_commit_id: view.head_commit_id,
                    active_agent_name: view.session.active_agent_name,
                    branches: view.branches,
                    commit_count: view.commit_count,
                    project_target_branch: view.session.project_target_branch,
                    project_store_key: view.session.project_store_key,
                }))
            }
            Request::Log { session_id } => {
                let api = self.open().await?;
                Ok(Response::Log(
                    blocking(move || api.log(&session_id).map_err(Into::into)).await?,
                ))
            }
            Request::Diff {
                session_id,
                from,
                to,
            } => {
                let api = self.open().await?;
                Ok(Response::Diff(
                    blocking(move || api.diff(&session_id, &from, &to).map_err(Into::into)).await?,
                ))
            }
            Request::Stop => {
                lock(&self.admission).stopping = true;
                while self.status().owners != 0 {
                    tokio::time::sleep(Duration::from_millis(25)).await;
                }
                Ok(Response::Status(self.status()))
            }
            Request::Query {
                session_id,
                turn_id,
            } => Ok(Response::View(Box::new(
                self.query(session_id, turn_id).await?,
            ))),
            Request::Open { session_id } => {
                let api = self.open().await?;
                Ok(Response::Session(
                    blocking(move || {
                        api.open_session(crate::OpenSessionRequest { id: session_id })
                            .map_err(Into::into)
                    })
                    .await?,
                ))
            }
            Request::Commit {
                session_id,
                commit_id,
            } => {
                let api = self.open().await?;
                let result = blocking(move || {
                    api.git_store_for_coordinator()?
                        .observe_commit(&session_id, &commit_id)
                        .map_err(crate::session::map_session_error)
                        .map_err(Into::into)
                })
                .await?;
                Ok(Response::Commit(Box::new(result)))
            }
            Request::Transcript {
                session_id,
                commit_id,
                offset,
                limit,
            } => {
                let api = self.open().await?;
                Ok(Response::Transcript(
                    api.read_transcript_page(&session_id, &commit_id, offset, limit)
                        .await?,
                ))
            }
            Request::Cancel {
                session_id,
                turn_id,
            } => {
                let owned = {
                    // 在同一准入临界区内接管取消；监督者不能在置位前移除当前操作。
                    let state = lock(&self.admission);
                    state
                        .owners
                        .get(&session_id)
                        .filter(|owner| {
                            owner
                                .identity
                                .as_ref()
                                .is_some_and(|request| request.turn_id == turn_id)
                        })
                        .and_then(|owner| owner.execution_cancel.as_ref())
                        .is_some_and(|cancel| {
                            #[cfg(test)]
                            self.gates.pause_cancel_store();
                            cancel.store(true, Ordering::Release);
                            true
                        })
                };
                // 未接管时仍查询持久状态，但绝不顺带恢复或重新执行。
                if !owned {
                    self.query(session_id, Some(turn_id)).await?;
                }
                Ok(Response::Cancelled { owned })
            }
            request @ Request::Create { .. } => {
                self.wait_reply(self.claim(
                    format!("create:{}", uuid::Uuid::new_v4()),
                    None,
                    || request,
                )?)
                .await
            }
            Request::Submit {
                request,
                configuration,
            } => {
                validate_identity(&request.session_id)?;
                if request.input.len() > 2 * 1024 * 1024 || !request.workspace.is_absolute() {
                    return Err(LocalError::new(
                        LocalErrorKind::BadRequest,
                        "invalid Turn request",
                    ));
                }
                let mut configuration = configuration;
                // 共享纯 probe 时保留正式配置；等它释放后仅重新准入一次，持续争抢返回 Busy。
                for _ in 0..2 {
                    let owner =
                        self.claim(request.session_id.clone(), Some(request.clone()), || {
                            Request::Submit {
                                request: request.clone(),
                                configuration: configuration.take(),
                            }
                        })?;
                    let response = self.wait_reply(owner).await?;
                    if !matches!(
                        response,
                        Response::Acceptance(Acceptance::NeedsConfiguration)
                    ) || configuration.is_none()
                    {
                        return Ok(response);
                    }
                    #[cfg(test)]
                    cancellation_tests::pause(&self.gates.retry).await;
                }
                Err(busy())
            }
            request @ (Request::Write { .. } | Request::Rewind { .. }) => {
                let id = match &request {
                    Request::Write { session_id, .. } | Request::Rewind { session_id, .. } => {
                        session_id.clone()
                    }
                    _ => unreachable!(),
                };
                validate_identity(&id)?;
                self.wait_reply(self.claim(id, None, || request)?).await
            }
            Request::Subscribe { .. } => Err(internal()),
        }
    }

    async fn wait_reply(&self, owner: Arc<Owner>) -> Result<Response> {
        let mut reply = owner.reply.subscribe();
        loop {
            let response = reply.borrow_and_update().clone();
            if let Some(response) = response {
                if !matches!(response, Response::Acceptance(ref value) if *value != Acceptance::NeedsConfiguration)
                {
                    let mut done = owner.done.subscribe();
                    while done.borrow_and_update().is_none() {
                        done.changed().await.map_err(|_| internal())?;
                    }
                }
                return Ok(response);
            }
            reply.changed().await.map_err(|_| internal())?;
        }
    }

    async fn subscribe(
        &self,
        stream: &mut UnixStream,
        session_id: String,
        turn_id: TurnId,
        previous: Option<EventCursor>,
    ) -> Result<()> {
        // 慢观察者不能占满所有连接槽，保留普通查询和取消的接入空间。
        let _subscription = self.subscriptions.try_acquire().map_err(|_| busy())?;
        let (owner, mut events, mut done, view, known_completion) = loop {
            let candidate = lock(&self.admission).owners.get(&session_id).cloned();
            // 候选者可能尚在核验身份；先登记通道，快照后才按核心验证结果决定归属。
            let events = candidate.as_ref().map(|owner| owner.events.subscribe());
            let done = candidate.as_ref().map(|owner| owner.done.subscribe());
            let view = self.query(session_id.clone(), Some(turn_id)).await?;
            let state = lock(&self.admission);
            if state.owners.get(&session_id).is_some_and(|current| {
                !candidate
                    .as_ref()
                    .is_some_and(|owner| Arc::ptr_eq(owner, current))
            }) {
                // 查询期间出现新所有者：为它登记通道并重新固定快照，不能复用旧 cut。
                continue;
            }
            let owner = candidate.filter(|owner| {
                lock(&owner.turn)
                    .as_ref()
                    .is_some_and(|turn| turn.id == turn_id)
                    || owner.verified_turn.get() == Some(&turn_id)
            });
            let completion = state
                .completions
                .get(&(session_id.clone(), turn_id))
                .cloned();
            break (owner, events, done, view, completion);
        };
        // 固定引用后读取核心序号；转发队列可能落后，不能以转发进度判断快照覆盖。
        let mut sequence = owner.as_ref().map_or(0, |owner| owner.source_sequence());
        if previous
            .as_ref()
            .is_some_and(|cursor| cursor.epoch != self.epoch || cursor.sequence != sequence)
            || sequence > 0
        {
            ipc::write(
                stream,
                &Response::Observation(Observation::Gap {
                    cursor: self.cursor(sequence),
                }),
            )
            .await?;
        }
        if owner.is_none()
            && (known_completion.is_some()
                || matches!(view.requested_turn, Some(TurnObservation::Committed(_))))
        {
            let result = if matches!(known_completion, Some(Ok(()))) {
                Ok(())
            } else if matches!(view.requested_turn, Some(TurnObservation::Committed(_))) {
                // 可见的固定 pin 不是确认回执：后台按 session+turn 主动核验并补
                // 真实引用屏障，不经执行锁也不等待同 Session 其他 Turn 的生命周期；
                // 崩溃遗留 pending 由后续新提交或会话写操作自动关闭，不经此观察路径。
                let confirmed = self.confirm_committed(session_id.clone(), turn_id).await;
                if confirmed.is_ok() {
                    // 只有核验通过的固定结果才缓存成功；失败不落缓存，下一次
                    // 订阅重新核验，旧失败诊断不能永久挡住修复后的屏障。
                    lock(&self.admission)
                        .completions
                        .insert((session_id.clone(), turn_id), Ok(()));
                }
                confirmed
            } else {
                // 外层条件保证非 Committed 时必有已知完成缓存，且成功缓存已由首分支
                // 接管，所以这里只可能交付 Owner 退场留下的真实失败；不存在“仅可见”
                // 的完成状态，也不合成未知确认。
                known_completion.expect("非 Committed 视图必有已知完成缓存")
            };
            return ipc::write(
                stream,
                &Response::Observation(Observation::Finished {
                    view: Box::new(view),
                    cursor: self.cursor(sequence),
                    error: result.err(),
                }),
            )
            .await;
        }
        // 引用可见不等于发布屏障或核心收尾结束；有所有者时必须等独立 completion。
        ipc::write(
            stream,
            &Response::Observation(Observation::Snapshot {
                view: Box::new(view),
                cursor: self.cursor(sequence),
                owned: owner.is_some(),
            }),
        )
        .await?;
        let Some(owner) = owner else {
            return Ok(());
        };
        let events = events.as_mut().unwrap();
        let done = done.as_mut().unwrap();
        let mut closed = self.closed.subscribe();
        loop {
            let result = done.borrow_and_update().clone();
            if let Some(result) = result {
                let final_sequence = owner.sequence.load(Ordering::Acquire);
                if final_sequence != sequence {
                    ipc::write(
                        stream,
                        &Response::Observation(Observation::Gap {
                            cursor: self.cursor(final_sequence),
                        }),
                    )
                    .await?;
                }
                let view = self.query(session_id, Some(turn_id)).await?;
                return ipc::write(
                    stream,
                    &Response::Observation(Observation::Finished {
                        view: Box::new(view),
                        cursor: self.cursor(final_sequence),
                        error: result.err(),
                    }),
                )
                .await;
            }
            tokio::select! {
                event = events.recv() => match event {
                    Ok(event) if event.sequence > sequence => {
                        if event.sequence != sequence + 1 {
                            let view = self.query(session_id.clone(), Some(turn_id)).await?;
                            sequence = owner.source_sequence();
                            ipc::write(stream, &Response::Observation(Observation::Gap { cursor: self.cursor(sequence) })).await?;
                            ipc::write(stream, &Response::Observation(Observation::Snapshot { view: Box::new(view), cursor: self.cursor(sequence), owned: true })).await?;
                            // 触发缺口的旧事件也可能已在快照中；不在重载后重新拼接它。
                        } else {
                            sequence = event.sequence;
                            ipc::write(stream, &Response::Observation(Observation::Event { cursor: self.cursor(sequence), event: event.event })).await?;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => {
                        *events = events.resubscribe();
                        let view = self.query(session_id.clone(), Some(turn_id)).await?;
                        sequence = owner.source_sequence();
                        ipc::write(stream, &Response::Observation(Observation::Gap { cursor: self.cursor(sequence) })).await?;
                        ipc::write(stream, &Response::Observation(Observation::Snapshot { view: Box::new(view), cursor: self.cursor(sequence), owned: true })).await?;
                    }
                    _ => {},
                },
                _ = done.changed() => {},
                _ = closed.changed() => {},
            }
        }
    }
}

fn validate_identity(id: &str) -> Result<()> {
    if id.len() != 32
        || !id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(LocalError::new(
            LocalErrorKind::BadRequest,
            "canonical SessionId required",
        ));
    }
    Ok(())
}

async fn connection(backend: Arc<Backend>, mut stream: UnixStream) -> Result<()> {
    ipc::check_peer(&stream)?;
    let request = tokio::time::timeout(Duration::from_secs(10), ipc::read::<Request>(&mut stream))
        .await
        .map_err(|_| LocalError::new(LocalErrorKind::Transport, "request frame timed out"))??;
    if let Request::Subscribe {
        session_id,
        turn_id,
        cursor,
    } = request
    {
        if let Err(error) = backend
            .subscribe(&mut stream, session_id, turn_id, cursor)
            .await
        {
            ipc::write(&mut stream, &Response::Error(error)).await?;
        }
        return Ok(());
    }
    let response = backend
        .dispatch(request)
        .await
        .unwrap_or_else(Response::Error);
    ipc::write(&mut stream, &response).await
}

/// 前台运行后台进程主体。调用者不得为正常 stop abort 此 future；它持有终身实例锁。
/// 不读 dotenv、不预热 Component、不自动恢复；正常 stop 关闭准入并等待全部接管任务。
pub async fn serve(layout: crate::StateLayout) -> Result<()> {
    let binding_layout = layout.clone();
    let (instance, listener) = blocking(move || ipc::Instance::bind(&binding_layout)).await?;
    let listener = UnixListener::from_std(listener)?;
    let backend = Backend::new(layout);
    let mut connections = JoinSet::new();
    let mut failure = None;
    loop {
        let status = backend.status();
        if status.stopping && status.owners == 0 {
            break;
        }
        tokio::select! {
            accepted = listener.accept() => match accepted {
                Ok((stream, _)) if connections.len() < 64 => { connections.spawn(connection(backend.clone(), stream)); }
                Ok(_) => {},
                Err(error) => { failure = Some(error.into()); lock(&backend.admission).stopping = true; break; }
            },
            _ = connections.join_next(), if !connections.is_empty() => {},
            _ = tokio::time::sleep(Duration::from_millis(25)) => {},
        }
    }
    while backend.status().owners != 0 {
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    backend.closed.send_replace(true);
    drop(listener);
    while connections.join_next().await.is_some() {}
    blocking(move || instance.cleanup()).await?;
    failure.map_or(Ok(()), Err)
}

#[cfg(test)]
#[path = "server_cancel_tests.rs"]
mod cancellation_tests;
#[cfg(test)]
pub(super) use cancellation_tests::Provider;

#[cfg(test)]
mod tests {
    use super::*;

    async fn subscription_handoff(start_before_snapshot: bool, claim_before_snapshot: bool) {
        use std::{future::Future, task::Poll};
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let directory = tempfile::tempdir().unwrap();
        let backend = Backend::new(crate::StateLayout::in_state_root(directory.path()));
        let root = crate::LocalWorkspaceRoot::from_path(directory.path()).unwrap();
        let Response::Session(session) = backend
            .dispatch(Request::Create {
                request: crate::CreateSessionRequest {
                    label: None,
                    active_agent: "local-test".into(),
                    project_branch: "main".into(),
                    project_store_key: root.store_key_hex(),
                },
            })
            .await
            .unwrap()
        else {
            panic!("session")
        };
        let request = TurnRequest {
            session_id: session.session_id,
            turn_id: super::super::new_turn_id(),
            workspace: root.canonical_path().into(),
            input: "handoff".into(),
        };
        let owner = Arc::new(Owner::new(Some(request.clone())));
        if claim_before_snapshot {
            lock(&backend.admission)
                .owners
                .insert(request.session_id.clone(), owner.clone());
        }
        let provider = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let configuration = super::super::tests::frozen(
            directory.path(),
            &format!("http://{}/v1", provider.local_addr().unwrap()),
        );
        let mut submit =
            Box::pin(backend.submit(request.clone(), Some(Box::new(configuration)), &owner));
        let (writer, mut reader) = UnixStream::pair().unwrap();
        let (entered, entry) = oneshot::channel();
        let (release, resume) = oneshot::channel();
        let mut observing = None;
        if !start_before_snapshot {
            *lock(&backend.snapshot_gate) = Some((entered, resume));
            observing = Some(tokio::spawn(connection(backend.clone(), writer)));
            ipc::write(
                &mut reader,
                &Request::Subscribe {
                    session_id: request.session_id.clone(),
                    turn_id: request.turn_id,
                    cursor: None,
                },
            )
            .await
            .unwrap();
            tokio::time::timeout(Duration::from_secs(5), entry)
                .await
                .unwrap()
                .unwrap();
        } else {
            drop(entered);
            drop(resume);
            drop(writer);
        }
        if !claim_before_snapshot {
            lock(&backend.admission)
                .owners
                .insert(request.session_id.clone(), owner.clone());
        }
        // 保留真实 submit future，但在核心接管后暂停 acceptance continuation 的轮询。
        tokio::time::timeout(
            Duration::from_secs(5),
            futures::future::poll_fn(|cx| {
                assert!(submit.as_mut().poll(cx).is_pending());
                if lock(&owner.core).is_some() {
                    Poll::Ready(())
                } else {
                    Poll::Pending
                }
            }),
        )
        .await
        .unwrap();
        let (mut response, _) = tokio::time::timeout(Duration::from_secs(10), provider.accept())
            .await
            .expect("core must reach the provider before observing the acceptance handoff")
            .unwrap();
        // provider 连接证明核心已通过真实准入并发布 TurnStarted，仍不恢复后台 continuation。
        if start_before_snapshot {
            let (writer, client) = UnixStream::pair().unwrap();
            reader = client;
            observing = Some(tokio::spawn(connection(backend.clone(), writer)));
            ipc::write(
                &mut reader,
                &Request::Subscribe {
                    session_id: request.session_id.clone(),
                    turn_id: request.turn_id,
                    cursor: None,
                },
            )
            .await
            .unwrap();
        } else {
            release.send(()).unwrap();
        }
        let snapshot = loop {
            match observe(&mut reader).await {
                Observation::Gap { .. } => {}
                observation => break observation,
            }
        };
        assert!(
            matches!(&snapshot, Observation::Snapshot { owned: true, view, .. }
                if matches!(&view.requested_turn, Some(TurnObservation::Pending(turn)) if turn.turn_id == request.turn_id)),
            "verified Turn must remain owned across acceptance handoff: {snapshot:?}"
        );
        // 排空请求后才交付响应，避免未读请求的 TCP reset 冒充执行失败。
        let mut headers = Vec::new();
        while !headers.ends_with(b"\r\n\r\n") {
            headers.push(response.read_u8().await.unwrap());
            assert!(headers.len() <= 65_536);
        }
        let headers = String::from_utf8(headers).unwrap();
        let length: usize = headers
            .lines()
            .find_map(|line| {
                line.to_ascii_lowercase()
                    .strip_prefix("content-length:")
                    .map(|n| n.trim().parse().unwrap())
            })
            .unwrap();
        assert!(length < 1_048_576);
        response.read_exact(&mut vec![0; length]).await.unwrap();
        let body = "data: {\"choices\":[{\"delta\":{\"content\":\"handoff completed\"},\"finish_reason\":null}]}\n\ndata: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n";
        response.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
        tokio::time::timeout(Duration::from_secs(5), submit)
            .await
            .unwrap()
            .unwrap();
        let (completion, _) = lock(&owner.core).take().unwrap();
        let completed = completion.await.unwrap().unwrap();
        assert!(
            matches!(completed, crate::TurnCompletion::Committed(ref turn) if matches!(turn.outcome, crate::TurnOutcome::Completed { .. }))
        );
        owner.done.send_replace(Some(Ok(())));
        loop {
            if let Observation::Finished { view, error, .. } = observe(&mut reader).await {
                assert!(error.is_none(), "{error:?}");
                assert!(
                    matches!(view.requested_turn, Some(TurnObservation::Committed(turn)) if matches!(turn.outcome, crate::TurnOutcome::Completed { .. }))
                );
                break;
            }
        }
        observing.unwrap().await.unwrap().unwrap();
        lock(&backend.admission).owners.clear();
    }

    #[tokio::test]
    async fn subscription_handoff_after_core_acceptance() {
        subscription_handoff(true, true).await;
    }

    #[tokio::test]
    async fn subscription_handoff_verifies_owner_after_snapshot_capture() {
        subscription_handoff(false, true).await;
    }

    #[tokio::test]
    async fn subscription_handoff_claim_during_snapshot_capture() {
        subscription_handoff(false, false).await;
    }

    #[tokio::test]
    async fn invalid_frozen_tool_tls_roots_reject_before_acceptance() {
        let directory = tempfile::tempdir().unwrap();
        let backend = Backend::new(crate::StateLayout::in_state_root(directory.path()));
        let root = crate::LocalWorkspaceRoot::from_path(directory.path()).unwrap();
        let Response::Session(session) = backend
            .dispatch(Request::Create {
                request: crate::CreateSessionRequest {
                    label: None,
                    active_agent: "local-test".into(),
                    project_branch: "main".into(),
                    project_store_key: root.store_key_hex(),
                },
            })
            .await
            .unwrap()
        else {
            panic!("session")
        };
        let configuration = super::super::tests::frozen(directory.path(), "http://127.0.0.1:1/v1");
        // reqwest 的 DER 包装不解析证书；后台须在 TurnStarted 前验证 Tool 使用的根。
        let mut material = serde_json::to_value(configuration).unwrap();
        material["connection"]["roots_der"] = serde_json::json!([[0, 1, 2]]);
        let configuration = serde_json::from_value(material).unwrap();
        let turn_id = super::super::new_turn_id();
        let response = backend
            .dispatch(Request::Submit {
                request: TurnRequest {
                    session_id: session.session_id.clone(),
                    turn_id,
                    workspace: root.canonical_path().into(),
                    input: "invalid TLS roots".into(),
                },
                configuration: Some(Box::new(configuration)),
            })
            .await
            .unwrap();
        backend.dispatch(Request::Stop).await.unwrap();
        assert!(matches!(
            response,
            Response::Error(LocalError {
                kind: LocalErrorKind::BadRequest,
                ..
            })
        ));
        let view = backend
            .query(session.session_id, Some(turn_id))
            .await
            .unwrap();
        assert!(view.requested_turn.is_none());
        assert!(view.active_turn.is_none());
    }

    #[tokio::test]
    async fn set_agent_without_pending_does_not_require_project_stores() {
        let directory = tempfile::tempdir().unwrap();
        let backend = Backend::new(crate::StateLayout::in_state_root(directory.path()));
        let root = crate::LocalWorkspaceRoot::from_path(directory.path()).unwrap();
        let Response::Session(session) = backend
            .dispatch(Request::Create {
                request: crate::CreateSessionRequest {
                    label: None,
                    active_agent: "原代理".into(),
                    project_branch: "main".into(),
                    project_store_key: root.store_key_hex(),
                },
            })
            .await
            .unwrap()
        else {
            panic!("应创建会话");
        };
        let project_stores = backend.layout.project_dir(&root.store_key());
        std::fs::remove_dir_all(&project_stores).unwrap();
        assert!(
            backend
                .query(session.session_id.clone(), None)
                .await
                .unwrap()
                .active_turn
                .is_none()
        );
        let response = backend
            .dispatch(Request::Write {
                session_id: session.session_id.clone(),
                change: SessionWrite::SetAgent {
                    name: "新代理".into(),
                },
            })
            .await
            .unwrap();
        assert!(
            matches!(response, Response::Write(SessionWriteResult::SetAgent(_))),
            "缺少项目库不应阻止无 pending 的 SetAgent"
        );
        assert_eq!(
            backend
                .query(session.session_id, None)
                .await
                .unwrap()
                .session
                .active_agent_name,
            "新代理"
        );
        assert!(!project_stores.exists(), "普通会话写操作不创建项目库");
        backend.dispatch(Request::Stop).await.unwrap();
    }

    pub(super) async fn pending_backend() -> (tempfile::TempDir, Arc<Backend>, TurnRequest) {
        let directory = tempfile::tempdir().unwrap();
        let backend = Backend::new(crate::StateLayout::in_state_root(directory.path()));
        let root = crate::LocalWorkspaceRoot::from_path(directory.path()).unwrap();
        let Response::Session(session) = backend
            .dispatch(Request::Create {
                request: crate::CreateSessionRequest {
                    label: None,
                    active_agent: "test".into(),
                    project_branch: "main".into(),
                    project_store_key: root.store_key_hex(),
                },
            })
            .await
            .unwrap()
        else {
            panic!("session")
        };
        let request = TurnRequest {
            session_id: session.session_id,
            turn_id: super::super::new_turn_id(),
            workspace: root.canonical_path().into(),
            input: "recover me".into(),
        };
        let api = backend.open().await.unwrap();
        let pending = request.clone();
        tokio::task::spawn_blocking(move || {
            let store = api.git_store_for_coordinator().unwrap();
            let (_, guard) = api.lock_execution(&pending.session_id).unwrap();
            store
                .begin_pending_turn(
                    lattice_session::pending::TurnStartedFacts {
                        turn_id: pending.turn_id,
                        session_id: lancet_protocol::SessionId::parse(&pending.session_id).unwrap(),
                        project_store_key: root.store_key(),
                        parent_conversation_tip: None,
                        user_message: lattice_conversation::NativeMessage::new(
                            lattice_conversation::MessageRole::User,
                            None,
                            vec![lattice_conversation::MessagePart::semantic_only(
                                lattice_conversation::MessageSemantic::Text {
                                    text: pending.input,
                                },
                            )],
                        ),
                        config: lattice_session::pending::FrozenTurnConfig {
                            agent_name: "test".into(),
                            model: "test".into(),
                            provider: "test".into(),
                            system_prompt: None,
                            profile_snapshot_json: "{}".into(),
                            policy_snapshot_json: "{}".into(),
                            catalog: lattice_session::pending::ToolCatalogSnapshot::new(
                                "test-build".into(),
                                vec![],
                            )
                            .unwrap(),
                            limits: lattice_session::pending::TurnExecutionLimits {
                                timeout_secs: 300,
                                max_model_steps: lattice_session::pending::MAX_MODEL_STEPS,
                                max_model_attempts: 1,
                            },
                        },
                        accepted_at: "2026-08-24T00:00:00Z".into(),
                    },
                    &guard,
                )
                .unwrap();
        })
        .await
        .unwrap();
        (directory, backend, request)
    }

    #[tokio::test]
    async fn snapshot_cut_excludes_covered_tokens_even_when_forwarding_lags() {
        for gap_reload in [false, true] {
            let (_directory, backend, request) = pending_backend().await;
            let owner = Arc::new(Owner::new(Some(request.clone())));
            *lock(&owner.turn) = Some(OwnedTurn {
                id: request.turn_id,
                completion: None,
            });
            let source = Arc::new(AtomicU64::new(0));
            let (_completion, receiver) = oneshot::channel();
            *lock(&owner.core) = Some((receiver, source.clone()));
            lock(&backend.admission)
                .owners
                .insert(request.session_id.clone(), owner.clone());
            let (entered, entry) = oneshot::channel();
            let (release, resume) = oneshot::channel();
            if !gap_reload {
                *lock(&backend.snapshot_gate) = Some((entered, resume));
            } else {
                // Gap 路径在初始快照之后才武装相同读取屏障。
                *lock(&backend.snapshot_gate) = None;
                drop(entered);
                drop(resume);
            }
            let (mut writer, mut reader) = UnixStream::pair().unwrap();
            let server = backend.clone();
            let session_id = request.session_id.clone();
            let id = request.turn_id;
            let task =
                tokio::spawn(
                    async move { server.subscribe(&mut writer, session_id, id, None).await },
                );
            let release = if gap_reload {
                assert!(matches!(
                    observe(&mut reader).await,
                    Observation::Snapshot { .. }
                ));
                let (entered, entry) = oneshot::channel();
                let (release, resume) = oneshot::channel();
                *lock(&backend.snapshot_gate) = Some((entered, resume));
                source.store(2, Ordering::Release);
                owner
                    .events
                    .send(crate::turn::SequencedTurnEvent {
                        sequence: 2,
                        event: crate::TurnEvent::TokenDelta {
                            content: "covered".into(),
                        },
                    })
                    .unwrap();
                tokio::time::timeout(Duration::from_secs(5), entry)
                    .await
                    .unwrap()
                    .unwrap();
                release
            } else {
                tokio::time::timeout(Duration::from_secs(5), entry)
                    .await
                    .unwrap()
                    .unwrap();
                release
            };
            let api = backend.open().await.unwrap();
            let session_id = request.session_id.clone();
            source.store(2, Ordering::Release);
            tokio::task::spawn_blocking(move || {
                use lancet_protocol::ModelAttemptKey;
                use lattice_session::pending::PendingStage;
                let store = api.git_store_for_coordinator().unwrap();
                let (_, guard) = api.lock_execution(&session_id).unwrap();
                let key = ModelAttemptKey {
                    turn_id: id,
                    model_step_ordinal: 0,
                    attempt_ordinal: 0,
                };
                store
                    .advance_pending_turn(
                        &session_id,
                        PendingStage::ModelAttemptStarted { key },
                        100,
                        &guard,
                    )
                    .unwrap();
                let message = lattice_conversation::NativeMessage::new(
                    lattice_conversation::MessageRole::Assistant,
                    None,
                    vec![lattice_conversation::MessagePart::semantic_only(
                        lattice_conversation::MessageSemantic::Text {
                            text: "covered".into(),
                        },
                    )],
                );
                store
                    .advance_pending_turn(
                        &session_id,
                        PendingStage::MessageComplete {
                            key,
                            message,
                            usage: None,
                        },
                        101,
                        &guard,
                    )
                    .unwrap();
            })
            .await
            .unwrap();
            if !gap_reload {
                // 序号已由核心分配，但后台尚未更新转发进度。
                owner
                    .events
                    .send(crate::turn::SequencedTurnEvent {
                        sequence: 2,
                        event: crate::TurnEvent::TokenDelta {
                            content: "covered".into(),
                        },
                    })
                    .unwrap();
            }
            release.send(()).unwrap();
            let (view, cursor) = loop {
                match observe(&mut reader).await {
                    Observation::Gap { .. } => {}
                    Observation::Snapshot { view, cursor, .. } => break (view, cursor),
                    other => panic!("unexpected {other:?}"),
                }
            };
            let journal = &view.active_turn.as_ref().unwrap().journal_oid;
            let page = backend
                .open()
                .await
                .unwrap()
                .read_transcript_page(&request.session_id, journal, 0, 65_536)
                .await
                .unwrap();
            assert!(page.jsonl.contains("covered"), "{}", page.jsonl);
            assert_eq!(
                cursor.sequence, 2,
                "snapshot must cut at the source sequence after fixing refs"
            );
            source.store(3, Ordering::Release);
            owner
                .events
                .send(crate::turn::SequencedTurnEvent {
                    sequence: 3,
                    event: crate::TurnEvent::TurnCommitting,
                })
                .unwrap();
            let next = observe(&mut reader).await;
            assert!(
                matches!(
                    next,
                    Observation::Event {
                        event: crate::TurnEvent::TurnCommitting,
                        ..
                    }
                ),
                "{next:?}"
            );
            owner.sequence.store(3, Ordering::Release);
            owner.done.send_replace(Some(Ok(())));
            assert!(matches!(
                observe(&mut reader).await,
                Observation::Finished { .. }
            ));
            task.await.unwrap().unwrap();
            lock(&backend.admission).owners.clear();
        }
    }

    #[tokio::test]
    async fn auto_close_probe_and_write_preserve_old_completion() {
        for write in [false, true] {
            let (_directory, backend, old) = pending_backend().await;
            let mut new = old.clone();
            new.turn_id = super::super::new_turn_id();
            new.input = "新的输入".into();
            let response = backend
                .dispatch(if write {
                    Request::Write {
                        session_id: old.session_id.clone(),
                        change: SessionWrite::SetAgent {
                            name: "新代理".into(),
                        },
                    }
                } else {
                    Request::Submit {
                        request: new.clone(),
                        configuration: None,
                    }
                })
                .await
                .unwrap();
            if write {
                assert!(matches!(
                    response,
                    Response::Write(SessionWriteResult::SetAgent(_))
                ));
            } else {
                assert!(matches!(
                    response,
                    Response::Acceptance(Acceptance::NeedsConfiguration)
                ));
            }
            let view = backend
                .query(old.session_id.clone(), Some(old.turn_id))
                .await
                .unwrap();
            assert!(view.active_turn.is_none());
            assert!(
                matches!(view.requested_turn, Some(TurnObservation::Committed(ref turn))
                if turn.turn_id == old.turn_id && turn.origin == crate::ClosureOrigin::Reconciled)
            );
            if write {
                assert_eq!(view.session.active_agent_name, "新代理");
            }
            assert!(
                backend
                    .query(new.session_id.clone(), Some(new.turn_id))
                    .await
                    .unwrap()
                    .requested_turn
                    .is_none()
            );
            assert!(
                !lock(&backend.admission)
                    .completions
                    .contains_key(&(new.session_id, new.turn_id))
            );
            let (mut writer, mut reader) = UnixStream::pair().unwrap();
            let server = backend.clone();
            let task = tokio::spawn(async move {
                server
                    .subscribe(&mut writer, old.session_id, old.turn_id, None)
                    .await
            });
            let observation = observe(&mut reader).await;
            task.await.unwrap().unwrap();
            assert!(
                matches!(observation, Observation::Finished { error: None, .. }),
                "{observation:?}"
            );
            backend.dispatch(Request::Stop).await.unwrap();
        }
    }

    #[tokio::test]
    async fn recovery_failure_after_disconnect_follows_the_durable_stage() {
        for independent in [false, true] {
            for after_publish in [false, true] {
                let (_directory, backend, old) = pending_backend().await;
                let mut new = old.clone();
                new.turn_id = super::super::new_turn_id();
                new.input = "not accepted".into();
                let operation = if independent {
                    Request::Write {
                        session_id: old.session_id.clone(),
                        change: SessionWrite::SetAgent {
                            name: "不应应用".into(),
                        },
                    }
                } else {
                    Request::Submit {
                        request: new.clone(),
                        configuration: None,
                    }
                };
                let layout = backend.layout.clone();
                let root = crate::LocalWorkspaceRoot::from_path(&old.workspace).unwrap();
                let ledger = tokio::task::spawn_blocking(move || {
                    lancet_audit::invocation_store::InvocationStore::open(
                        &layout,
                        &root.store_key(),
                    )
                    .unwrap()
                })
                .await
                .unwrap();
                let ledger = Arc::new(ledger);
                let before = {
                    let ledger = ledger.clone();
                    tokio::task::spawn_blocking(move || ledger.pinned_view().unwrap().commit_oid)
                        .await
                        .unwrap()
                };
                let (entered, release, fault_result) =
                    crate::turn::finalize_fault::arm(old.turn_id, after_publish);
                let (writer, mut client) = UnixStream::pair().unwrap();
                let server = backend.clone();
                let operation_task = tokio::spawn(connection(server, writer));
                ipc::write(&mut client, &operation).await.unwrap();
                tokio::time::timeout(Duration::from_secs(5), entered)
                    .await
                    .unwrap()
                    .unwrap();
                // 关闭旧任务不接管新执行取消，写操作与无配置探测都保持此边界。
                for turn_id in [old.turn_id, new.turn_id] {
                    assert!(matches!(
                        backend
                            .dispatch(Request::Cancel {
                                session_id: old.session_id.clone(),
                                turn_id,
                            })
                            .await
                            .unwrap(),
                        Response::Cancelled { owned: false }
                    ));
                }
                let owner = lock(&backend.admission)
                    .owners
                    .get(&old.session_id)
                    .unwrap()
                    .clone();
                let api = backend.open().await.unwrap();
                let session_id = old.session_id.clone();
                assert!(
                    tokio::task::spawn_blocking(move || matches!(
                        api.try_lock_execution(&session_id),
                        Err(crate::ApiError::Busy(_))
                    ))
                    .await
                    .unwrap(),
                    "recovery must hold the full Turn lock"
                );
                let view = backend
                    .open()
                    .await
                    .unwrap()
                    .read_view(&old.session_id, Some(old.turn_id))
                    .await
                    .unwrap();
                let pending = view.active_turn.unwrap();
                assert!(pending.snapshot.attempt_usages.is_empty());
                assert!(pending.snapshot.post_user_transcript.is_empty());
                assert!(
                    matches!(pending.snapshot.stage, lattice_session::pending::PendingStage::TurnClosing(ref closing)
                        if closing.candidate.outcome == crate::TurnOutcome::Interrupted { reason: lattice_session::pending::TurnInterruption::ProcessInterruptedBeforeModelAttempt }
                    ),
                    "recovery must not start a provider attempt"
                );
                let (mut observer_writer, mut observer_reader) = UnixStream::pair().unwrap();
                let observer_backend = backend.clone();
                let observer_session = old.session_id.clone();
                let observer_turn = old.turn_id;
                let observer_task = tokio::spawn(async move {
                    observer_backend
                        .subscribe(&mut observer_writer, observer_session, observer_turn, None)
                        .await
                });
                let inflight = observe(&mut observer_reader).await;
                drop(observer_reader);
                // 操作连接先断开，再让真实最终发布失败；错误须由后台保留而非客户端回传。
                drop(client);
                release.send(()).unwrap();
                let mut done = owner.done.subscribe();
                let result = tokio::time::timeout(Duration::from_secs(5), async {
                    loop {
                        if let Some(result) = done.borrow_and_update().clone() {
                            break result;
                        }
                        done.changed().await.unwrap();
                    }
                })
                .await
                .unwrap();
                assert_eq!(
                    fault_result.await.unwrap(),
                    Some(if after_publish {
                        "injected ref-fsync failure"
                    } else {
                        "injected object-fsync failure"
                    })
                );
                let error = result.expect_err("injected recovery failure");
                // IPC 按既有安全边界脱敏底层路径；未发布的 pending 重订阅须保留这份精确公开错误，不换成旧错误或 unknown。
                assert_eq!(
                    error,
                    LocalError::from(crate::ApiError::Internal(String::new()))
                );
                assert!(
                    operation_task.await.unwrap().is_err(),
                    "disconnected operation cannot deliver its receipt"
                );
                assert!(
                    matches!(inflight, Observation::Snapshot { owned: false, ref view, .. }
                    if matches!(&view.requested_turn, Some(TurnObservation::Pending(turn))
                        if turn.turn_id == old.turn_id && turn.journal_oid == pending.oid.to_string())
                        && view.active_turn.as_ref().is_some_and(|turn| turn.turn_id == old.turn_id)),
                    "{inflight:?}"
                );
                observer_task.await.unwrap().unwrap();
                assert_eq!(
                    lock(&backend.admission)
                        .completions
                        .get(&(old.session_id.clone(), old.turn_id)),
                    Some(&Err(error.clone())),
                    "断连的观察者不能抹掉完成记录"
                );
                if independent {
                    assert_eq!(
                        backend
                            .query(old.session_id.clone(), None)
                            .await
                            .unwrap()
                            .session
                            .active_agent_name,
                        "test",
                        "关闭失败后 SetAgent 不应生效"
                    );
                }
                assert!(
                    owner
                        .reply
                        .borrow()
                        .as_ref()
                        .is_some_and(|reply| matches!(reply, Response::Error(_))),
                    "B must not receive acceptance"
                );
                for _ in 0..2 {
                    let (mut writer, mut reader) = UnixStream::pair().unwrap();
                    let server = backend.clone();
                    let session_id = old.session_id.clone();
                    let turn_id = old.turn_id;
                    let task = tokio::spawn(async move {
                        server
                            .subscribe(&mut writer, session_id, turn_id, None)
                            .await
                    });
                    let observation = observe(&mut reader).await;
                    task.await.unwrap().unwrap();
                    let Observation::Finished {
                        view,
                        error: observed_error,
                        ..
                    } = observation
                    else {
                        panic!("recovery observation missing: {observation:?}");
                    };
                    // 已发布但屏障失败时固定 pin 已经关闭：重订阅先核验固定结果并
                    // 重做真实引用屏障，注入故障是一次性的，因此这里交付确认；尚未
                    // 发布的 pending 仍保留精确诊断。
                    let expected_error = if after_publish {
                        None
                    } else {
                        Some(error.clone())
                    };
                    assert_eq!(
                        observed_error, expected_error,
                        "independent={independent}, after_publish={after_publish}"
                    );
                    match view.requested_turn.unwrap() {
                        TurnObservation::Committed(turn) => {
                            assert!(after_publish);
                            assert_eq!(turn.turn_id, old.turn_id);
                            assert!(view.active_turn.is_none());
                            assert!(matches!(
                                turn.outcome,
                                crate::TurnOutcome::Interrupted { .. }
                            ));
                        }
                        TurnObservation::Pending(turn) => {
                            assert!(!after_publish);
                            assert_eq!(turn.turn_id, old.turn_id);
                            assert_eq!(view.active_turn.unwrap().turn_id, old.turn_id);
                        }
                    }
                }
                assert!(
                    backend
                        .query(new.session_id.clone(), Some(new.turn_id))
                        .await
                        .unwrap()
                        .requested_turn
                        .is_none()
                );
                assert!(
                    !lock(&backend.admission)
                        .completions
                        .contains_key(&(new.session_id, new.turn_id))
                );
                let after =
                    tokio::task::spawn_blocking(move || ledger.pinned_view().unwrap().commit_oid)
                        .await
                        .unwrap();
                assert_eq!(
                    after, before,
                    "recovery and read-only subscriptions must not dispatch Tools"
                );
                assert_eq!(backend.status().owners, 0);
                backend.dispatch(Request::Stop).await.unwrap();
            }
        }
    }

    #[tokio::test]
    async fn rejected_identity_preserves_original_completion() {
        let directory = tempfile::tempdir().unwrap();
        let backend = Backend::new(crate::StateLayout::in_state_root(directory.path()));
        let root = crate::LocalWorkspaceRoot::from_path(directory.path()).unwrap();
        let Response::Session(session) = backend
            .dispatch(Request::Create {
                request: crate::CreateSessionRequest {
                    label: None,
                    active_agent: "local-test".into(),
                    project_branch: "main".into(),
                    project_store_key: root.store_key_hex(),
                },
            })
            .await
            .unwrap()
        else {
            panic!("session")
        };
        let request = TurnRequest {
            session_id: session.session_id.clone(),
            turn_id: super::super::new_turn_id(),
            workspace: root.canonical_path().into(),
            input: "original input".into(),
        };
        let response = backend
            .dispatch(Request::Submit {
                request: request.clone(),
                configuration: Some(Box::new(super::super::tests::frozen(
                    directory.path(),
                    "http://127.0.0.1:1/v1",
                ))),
            })
            .await
            .unwrap();
        assert!(matches!(
            response,
            Response::Acceptance(Acceptance::Accepted { .. })
        ));
        tokio::time::timeout(Duration::from_secs(5), async {
            while backend.status().owners != 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let key = (request.session_id.clone(), request.turn_id);
        assert_eq!(
            lock(&backend.admission).completions.get(&key),
            Some(&Ok(()))
        );
        // 旧完成缓存（成功或真实失败）都不是权威，冲突请求的错误也不能写入观察
        // 结果：固定 pin 由后台重新核验，核验通过后交付确认。
        for cached in [
            Ok(()),
            Err(LocalError::new(
                LocalErrorKind::StoreUnavailable,
                "original durability failure",
            )),
        ] {
            lock(&backend.admission)
                .completions
                .insert(key.clone(), cached.clone());
            let mut conflicting = request.clone();
            conflicting.input = "different input".into();
            let (entered, entry) = oneshot::channel();
            let (release, resume) = oneshot::channel();
            *lock(&backend.snapshot_gate) = Some((entered, resume));
            let server = backend.clone();
            let conflict = tokio::spawn(async move {
                server
                    .dispatch(Request::Submit {
                        request: conflicting,
                        configuration: None,
                    })
                    .await
            });
            tokio::time::timeout(Duration::from_secs(5), entry)
                .await
                .unwrap()
                .unwrap();
            let (mut writer, mut reader) = UnixStream::pair().unwrap();
            let server = backend.clone();
            let session_id = request.session_id.clone();
            let turn_id = request.turn_id;
            let observing = tokio::spawn(async move {
                server
                    .subscribe(&mut writer, session_id, turn_id, None)
                    .await
            });
            let during_validation = observe(&mut reader).await;
            release.send(()).unwrap();
            let response = conflict.await.unwrap().unwrap();
            observing.await.unwrap().unwrap();
            assert!(
                matches!(&during_validation, Observation::Finished { view, error: None, .. }
                    if matches!(&view.requested_turn, Some(TurnObservation::Committed(turn)) if turn.turn_id == request.turn_id)),
                "{during_validation:?}"
            );
            assert!(
                matches!(response, Response::Error(error) if error.kind == LocalErrorKind::Conflict)
            );
            let (mut writer, mut reader) = UnixStream::pair().unwrap();
            let server = backend.clone();
            let session_id = request.session_id.clone();
            let turn_id = request.turn_id;
            let task = tokio::spawn(async move {
                server
                    .subscribe(&mut writer, session_id, turn_id, None)
                    .await
            });
            let observation = observe(&mut reader).await;
            task.await.unwrap().unwrap();
            assert!(
                matches!(&observation, Observation::Finished { view, error: None, .. }
                    if matches!(&view.requested_turn, Some(TurnObservation::Committed(turn)) if turn.turn_id == request.turn_id)),
                "{observation:?}"
            );
        }
        backend.dispatch(Request::Stop).await.unwrap();
    }

    #[tokio::test]
    async fn visible_commit_waits_for_the_owner_then_reverifies_and_confirms() {
        let directory = tempfile::tempdir().unwrap();
        let layout = crate::StateLayout::in_state_root(directory.path());
        let backend = Backend::new(layout);
        let root = crate::LocalWorkspaceRoot::from_path(directory.path()).unwrap();
        let Response::Session(session) = backend
            .dispatch(Request::Create {
                request: crate::CreateSessionRequest {
                    label: None,
                    active_agent: "test".into(),
                    project_branch: "main".into(),
                    project_store_key: root.store_key_hex(),
                },
            })
            .await
            .unwrap()
        else {
            panic!("session")
        };
        let path = directory.path().join("agent.toml");
        std::fs::write(
            &path,
            "schema_version=1\n[agent]\nname='test'\nmodel='stub-model'\n[prompt]\nsystem='test'\n",
        )
        .unwrap();
        let id = super::super::new_turn_id();
        let request = TurnRequest {
            session_id: session.session_id.clone(),
            turn_id: id,
            workspace: root.canonical_path().into(),
            input: "test".into(),
        };
        let llm = lattice_agent::testing::FakeStreamingLlmClient::builder()
            .push_events(vec![
                lattice_llm::LlmEvent::Token {
                    content: "stored".into(),
                },
                lattice_llm::LlmEvent::Done {
                    finish_reason: "stop".into(),
                    usage: None,
                },
            ])
            .build();
        let execution = crate::start_turn(
            backend.open().await.unwrap(),
            id,
            root,
            crate::RunTurnRequest {
                input: request.input.clone(),
                session_id: session.session_id.clone(),
                loaded_profile: Some(crate::load_agent_profile_path(&path).unwrap()),
                cancel_flag: None,
                test_llm: Some((Arc::new(llm), lattice_llm::ProviderKind::OpenAiCompatible)),
                test_project_root: Some(directory.path().to_string_lossy().into()),
            },
            None,
        );
        execution.acceptance.await.unwrap().unwrap();
        execution.completion.await.unwrap().unwrap();
        let owner = Arc::new(Owner::new(Some(request.clone())));
        *lock(&owner.turn) = Some(OwnedTurn {
            id,
            completion: None,
        });
        lock(&backend.admission)
            .owners
            .insert(session.session_id.clone(), owner.clone());
        let (mut writer, mut reader) = UnixStream::pair().unwrap();
        let server = backend.clone();
        let task = tokio::spawn(async move {
            server
                .subscribe(&mut writer, session.session_id, id, None)
                .await
        });
        assert!(
            matches!(observe(&mut reader).await, Observation::Snapshot { owned: true, view, .. } if matches!(view.requested_turn, Some(TurnObservation::Committed(_))))
        );
        assert!(!task.is_finished());
        owner.done.send_replace(Some(Err(internal())));
        assert!(matches!(
            observe(&mut reader).await,
            Observation::Finished { error: Some(_), .. }
        ));
        task.await.unwrap().unwrap();
        lock(&backend.admission).owners.clear();
        // 只可见的固定 pin 仍不是回执：owner 的错误回执退场后，新订阅重新核验
        // 固定结果并重做真实引用屏障，核验通过才交付确认；旧错误缓存不永久阻止核验。
        lock(&backend.admission)
            .completions
            .insert((request.session_id.clone(), id), Err(internal()));
        let (mut writer, mut reader) = UnixStream::pair().unwrap();
        let server = backend.clone();
        let id_string = request.session_id.clone();
        let task =
            tokio::spawn(async move { server.subscribe(&mut writer, id_string, id, None).await });
        let failure = observe(&mut reader).await;
        task.await.unwrap().unwrap();
        assert!(
            matches!(failure, Observation::Finished { error: None, .. }),
            "{failure:?}"
        );
        assert_eq!(
            lock(&backend.admission)
                .completions
                .get(&(request.session_id.clone(), id)),
            Some(&Ok(())),
            "重新核验成功只对应已核验的固定结果"
        );
        let confirmed = backend
            .dispatch(Request::Submit {
                request: request.clone(),
                configuration: None,
            })
            .await
            .unwrap();
        assert!(matches!(
            confirmed,
            Response::Acceptance(Acceptance::Existing { .. })
        ));
        backend.dispatch(Request::Stop).await.unwrap();
        let (mut writer, mut reader) = UnixStream::pair().unwrap();
        let server = backend.clone();
        let id_string = request.session_id.clone();
        let task =
            tokio::spawn(async move { server.subscribe(&mut writer, id_string, id, None).await });
        assert!(matches!(
            observe(&mut reader).await,
            Observation::Finished { error: None, .. }
        ));
        task.await.unwrap().unwrap();
        let restarted = Backend::new(backend.layout.clone());
        let (mut writer, mut reader) = UnixStream::pair().unwrap();
        let task = tokio::spawn(async move {
            restarted
                .subscribe(&mut writer, request.session_id, id, None)
                .await
        });
        assert!(matches!(
            observe(&mut reader).await,
            Observation::Finished { error: None, .. }
        ));
        task.await.unwrap().unwrap();
    }

    /// 真实链路建立一个已关闭 Turn（FakeStreamingLlmClient 走完整提交），
    /// 作为重启确认与并发观察场景的公共前置。
    async fn committed_backend() -> (tempfile::TempDir, Arc<Backend>, TurnRequest, String) {
        let directory = tempfile::tempdir().unwrap();
        let layout = crate::StateLayout::in_state_root(directory.path());
        let backend = Backend::new(layout);
        let root = crate::LocalWorkspaceRoot::from_path(directory.path()).unwrap();
        let Response::Session(session) = backend
            .dispatch(Request::Create {
                request: crate::CreateSessionRequest {
                    label: None,
                    active_agent: "test".into(),
                    project_branch: "main".into(),
                    project_store_key: root.store_key_hex(),
                },
            })
            .await
            .unwrap()
        else {
            panic!("session")
        };
        let path = directory.path().join("agent.toml");
        std::fs::write(
            &path,
            "schema_version=1\n[agent]\nname='test'\nmodel='stub-model'\n[prompt]\nsystem='test'\n",
        )
        .unwrap();
        let id = super::super::new_turn_id();
        let request = TurnRequest {
            session_id: session.session_id.clone(),
            turn_id: id,
            workspace: root.canonical_path().into(),
            input: "test".into(),
        };
        let llm = lattice_agent::testing::FakeStreamingLlmClient::builder()
            .push_events(vec![
                lattice_llm::LlmEvent::Token {
                    content: "stored".into(),
                },
                lattice_llm::LlmEvent::Done {
                    finish_reason: "stop".into(),
                    usage: None,
                },
            ])
            .build();
        let execution = crate::start_turn(
            backend.open().await.unwrap(),
            id,
            root,
            crate::RunTurnRequest {
                input: request.input.clone(),
                session_id: session.session_id.clone(),
                loaded_profile: Some(crate::load_agent_profile_path(&path).unwrap()),
                cancel_flag: None,
                test_llm: Some((Arc::new(llm), lattice_llm::ProviderKind::OpenAiCompatible)),
                test_project_root: Some(directory.path().to_string_lossy().into()),
            },
            None,
        );
        execution.acceptance.await.unwrap().unwrap();
        let crate::TurnCompletion::Committed(commit) = execution.completion.await.unwrap().unwrap()
        else {
            panic!("committed turn")
        };
        (directory, backend, request, commit.commit_id)
    }

    #[tokio::test]
    async fn restarted_backend_confirms_the_fixed_committed_result() {
        let (_directory, backend, request, commit_id) = committed_backend().await;
        let session_id = request.session_id.clone();
        let id = request.turn_id;
        // 重启等价：新 Backend 的内存完成表为空，固定结果必须由后台自行核验并补屏障。
        let restarted = Backend::new(backend.layout.clone());
        for _ in 0..2 {
            let (mut writer, mut reader) = UnixStream::pair().unwrap();
            let server = restarted.clone();
            let observing_session = session_id.clone();
            let task = tokio::spawn(async move {
                server
                    .subscribe(&mut writer, observing_session, id, None)
                    .await
            });
            let observation = observe(&mut reader).await;
            task.await.unwrap().unwrap();
            assert!(
                matches!(
                    &observation,
                    Observation::Finished { view, error: None, .. }
                        if matches!(&view.requested_turn, Some(TurnObservation::Committed(turn))
                            if turn.turn_id == id && turn.commit_id == commit_id)
                ),
                "观察已关闭结果必须取得完成确认：{observation:?}"
            );
        }
        assert_eq!(
            lock(&restarted.admission)
                .completions
                .get(&(session_id, id)),
            Some(&Ok(())),
            "成功确认只对应已核验的固定结果"
        );
    }

    #[tokio::test]
    async fn confirming_an_old_result_does_not_wait_on_another_turns_execution() {
        let (_directory, backend, request, _commit_id) = committed_backend().await;
        let session_id = request.session_id.clone();
        let id = request.turn_id;
        // 同 Session 另一个 Turn 正在执行：注册其 Owner 并真实持有整 Turn 执行锁。
        let other_id = super::super::new_turn_id();
        let other = Arc::new(Owner::new(None));
        *lock(&other.turn) = Some(OwnedTurn {
            id: other_id,
            completion: None,
        });
        lock(&backend.admission)
            .owners
            .insert(session_id.clone(), other.clone());
        let api = backend.open().await.unwrap();
        let holder_session = session_id.clone();
        let (locked, acquired): (_, oneshot::Receiver<()>) = oneshot::channel();
        let (release, released): (_, oneshot::Receiver<()>) = oneshot::channel();
        let holder = tokio::task::spawn_blocking(move || {
            let (_resolved, _guard) = api.lock_execution(&holder_session).unwrap();
            let _ = locked.send(());
            let _ = released.blocking_recv();
        });
        acquired.await.unwrap();
        let (mut writer, mut reader) = UnixStream::pair().unwrap();
        let server = backend.clone();
        let observing_session = session_id.clone();
        let task = tokio::spawn(async move {
            server
                .subscribe(&mut writer, observing_session, id, None)
                .await
        });
        // 确认旧固定结果不得等待另一 Turn 的生命周期。
        let observation = tokio::time::timeout(Duration::from_secs(5), observe(&mut reader)).await;
        let observation = observation.expect("confirmation must not wait on the other turn");
        assert!(
            matches!(&observation, Observation::Finished { error: None, .. }),
            "{observation:?}"
        );
        // 另一 Turn 的 Owner 未被关闭、取消或改写。
        assert!(other.done.borrow().is_none());
        assert!(other.reply.borrow().is_none());
        assert!(lock(&backend.admission).owners.contains_key(&session_id));
        drop(release);
        holder.await.unwrap();
        task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn a_stale_failure_completion_does_not_block_reverification() {
        let (_directory, backend, request, _commit_id) = committed_backend().await;
        let session_id = request.session_id.clone();
        let id = request.turn_id;
        // 旧真实失败诊断（如屏障曾失败）留在完成表；固定 pin 仍是权威，
        // 后续观察必须重新核验并补屏障，而不是被旧错误永久挡住。
        lock(&backend.admission).completions.insert(
            (session_id.clone(), id),
            Err(LocalError::new(
                LocalErrorKind::Internal,
                "finalize barrier failed",
            )),
        );
        let (mut writer, mut reader) = UnixStream::pair().unwrap();
        let server = backend.clone();
        let observing_session = session_id.clone();
        let task = tokio::spawn(async move {
            server
                .subscribe(&mut writer, observing_session, id, None)
                .await
        });
        assert!(
            matches!(
                observe(&mut reader).await,
                Observation::Finished { error: None, .. }
            ),
            "已修复的固定结果必须重新核验成功"
        );
        task.await.unwrap().unwrap();
        assert_eq!(
            lock(&backend.admission).completions.get(&(session_id, id)),
            Some(&Ok(())),
            "成功缓存只对应已核验的固定结果"
        );
    }

    async fn observe(socket: &mut UnixStream) -> Observation {
        let Response::Observation(observation) =
            tokio::time::timeout(Duration::from_secs(5), ipc::read(socket))
                .await
                .unwrap()
                .unwrap()
        else {
            panic!("expected observation")
        };
        observation
    }

    #[tokio::test]
    async fn loss_before_forwarding_slow_receiver_and_tail_each_reload_fixed_state() {
        let directory = tempfile::tempdir().unwrap();
        let layout = crate::StateLayout::in_state_root(directory.path());
        let backend = Backend::new(layout);
        let key = crate::LocalWorkspaceRoot::from_path(directory.path())
            .unwrap()
            .store_key_hex();
        let Response::Session(session) = backend
            .dispatch(Request::Create {
                request: crate::CreateSessionRequest {
                    label: None,
                    active_agent: "test".into(),
                    project_branch: "main".into(),
                    project_store_key: key,
                },
            })
            .await
            .unwrap()
        else {
            panic!("expected session")
        };
        let request = TurnRequest {
            session_id: session.session_id.clone(),
            turn_id: TurnId::parse(&uuid::Uuid::new_v4().to_string()).unwrap(),
            workspace: directory.path().into(),
            input: "test".into(),
        };
        let owner = Arc::new(Owner::new(Some(request.clone())));
        *lock(&owner.turn) = Some(OwnedTurn {
            id: request.turn_id,
            completion: None,
        });
        lock(&backend.admission)
            .owners
            .insert(session.session_id.clone(), owner.clone());
        let (mut writer, mut reader) = UnixStream::pair().unwrap();
        let server = backend.clone();
        let task = tokio::spawn(async move {
            server
                .subscribe(&mut writer, request.session_id, request.turn_id, None)
                .await
        });
        assert!(matches!(
            observe(&mut reader).await,
            Observation::Snapshot { .. }
        ));
        owner.sequence.store(2, Ordering::Release);
        owner
            .events
            .send(crate::turn::SequencedTurnEvent {
                sequence: 2,
                event: crate::TurnEvent::TurnCommitting,
            })
            .unwrap();
        assert!(
            matches!(observe(&mut reader).await, Observation::Gap { cursor } if cursor.sequence == 2)
        );
        assert!(matches!(
            observe(&mut reader).await,
            Observation::Snapshot { .. }
        ));
        owner.sequence.store(3, Ordering::Release);
        owner
            .events
            .send(crate::turn::SequencedTurnEvent {
                sequence: 3,
                event: crate::TurnEvent::TurnCommitting,
            })
            .unwrap();
        assert!(
            matches!(observe(&mut reader).await, Observation::Event { cursor, .. } if cursor.sequence == 3)
        );
        // 当前线程不让订阅任务运行，确定性越过队列容量，不依赖机器速度。
        for sequence in 4..=70 {
            owner.sequence.store(sequence, Ordering::Release);
            owner
                .events
                .send(crate::turn::SequencedTurnEvent {
                    sequence,
                    event: crate::TurnEvent::TurnCommitting,
                })
                .unwrap();
        }
        assert!(
            matches!(observe(&mut reader).await, Observation::Gap { cursor } if cursor.sequence == 70)
        );
        assert!(matches!(
            observe(&mut reader).await,
            Observation::Snapshot { .. }
        ));
        owner.sequence.store(71, Ordering::Release);
        owner.done.send_replace(Some(Ok(())));
        assert!(
            matches!(observe(&mut reader).await, Observation::Gap { cursor } if cursor.sequence == 71)
        );
        assert!(matches!(
            observe(&mut reader).await,
            Observation::Finished { .. }
        ));
        task.await.unwrap().unwrap();
        lock(&backend.admission).owners.clear();
    }
}
