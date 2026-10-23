// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use super::{ipc, types::*};
use crate::turn::{
    coordinator::{CoordinatorStoreIo, RandomIdSource, TurnCoordinator},
    observer::ObserverSink,
};
use futures::FutureExt;
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::{
    net::{UnixListener, UnixStream},
    sync::{oneshot, watch},
    task::JoinSet,
};

struct Owner {
    identity: Option<TurnRequest>,
    session: Option<SessionId>,
    reply: watch::Sender<Option<Response>>,
    done: watch::Sender<Option<Result<()>>>,
    observer: ObserverLease,
    execution_cancel: Arc<AtomicBool>,
    started: AtomicBool,
    core: Mutex<Option<oneshot::Receiver<crate::session::Result<crate::TurnCompletion>>>>,
    store: Mutex<Option<crate::SessionApi>>,
}
impl Owner {
    fn reply(&self, response: Response) {
        self.reply.send_replace(Some(response));
    }
}
#[derive(Default)]
struct Admission {
    stopping: bool,
    turns: HashMap<TurnId, Arc<Owner>>,
    sessions: HashMap<SessionId, Arc<Owner>>,
    owners: Vec<Arc<Owner>>,
    diagnostics: HashMap<TurnId, LocalError>,
    unconfirmed_operations: usize,
}
struct Backend {
    layout: crate::StateLayout,
    epoch: String,
    admission: Mutex<Admission>,
    changed: watch::Sender<u64>,
    closed: watch::Sender<bool>,
    stores: Arc<crate::stores::StoreManager>,
    observers: Arc<Mutex<HashMap<SessionId, CachedObserver>>>,
    subscriptions: tokio::sync::Semaphore,
    #[cfg(test)]
    gates: tests::Gates,
}
struct CachedObserver {
    sink: Arc<ObserverSink>,
    leases: usize,
}
struct ObserverLease {
    sink: Arc<ObserverSink>,
    session_id: SessionId,
    cache: Arc<Mutex<HashMap<SessionId, CachedObserver>>>,
}
impl std::ops::Deref for ObserverLease {
    type Target = Arc<ObserverSink>;
    fn deref(&self) -> &Self::Target {
        &self.sink
    }
}
impl Drop for ObserverLease {
    fn drop(&mut self) {
        let mut cache = lock(&self.cache);
        let entry = cache.get_mut(&self.session_id).expect("observer lease");
        entry.leases -= 1;
        if entry.leases == 0 {
            cache.remove(&self.session_id);
        }
    }
}
fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().expect("backend lock poisoned")
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
fn conflict(reason: ConflictReason) -> LocalError {
    LocalError {
        conflict: Some(reason),
        ..LocalError::new(LocalErrorKind::Conflict, "request identity conflicts")
    }
}
fn not_found() -> LocalError {
    LocalError::new(LocalErrorKind::NotFound, "session or Turn not found")
}
fn uninitialized(error: &LocalError) -> bool {
    matches!(
        error.store.as_deref(),
        Some(StoreDiagnostic::NotInitialized { .. })
    )
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
            stores: crate::stores::StoreManager::new(layout.clone()),
            layout,
            epoch: uuid::Uuid::new_v4().to_string(),
            admission: Mutex::default(),
            changed: watch::channel(0).0,
            closed: watch::channel(false).0,
            observers: Arc::default(),
            subscriptions: tokio::sync::Semaphore::new(32),
            #[cfg(test)]
            gates: tests::Gates::default(),
        })
    }
    fn observer(&self, session_id: SessionId) -> ObserverLease {
        let mut cache = lock(&self.observers);
        let entry = cache.entry(session_id).or_insert_with(|| CachedObserver {
            sink: Arc::default(),
            leases: 0,
        });
        entry.leases += 1;
        ObserverLease {
            sink: entry.sink.clone(),
            session_id,
            cache: self.observers.clone(),
        }
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
        self.stores.acquire(false).await.map_err(Into::into)
    }
    async fn optional_store(&self) -> Result<Option<crate::SessionApi>> {
        let mut changed = self.stores.subscribe();
        loop {
            match self.open().await {
                Ok(api) => return Ok(Some(api)),
                Err(error) if uninitialized(&error) => return Ok(None),
                Err(error)
                    if error
                        .recovery
                        .as_ref()
                        .is_some_and(|r| r.cause == RecoveryCause::WriteDisabled) =>
                {
                    changed.changed().await.map_err(|_| internal())?;
                }
                Err(error) => return Err(error),
            }
        }
    }
    fn claim(
        self: &Arc<Self>,
        operation: Request,
        submitter: Option<UnixStream>,
    ) -> Result<Arc<Owner>> {
        let (identity, session) = match &operation {
            Request::Submit { request, .. } => (Some(request.clone()), Some(request.session.id())),
            Request::Rewind { session_id, .. } => (None, Some(*session_id)),
            _ => return Err(internal()),
        };
        let owner = {
            let mut state = lock(&self.admission);
            if identity
                .as_ref()
                .is_some_and(|r| state.turns.contains_key(&r.turn_id))
            {
                return Err(conflict(ConflictReason::TurnIdUsed));
            }
            if identity.is_none() && session.is_some_and(|sid| state.sessions.contains_key(&sid)) {
                return Err(busy());
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
            let owner = Arc::new(Owner {
                identity,
                session,
                reply: watch::channel(None).0,
                done: watch::channel(None).0,
                observer: self.observer(session.expect("operation session")),
                execution_cancel: Arc::default(),
                started: AtomicBool::new(false),
                core: Mutex::new(None),
                store: Mutex::new(None),
            });
            if let Some(request) = &owner.identity {
                state.turns.insert(request.turn_id, owner.clone());
            } else if let Some(sid) = session {
                state.sessions.insert(sid, owner.clone());
                owner.observer.owner_changed();
            }
            state.owners.push(owner.clone());
            owner
        };
        let backend = self.clone();
        let task_owner = owner.clone();
        tokio::spawn(async move {
            let mut result =
                std::panic::AssertUnwindSafe(backend.perform(operation, &task_owner, submitter))
                    .catch_unwind()
                    .await
                    .unwrap_or_else(|_| Err(internal()));
            let completion = lock(&task_owner.core).take();
            // 回执转交失败不能等待模型或工具排空，也不能否认可能已提交的接受事务。
            if task_owner.reply.borrow().is_none() {
                let response = if let Some(request) = &task_owner.identity
                    && completion.is_some()
                {
                    Response::Acceptance(Acceptance::ConfirmationUnknown {
                        turn_id: request.turn_id,
                        session_id: request.session.id(),
                    })
                } else {
                    Response::Error(result.clone().err().unwrap_or_else(internal))
                };
                task_owner.reply(response);
            }
            if let Some(completion) = completion {
                match completion
                    .await
                    .map_err(|_| LocalError::recovery(RecoveryCause::WorkerFailed))
                    .and_then(|r| r.map_err(Into::into))
                {
                    Ok(crate::TurnCompletion::Closed(_)) => {}
                    Err(error) => result = Err(error),
                }
            }
            #[cfg(test)]
            tests::pause(&backend.gates.retire).await;
            {
                let mut state = lock(&backend.admission);
                if let Err(error) = &result
                    && matches!(
                        error.kind,
                        LocalErrorKind::StoreUnavailable | LocalErrorKind::Internal
                    )
                    // 提交的只读准入检查失败时尚无执行收尾；建库提交不确定仍计入。
                    && (task_owner.identity.is_none()
                        || task_owner.started.load(Ordering::Acquire)
                        || error.recovery.is_some())
                {
                    state.unconfirmed_operations += 1;
                    if let Some(request) = &task_owner.identity {
                        state.diagnostics.insert(request.turn_id, error.clone());
                    }
                }
                if let Some(request) = &task_owner.identity {
                    state.turns.remove(&request.turn_id);
                }
                if let Some(sid) = task_owner.session
                    && state
                        .sessions
                        .get(&sid)
                        .is_some_and(|o| Arc::ptr_eq(o, &task_owner))
                {
                    state.sessions.remove(&sid);
                    task_owner.observer.owner_changed();
                    task_owner.observer.release();
                }
                state.owners.retain(|o| !Arc::ptr_eq(o, &task_owner));
                task_owner.done.send_replace(Some(result));
                backend.changed.send_modify(|v| *v += 1);
            }
            // 监督退休前始终持有库代租约，重开不得越过独立收尾。
            drop(lock(&task_owner.store).take());
        });
        Ok(owner)
    }
    async fn perform(
        self: &Arc<Self>,
        operation: Request,
        owner: &Arc<Owner>,
        submitter: Option<UnixStream>,
    ) -> Result<()> {
        match operation {
            Request::Submit {
                request,
                configuration,
            } => self.submit(request, configuration, owner, submitter).await,
            Request::Rewind {
                session_id,
                target,
                workspace,
            } => {
                let root = canonical_workspace(workspace).await?;
                let api = self.open().await?;
                *lock(&owner.store) = Some(api.clone());
                let binding = api.read(move |r| r.session(session_id)).await?;
                if binding.project_store_key != root.store_key() {
                    return Err(conflict(ConflictReason::BindingMismatch));
                }
                let io = CoordinatorStoreIo::acquire_api(api, session_id)
                    .await
                    .map_err(crate::ApiError::from)?;
                let record = TurnCoordinator::new(io, Arc::new(RandomIdSource))
                    .rewind(target)
                    .await
                    .map_err(crate::ApiError::from)?;
                #[cfg(test)]
                tests::pause(&self.gates.rewind_committed).await;
                owner.observer.replace_path(target);
                owner.reply(Response::Rewind(record));
                Ok(())
            }
            _ => Err(internal()),
        }
    }
    async fn submit(
        self: &Arc<Self>,
        request: TurnRequest,
        configuration: Box<super::FrozenConfiguration>,
        owner: &Arc<Owner>,
        submitter: Option<UnixStream>,
    ) -> Result<()> {
        let api = self.optional_store().await?;
        *lock(&owner.store) = api.clone();
        // 身份优先于工作区、配置和同会话 Busy；重发不承担找回职责。
        if let Some(api) = &api {
            let turn_id = request.turn_id;
            if api.read(move |r| r.turn_node(turn_id)).await?.is_some() {
                return Err(conflict(ConflictReason::TurnIdUsed));
            }
        }
        let workspace = canonical_workspace(request.workspace.clone()).await?;
        let sid = request.session.id();
        let record = if let Some(api) = &api {
            match api.read(move |r| r.session(sid)).await {
                Ok(record) => Some(record),
                Err(crate::ApiError::Store(error))
                    if matches!(
                        error.as_ref(),
                        lattice_session::store::StoreError::NotFound { .. }
                    ) =>
                {
                    None
                }
                Err(error) => return Err(error.into()),
            }
        } else {
            None
        };
        match request.session {
            SubmitSession::New(_) if record.is_some() => {
                return Err(conflict(ConflictReason::SessionExists));
            }
            SubmitSession::Existing(_) => {
                let record = record.as_ref().ok_or_else(not_found)?;
                if record.project_store_key != workspace.store_key() {
                    return Err(conflict(ConflictReason::BindingMismatch));
                }
            }
            _ => {}
        }
        {
            let mut state = lock(&self.admission);
            if state.sessions.contains_key(&sid) {
                return Err(busy());
            }
            // 身份、绑定与 Busy 的优先级不变；廉价拒绝不能占住新会话身份。
            if request.input.len() > lattice_session::store::MAX_USER_INPUT_BYTES
                || !request.workspace.is_absolute()
            {
                return Err(LocalError::new(
                    LocalErrorKind::BadRequest,
                    "invalid Turn request",
                ));
            }
            if request
                .agent
                .as_ref()
                .is_some_and(|name| name.trim().is_empty() || name.len() > 128)
                || (request.agent.is_none()
                    && (request.session.is_new()
                        || record
                            .as_ref()
                            .is_some_and(|record| record.current == NodeId(0))))
            {
                return Err(LocalError::new(
                    LocalErrorKind::BadRequest,
                    "new Session or root requires explicit agent",
                ));
            }
            state.sessions.insert(sid, owner.clone());
            owner.observer.owner_changed();
        }
        #[cfg(feature = "test-crash-barriers")]
        crate::turn::crash_barrier::schedule(
            "admission_registered",
            serde_json::json!({"turn_id":request.turn_id,"session_id":sid}),
        )
        .await;
        #[cfg(test)]
        tests::pause(&self.gates.registered).await;
        check_submitter(submitter.as_ref())?;
        let inherited = if !request.session.is_new() {
            api.as_ref()
                .expect("existing Session store")
                .read(move |r| r.current_agent(sid))
                .await?
        } else {
            None
        };
        let selected = request.agent.clone().or(inherited).ok_or_else(|| {
            LocalError::new(
                LocalErrorKind::BadRequest,
                "current agent could not be inherited",
            )
        })?;
        #[cfg(test)]
        tests::pause(&self.gates.prepare).await;
        let (loaded_profile, connection) = blocking(move || configuration.into_execution()).await?;
        if loaded_profile.profile.agent.name != selected {
            return Err(LocalError::new(
                LocalErrorKind::BadRequest,
                "loaded agent differs from selection",
            ));
        }
        let api = match api {
            Some(api) => api,
            // 建库在接受事务之前；任何失败都说明这一轮确定未被接受，按拒绝返回。
            None => self.stores.acquire(true).await?,
        };
        *lock(&owner.store) = Some(api.clone());
        let shutdown = api.shutdown();
        check_submitter(submitter.as_ref())?;
        {
            let _state = lock(&self.admission);
            owner.started.store(true, Ordering::Release);
            owner.observer.owner_changed();
        }
        let execution = crate::turn::start_execution(crate::turn::execute::ExecutionInput {
            session: api,
            layout: self.layout.clone(),
            turn_id: request.turn_id,
            workspace,
            shutdown,
            observer: owner.observer.clone(),
            new_session: request.session.is_new().then_some(sid),
            request: crate::RunTurnRequest {
                input: request.input,
                session_id: (!request.session.is_new()).then_some(sid),
                agent: request.agent,
                loaded_profile: Some(loaded_profile),
                cancel_flag: Some(owner.execution_cancel.clone()),
                #[cfg(any(test, feature = "test-utils"))]
                test_llm: None,
                #[cfg(any(test, feature = "test-utils"))]
                test_project_root: None,
            },
            connection: Some(connection),
        });
        *lock(&owner.core) = Some(execution.completion);
        let accepted = match execution.acceptance.await {
            Ok(Ok(accepted)) => accepted,
            Ok(Err(error)) => {
                let error = LocalError::from(error);
                owner.reply(Response::Error(error.clone()));
                return Err(error);
            }
            Err(_) => {
                owner.reply(Response::Acceptance(Acceptance::ConfirmationUnknown {
                    turn_id: request.turn_id,
                    session_id: sid,
                }));
                return Err(internal());
            }
        };
        #[cfg(test)]
        tests::fail_forwarding(&self.gates.forward_panic).await;
        owner.reply(Response::Acceptance(match accepted {
            crate::TurnAcceptance::Accepted { turn } => Acceptance::Accepted {
                turn: TurnView::from(&turn),
            },
            crate::TurnAcceptance::ConfirmationUnknown { turn_id, .. } => {
                Acceptance::ConfirmationUnknown {
                    turn_id,
                    session_id: sid,
                }
            }
        }));
        Ok(())
    }
    async fn lookup(&self, turn_id: TurnId) -> Result<TurnLookup> {
        loop {
            let candidate = lock(&self.admission).turns.get(&turn_id).cloned();
            if let Some(api) = self.optional_store().await?
                && let Some(node) = api.read(move |r| r.turn_node(turn_id)).await?
            {
                return Ok(TurnLookup::Accepted {
                    turn: api
                        .read(move |r| r.turn_detail(node.session_id, turn_id))
                        .await?,
                });
            }
            let state = lock(&self.admission);
            if state.turns.contains_key(&turn_id) {
                return Ok(TurnLookup::InFlight);
            }
            // 准入可能在读事务之后完成；撤销登记时必须重读，NotFound 才能是终局。
            if candidate.is_none() {
                return Err(not_found());
            }
        }
    }
    async fn session_head(&self, session_id: SessionId) -> Result<SessionHead> {
        let observer = self.observer(session_id);
        loop {
            let generation = {
                let _state = lock(&self.admission);
                observer.owner_generation()
            };
            let api = self.optional_store().await?.ok_or_else(not_found)?;
            let mut head = api
                .read(move |read| crate::turn::observer::session_head(read, session_id))
                .await?;
            let (same_generation, rewinding) = {
                let state = lock(&self.admission);
                (head.owner, head.diagnostic) = head_status(&state, head.turn_id);
                (
                    generation == observer.owner_generation(),
                    state
                        .sessions
                        .get(&session_id)
                        .filter(|owner| owner.identity.is_none())
                        .cloned(),
                )
            };
            if let Some(owner) = rewinding {
                let mut done = owner.done.subscribe();
                while done.borrow_and_update().is_none() {
                    done.changed().await.map_err(|_| internal())?;
                }
                continue;
            }
            if same_generation {
                return Ok(head);
            }
        }
    }
    async fn dispatch(self: &Arc<Self>, request: Request) -> Result<Response> {
        match request {
            Request::Status => Ok(Response::Status(self.status())),
            Request::Stop => {
                let mut changed = self.changed.subscribe();
                lock(&self.admission).stopping = true;
                while self.status().owners != 0 {
                    changed.changed().await.map_err(|_| internal())?;
                }
                Ok(Response::Status(self.status()))
            }
            Request::ResolveSession { prefix } => Ok(Response::Session(
                self.open().await?.resolve_session(prefix).await?,
            )),
            Request::List { after, limit } => {
                let page = self
                    .open()
                    .await?
                    .read(move |r| r.list(after, limit as usize))
                    .await?;
                let state = lock(&self.admission);
                Ok(Response::Sessions(Page {
                    next: page.next,
                    items: page
                        .items
                        .into_iter()
                        .map(|session| {
                            let owner = session.unclosed_turn.is_some_and(|turn| {
                                state
                                    .turns
                                    .get(&turn)
                                    .is_some_and(|owner| owner.started.load(Ordering::Acquire))
                            });
                            SessionListRow { session, owner }
                        })
                        .collect(),
                }))
            }
            Request::Log { session_id } => Ok(Response::Log(
                self.open().await?.read(move |r| r.log(session_id)).await?,
            )),
            Request::Turn { turn_id } => Ok(Response::Turn(self.lookup(turn_id).await?)),
            Request::Head { session_id } => {
                Ok(Response::Head(self.session_head(session_id).await?))
            }
            Request::History {
                session_id,
                before,
                turns,
            } => {
                if !(1..=MAX_HISTORY_TURNS).contains(&turns) {
                    return Err(LocalError::new(
                        LocalErrorKind::BadRequest,
                        "history turns must be between 1 and 100",
                    ));
                }
                Ok(Response::History(LocalHistory {
                    entries: self
                        .open()
                        .await?
                        .read(move |r| r.history(session_id, before, turns))
                        .await?,
                }))
            }
            Request::Cancel { turn_id } => {
                let owned = {
                    let state = lock(&self.admission);
                    state.turns.get(&turn_id).is_some_and(|owner| {
                        owner.execution_cancel.store(true, Ordering::Release);
                        true
                    })
                };
                if !owned && let Some(api) = self.optional_store().await? {
                    api.read(move |r| r.turn_node(turn_id)).await?;
                }
                Ok(Response::Cancelled { owned })
            }
            Request::Submit { .. } => Err(internal()),
            request @ Request::Rewind { .. } => self.wait_reply(self.claim(request, None)?).await,
            Request::AuditQuery {
                workspace,
                selection,
                after,
                limit,
            } => {
                let filter = audit_filter(workspace, selection).await?;
                Ok(Response::Audit(
                    self.open()
                        .await?
                        .read(move |r| lancet_api::query_audit(r, &filter, after, limit as usize))
                        .await?,
                ))
            }
            Request::AuditTail {
                workspace,
                selection,
                after,
                limit,
            } => {
                let filter = audit_filter(workspace, selection).await?;
                Ok(Response::AuditTail(
                    self.open()
                        .await?
                        .read(move |r| lancet_api::tail_audit(r, &filter, after, limit as usize))
                        .await?,
                ))
            }
            Request::AuditReplay {
                workspace,
                session_id,
                turn_id,
            } => {
                let project = canonical_workspace(workspace).await?.store_key();
                Ok(Response::AuditReplay(
                    self.open()
                        .await?
                        .read(move |r| lancet_api::replay_audit(r, &project, session_id, turn_id))
                        .await?,
                ))
            }
            Request::OperatorStatus { workspace } => {
                let project = canonical_workspace(workspace).await?.store_key();
                Ok(Response::OperatorStatus(
                    self.open()
                        .await?
                        .read(move |r| lancet_api::collect_status(r, &project))
                        .await?,
                ))
            }
            Request::Watch { .. } => Err(internal()),
        }
    }
    async fn wait_reply(&self, owner: Arc<Owner>) -> Result<Response> {
        let mut reply = owner.reply.subscribe();
        loop {
            let response = reply.borrow_and_update().clone();
            if let Some(response) = response {
                if matches!(response, Response::Error(_) | Response::Rewind(_)) {
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
    async fn watch(
        &self,
        stream: &mut UnixStream,
        writer: &mut ipc::ResponseWriter,
        session_id: SessionId,
    ) -> Result<()> {
        let _permit = self.subscriptions.try_acquire().map_err(|_| busy())?;
        let observer = self.observer(session_id);
        let cursor = observer.register();
        let mut changed = observer.subscribe();
        let mut closed = self.closed.subscribe();
        let mut first = true;
        let mut last_head: Option<SessionHead> = None;
        loop {
            if *closed.borrow() {
                return Ok(());
            }
            changed.borrow_and_update();
            let generation = {
                let _state = lock(&self.admission);
                observer.owner_generation()
            };
            #[cfg(test)]
            tests::pause(&self.gates.capture_before).await;
            let api = self.optional_store().await?.ok_or_else(not_found)?;
            let sink = observer.clone();
            let push = cursor.clone();
            let mut updates = api
                .blocking(move |api| Ok(sink.capture(&api.store, session_id, &push)?))
                .await?;
            drop(api);
            #[cfg(test)]
            tests::pause(&self.gates.capture).await;
            // 代次识别完整的接管/退场；rewind 提交到发布之间也不能交付新位置。
            let has_head = matches!(updates.first(), Some(ViewUpdate::Head(_)));
            let head_turn = if let Some(ViewUpdate::Head(head)) = updates.first() {
                head.turn_id
            } else {
                last_head.as_ref().and_then(|head| head.turn_id)
            };
            let (owned, diagnostic, same_generation, rewinding) = {
                let state = lock(&self.admission);
                let (owned, diagnostic) = head_status(&state, head_turn);
                (
                    owned,
                    diagnostic,
                    generation == observer.owner_generation(),
                    state
                        .sessions
                        .get(&session_id)
                        .filter(|owner| owner.identity.is_none())
                        .cloned(),
                )
            };
            let retry = !same_generation
                || rewinding.is_some()
                || observer.path_replacement_pending(&cursor);
            #[cfg(test)]
            tests::pause(&self.gates.capture_checked).await;
            if retry {
                observer.retry_capture(&cursor, first, &updates);
                if let Some(owner) = rewinding {
                    let mut done = owner.done.subscribe();
                    while done.borrow_and_update().is_none() {
                        done.changed().await.map_err(|_| internal())?;
                    }
                }
                continue;
            }
            // 诊断也可能独立于节点变化发布；比较已交付的头，不读取会话库来探测。
            if !has_head
                && last_head
                    .as_ref()
                    .is_some_and(|head| head.owner != owned || head.diagnostic != diagnostic)
            {
                observer.refresh_head(&cursor);
                continue;
            }
            if let Some(ViewUpdate::Head(head)) = updates.first_mut() {
                head.owner = owned;
                head.diagnostic = diagnostic;
            }
            for update in updates {
                writer
                    .write(stream, &Response::View(update.clone()))
                    .await?;
                match &update {
                    ViewUpdate::Head(head) => last_head = Some(head.clone()),
                    ViewUpdate::Text(text) => observer.sent(&cursor, text),
                    ViewUpdate::Entry { .. } => {}
                }
            }
            first = false;
            #[cfg(test)]
            tests::pause(&self.gates.watch_wait).await;
            tokio::select! {
                _ = changed.changed() => {},
                _ = closed.changed() => return Ok(()),
                result = stream.readable() => {
                    result?;
                    match stream.try_read(&mut [0u8; 1]) {
                        Ok(0) => return Ok(()),
                        Ok(_) => return Err(LocalError::new(LocalErrorKind::Protocol, "unexpected data on watch connection")),
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {},
                        Err(error) => return Err(error.into()),
                    }
                },
            }
        }
    }
}
fn head_status(state: &Admission, turn: Option<TurnId>) -> (bool, Option<LocalError>) {
    (
        turn.is_some_and(|turn| {
            state
                .turns
                .get(&turn)
                .is_some_and(|owner| owner.started.load(Ordering::Acquire))
        }),
        turn.and_then(|turn| state.diagnostics.get(&turn).cloned()),
    )
}
fn check_submitter(submitter: Option<&UnixStream>) -> Result<()> {
    if let Some(submitter) = submitter
        && !connected(submitter)?
    {
        return Err(LocalError::new(
            LocalErrorKind::BadRequest,
            "submitter departed before admission",
        ));
    }
    Ok(())
}
fn connected(stream: &UnixStream) -> Result<bool> {
    use std::os::fd::AsRawFd;
    let mut byte = 0u8;
    // 非阻塞窥读检查连接当前是否已关闭，不消费后续协议数据，也不依赖缓存的 readiness。
    let result = unsafe {
        libc::recv(
            stream.as_raw_fd(),
            (&mut byte as *mut u8).cast(),
            1,
            libc::MSG_PEEK | libc::MSG_DONTWAIT,
        )
    };
    if result >= 0 {
        return Ok(result != 0);
    }
    let error = std::io::Error::last_os_error();
    if error.kind() == std::io::ErrorKind::WouldBlock {
        Ok(true)
    } else {
        Err(error.into())
    }
}
async fn canonical_workspace(path: std::path::PathBuf) -> Result<crate::LocalWorkspaceRoot> {
    blocking(move || {
        let root = crate::LocalWorkspaceRoot::from_path(&path)?;
        if root.canonical_path() != path {
            return Err(LocalError::new(
                LocalErrorKind::BadRequest,
                "workspace must be canonical",
            ));
        }
        Ok(root)
    })
    .await
}
async fn audit_filter(
    workspace: std::path::PathBuf,
    selection: AuditSelection,
) -> Result<lattice_session::store::AuditFilter> {
    Ok(lattice_session::store::AuditFilter {
        project_store_key: canonical_workspace(workspace).await?.store_key(),
        session_id: selection.session_id,
        turn_id: selection.turn_id,
        method: selection.method,
        outcome: selection.outcome,
    })
}
async fn connection(backend: Arc<Backend>, mut stream: UnixStream) -> Result<()> {
    ipc::check_peer(&stream)?;
    let request = tokio::time::timeout(Duration::from_secs(10), ipc::read::<Request>(&mut stream))
        .await
        .map_err(|_| LocalError::new(LocalErrorKind::Transport, "request frame timed out"))??;
    let mut writer = ipc::ResponseWriter::default();
    if let Request::Watch { session_id } = request {
        let result = backend.watch(&mut stream, &mut writer, session_id).await;
        #[cfg(test)]
        tests::pause(&backend.gates.watch_done).await;
        if let Err(error) = result {
            writer.write(&mut stream, &Response::Error(error)).await?;
        }
        return Ok(());
    }
    let response = if matches!(request, Request::Submit { .. }) {
        let socket = stream.into_std()?;
        let submitter = UnixStream::from_std(socket.try_clone()?)?;
        stream = UnixStream::from_std(socket)?;
        match backend.claim(request, Some(submitter)) {
            Ok(owner) => backend.wait_reply(owner).await,
            Err(error) => Err(error),
        }
    } else {
        backend.dispatch(request).await
    };
    writer
        .write(&mut stream, &response.unwrap_or_else(Response::Error))
        .await
}
/// 实例锁唯一持有者；正常停止排空后台执行与读取，不创建领域库。
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
    let mut changed = backend.changed.subscribe();
    while backend.status().owners != 0 {
        changed.changed().await.map_err(|_| internal())?;
    }
    backend.closed.send_replace(true);
    drop(listener);
    while connections.join_next().await.is_some() {}
    drop(backend);
    blocking(move || instance.cleanup()).await?;
    failure.map_or(Ok(()), Err)
}

#[cfg(test)]
#[path = "server_cancel_tests.rs"]
mod tests;
