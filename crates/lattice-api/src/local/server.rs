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
    session: Mutex<Option<SessionId>>,
    reply: watch::Sender<Option<Response>>,
    done: watch::Sender<Option<Result<()>>>,
    observer: Arc<ObserverSink>,
    execution_cancel: Option<Arc<AtomicBool>>,
    core: Mutex<Option<oneshot::Receiver<crate::session::Result<crate::TurnCompletion>>>>,
    // 整个监督责任持有库代；独立 completion 返回不等于 Owner 已退休。
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
    subscriptions: tokio::sync::Semaphore,
    #[cfg(test)]
    gates: cancellation_tests::Gates,
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
async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T> + Send + 'static,
) -> Result<T> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|_| internal())?
}
fn same_request(a: &TurnRequest, b: &TurnRequest, sid: Option<SessionId>) -> bool {
    a.turn_id == b.turn_id
        && a.input == b.input
        && a.workspace == b.workspace
        && b.session_id
            .is_none_or(|id| Some(id) == a.session_id.or(sid))
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
            subscriptions: tokio::sync::Semaphore::new(32),
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
        self.stores.acquire(false).await.map_err(Into::into)
    }
    fn cursor(&self, sequence: u64) -> EventCursor {
        EventCursor {
            epoch: self.epoch.clone(),
            sequence,
        }
    }

    fn claim(
        self: &Arc<Self>,
        identity: Option<TurnRequest>,
        session: Option<SessionId>,
        operation: impl FnOnce() -> Request,
    ) -> Result<Arc<Owner>> {
        let (owner, operation) = {
            let mut state = lock(&self.admission);
            if let Some(request) = &identity
                && let Some(owner) = state.turns.get(&request.turn_id)
            {
                return if same_request(
                    owner.identity.as_ref().expect("submit owner"),
                    request,
                    *lock(&owner.session),
                ) {
                    Ok(owner.clone())
                } else {
                    Err(LocalError::new(
                        LocalErrorKind::Conflict,
                        "TurnId is bound to different input or workspace",
                    ))
                };
            }
            if session.is_some_and(|id| state.sessions.contains_key(&id)) {
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
            let operation = operation();
            let execution_cancel = matches!(
                &operation,
                Request::Submit {
                    configuration: Some(_),
                    ..
                }
            )
            .then(Arc::default);
            let owner = Arc::new(Owner {
                identity,
                session: Mutex::new(session),
                reply: watch::channel(None).0,
                done: watch::channel(None).0,
                observer: Arc::default(),
                execution_cancel,
                core: Mutex::new(None),
                store: Mutex::new(None),
            });
            if let Some(request) = &owner.identity {
                state.turns.insert(request.turn_id, owner.clone());
            }
            if let Some(id) = session {
                state.sessions.insert(id, owner.clone());
            }
            state.owners.push(owner.clone());
            (owner, operation)
        };
        let backend = self.clone();
        let task_owner = owner.clone();
        tokio::spawn(async move {
            let mut result = std::panic::AssertUnwindSafe(backend.perform(operation, &task_owner))
                .catch_unwind()
                .await
                .unwrap_or_else(|_| Err(internal()));
            // completion 在展开边界外保留；观察/准备失败不能遗失已经启动的执行责任。
            #[cfg(test)]
            cancellation_tests::pause(&backend.gates.supervise).await;
            let completion = lock(&task_owner.core).take();
            if let Some(completion) = completion {
                match completion
                    .await
                    .map_err(|_| LocalError::recovery(RecoveryCause::WorkerFailed))
                    .and_then(|r| r.map_err(Into::into))
                {
                    Ok(crate::TurnCompletion::Closed(_))
                    | Ok(crate::TurnCompletion::Running(_)) => {}
                    Err(error) => result = Err(error),
                }
            }
            if task_owner.reply.borrow().is_none() {
                task_owner.reply(Response::Error(
                    result.clone().err().unwrap_or_else(internal),
                ));
            }
            #[cfg(test)]
            cancellation_tests::pause(&backend.gates.retire).await;
            {
                let mut state = lock(&backend.admission);
                if let Err(error) = &result
                    && matches!(
                        error.kind,
                        LocalErrorKind::StoreUnavailable | LocalErrorKind::Internal
                    )
                {
                    state.unconfirmed_operations += 1;
                    if let Some(request) = &task_owner.identity {
                        state.diagnostics.insert(request.turn_id, error.clone());
                    }
                }
                if let Some(request) = &task_owner.identity {
                    state.turns.remove(&request.turn_id);
                }
                if let Some(sid) = *lock(&task_owner.session)
                    && state
                        .sessions
                        .get(&sid)
                        .is_some_and(|o| Arc::ptr_eq(o, &task_owner))
                {
                    state.sessions.remove(&sid);
                }
                state.owners.retain(|o| !Arc::ptr_eq(o, &task_owner));
                task_owner.done.send_replace(Some(result));
                backend.changed.send_modify(|v| *v += 1);
            }
            drop(lock(&task_owner.store).take());
        });
        Ok(owner)
    }

    async fn perform(self: &Arc<Self>, operation: Request, owner: &Arc<Owner>) -> Result<()> {
        match operation {
            Request::Submit {
                request,
                configuration,
            } => self.submit(request, configuration, owner).await,
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
                    return Err(LocalError::new(
                        LocalErrorKind::Conflict,
                        "workspace differs from Session binding",
                    ));
                }
                let io = CoordinatorStoreIo::acquire_api(api, session_id)
                    .await
                    .map_err(crate::ApiError::from)?;
                let coordinator = TurnCoordinator::new(io, Arc::new(RandomIdSource));
                let record = coordinator
                    .rewind(target)
                    .await
                    .map_err(crate::ApiError::from)?;
                owner.reply(Response::Rewind(record));
                Ok(())
            }
            _ => Err(internal()),
        }
    }

    async fn find_existing(&self, request: &TurnRequest) -> Result<Option<TurnView>> {
        let api = match self.open().await {
            Ok(api) => api,
            Err(error)
                if request.session_id.is_none()
                    && matches!(error.store, Some(StoreDiagnostic::NotInitialized { .. })) =>
            {
                return Ok(None);
            }
            Err(error) => return Err(error),
        };
        let turn_id = request.turn_id;
        let Some((sid, original_input, original_project)) = api
            .read(move |r| {
                let Some(node) = r.turn_node(turn_id)? else {
                    return Ok(None);
                };
                Ok(Some((
                    node.session_id,
                    r.turn_input(node.session_id, turn_id)?,
                    r.session(node.session_id)?.project_store_key,
                )))
            })
            .await?
        else {
            return Ok(None);
        };
        let workspace = canonical_workspace(request.workspace.clone()).await?;
        if original_input != request.input
            || original_project != workspace.store_key()
            || request.session_id.is_some_and(|id| id != sid)
        {
            return Err(LocalError::new(
                LocalErrorKind::Conflict,
                "TurnId identity differs",
            ));
        }
        let detail = api.read(move |r| r.turn_detail(sid, turn_id)).await?;
        Ok(Some(TurnView::from(&detail)))
    }

    async fn submit(
        self: &Arc<Self>,
        request: TurnRequest,
        configuration: Option<Box<super::FrozenConfiguration>>,
        owner: &Arc<Owner>,
    ) -> Result<()> {
        #[cfg(test)]
        cancellation_tests::pause(if configuration.is_some() {
            &self.gates.prepare
        } else {
            &self.gates.probe
        })
        .await;
        let workspace = canonical_workspace(request.workspace.clone()).await?;
        let api = match self.open().await {
            Ok(api) => Some(api),
            Err(error)
                if request.session_id.is_none()
                    && matches!(error.store, Some(StoreDiagnostic::NotInitialized { .. })) =>
            {
                None
            }
            Err(error) => return Err(error),
        };
        *lock(&owner.store) = api.clone();
        if let Some(turn) = self.find_existing(&request).await? {
            owner.reply(Response::Acceptance(Acceptance::Existing { turn }));
            return Ok(());
        }
        let inherited = if let Some(session_id) = request.session_id {
            let api = api.as_ref().expect("existing Session requires store");
            let record = api.read(move |r| r.session(session_id)).await?;
            if record.project_store_key != workspace.store_key() {
                return Err(LocalError::new(
                    LocalErrorKind::Conflict,
                    "workspace differs from Session binding",
                ));
            }
            api.read(move |r| r.current_agent(session_id)).await?
        } else {
            None
        };
        let selected = request
            .agent
            .clone()
            .or(inherited)
            .filter(|n| !n.trim().is_empty() && n.len() <= 128)
            .ok_or_else(|| {
                LocalError::new(
                    LocalErrorKind::BadRequest,
                    "new Session or root requires explicit agent",
                )
            })?;
        let Some(configuration) = configuration else {
            owner.reply(Response::Acceptance(Acceptance::NeedsConfiguration {
                agent_name: selected,
            }));
            return Ok(());
        };
        let (loaded_profile, connection) = blocking(move || configuration.into_execution()).await?;
        if loaded_profile.profile.agent.name != selected {
            return Err(LocalError::new(
                LocalErrorKind::Conflict,
                "loaded agent differs from selection",
            ));
        }
        let api = match api {
            Some(api) => api,
            None => match self.stores.acquire(true).await {
                Ok(api) => api,
                Err(error) => {
                    if error.requires_reopen() {
                        owner.reply(Response::Acceptance(Acceptance::ConfirmationUnknown {
                            turn_id: request.turn_id,
                            session_id: request.session_id,
                        }));
                    }
                    return Err(error.into());
                }
            },
        };
        *lock(&owner.store) = Some(api.clone());
        let shutdown = api.shutdown();
        let execution = crate::turn::start_execution(crate::turn::execute::ExecutionInput {
            session: api,
            layout: self.layout.clone(),
            turn_id: request.turn_id,
            workspace,
            shutdown,
            observer: owner.observer.clone(),
            reserve_session: Some(Box::new({
                let backend = self.clone();
                let owner = owner.clone();
                move |sid| {
                    let mut state = lock(&backend.admission);
                    if state.sessions.contains_key(&sid) {
                        return false;
                    }
                    *lock(&owner.session) = Some(sid);
                    state.sessions.insert(sid, owner.clone());
                    true
                }
            })),
            request: crate::RunTurnRequest {
                input: request.input,
                session_id: request.session_id,
                agent: request.agent,
                loaded_profile: Some(loaded_profile),
                cancel_flag: owner.execution_cancel.clone(),
                #[cfg(any(test, feature = "test-utils"))]
                test_llm: None,
                #[cfg(any(test, feature = "test-utils"))]
                test_project_root: None,
            },
            connection: Some(connection),
        });
        *lock(&owner.core) = Some(execution.completion);
        let accepted = execution.acceptance.await.map_err(|_| internal())??;
        #[cfg(test)]
        cancellation_tests::pause(&self.gates.accepted).await;
        #[cfg(test)]
        cancellation_tests::fail_forwarding(&self.gates.forward_panic).await;
        let response = match accepted {
            crate::TurnAcceptance::Accepted { turn } | crate::TurnAcceptance::Existing { turn } => {
                Acceptance::Accepted {
                    turn: TurnView::from(&turn),
                }
            }
            crate::TurnAcceptance::ConfirmationUnknown {
                turn_id,
                session_id,
            } => Acceptance::ConfirmationUnknown {
                turn_id,
                session_id,
            },
        };
        owner.reply(Response::Acceptance(response));
        Ok(())
    }

    async fn target(&self, target: TurnTarget) -> Result<SessionId> {
        let record = self
            .open()
            .await?
            .read(move |r| r.turn_node(target.turn_id))
            .await?
            .ok_or_else(|| LocalError::new(LocalErrorKind::NotFound, "Turn not found"))?;
        if target.session_id.is_some_and(|id| id != record.session_id) {
            return Err(LocalError::new(
                LocalErrorKind::Conflict,
                "Turn belongs to another Session",
            ));
        }
        Ok(record.session_id)
    }

    async fn query(
        &self,
        session_id: Option<SessionId>,
        turn_id: Option<TurnId>,
    ) -> Result<ObservationSnapshot> {
        let session_id = match (session_id, turn_id) {
            (session, Some(turn_id)) => {
                self.target(TurnTarget {
                    session_id: session,
                    turn_id,
                })
                .await?
            }
            (Some(session), None) => session,
            _ => {
                return Err(LocalError::new(
                    LocalErrorKind::BadRequest,
                    "SessionId or TurnId required",
                ));
            }
        };
        loop {
            let candidate = {
                let state = lock(&self.admission);
                match turn_id {
                    Some(turn) => state.turns.get(&turn),
                    None => state.sessions.get(&session_id),
                }
                .cloned()
            };
            let api = self.open().await?;
            let observer = candidate.as_ref().map(|o| o.observer.clone());
            let (snapshot, attempt, seq) = api
                .blocking(move |api| {
                    Ok(match observer {
                        Some(observer) => observer.snapshot(&api.store, session_id, turn_id)?,
                        None => (
                            api.store.begin_read()?.snapshot(session_id, turn_id)?,
                            None,
                            0,
                        ),
                    })
                })
                .await?;
            #[cfg(test)]
            cancellation_tests::pause(&self.gates.snapshot).await;
            let state = lock(&self.admission);
            let current = snapshot
                .selected_turn
                .as_ref()
                .and_then(|t| state.turns.get(&t.turn_id));
            // Owner 退场前可能刚提交最终状态，不能将旧读事务拼成 Running/NoOwner。
            let latest = match turn_id {
                Some(turn) => state.turns.get(&turn),
                None => state.sessions.get(&session_id),
            };
            if candidate
                .as_ref()
                .is_some_and(|before| !latest.is_some_and(|after| Arc::ptr_eq(before, after)))
            {
                continue;
            }
            if current
                .is_some_and(|current| !candidate.as_ref().is_some_and(|o| Arc::ptr_eq(o, current)))
            {
                continue;
            }
            let owned = current.is_some();
            let active = snapshot.active.map(|active| {
                use lattice_session::store::{ActiveStep, SlotState};
                let detail = snapshot
                    .selected_turn
                    .as_ref()
                    .expect("active Turn")
                    .steps
                    .iter()
                    .find(|s| s.key == active.key)
                    .expect("active detail");
                ActiveStepView {
                    key: active.key,
                    state: match active.state {
                        ActiveStep::Requesting { attempt } => {
                            ActiveStateView::Requesting { attempt }
                        }
                        ActiveStep::RetryReady { next_attempt } => {
                            ActiveStateView::RetryReady { next_attempt }
                        }
                        ActiveStep::Executing { reply, slots } => {
                            let lattice_session::store::StepDetailState::Executing { slots: rows } =
                                &detail.state
                            else {
                                unreachable!("same read transaction")
                            };
                            ActiveStateView::Executing {
                                reply,
                                slots: slots
                                    .into_iter()
                                    .zip(rows)
                                    .map(|(slot, row)| SlotView {
                                        invocation: row.clone(),
                                        result: match slot.state {
                                            SlotState::Terminal(t) => Some(t.result().clone()),
                                            _ => None,
                                        },
                                    })
                                    .collect(),
                            }
                        }
                    },
                }
            });
            return Ok(ObservationSnapshot {
                session: snapshot.session,
                current_agent: snapshot.current_agent,
                selected_turn: snapshot.selected_turn,
                active,
                history: snapshot.history,
                attempt: if owned { attempt } else { None },
                cursor: self.cursor(if owned { seq } else { 0 }),
                owned,
            });
        }
    }

    async fn history_page(
        &self,
        anchor: HistoryAnchor,
        cursor: Option<HistoryCursor>,
        max_bytes: u32,
    ) -> Result<HistoryPageView> {
        #[cfg(test)]
        cancellation_tests::pause(&self.gates.history).await;
        #[cfg(feature = "test-crash-barriers")]
        if crate::turn::crash_barrier::history_read_fault(&anchor).await {
            return Err(LocalError::new(
                LocalErrorKind::StoreUnavailable,
                "injected history read failure",
            ));
        }
        let page = self
            .open()
            .await?
            .read(move |read| {
                let page = read.history_page(&anchor, cursor.as_ref(), max_bytes as usize)?;
                let fragments = page
                    .fragments
                    .into_iter()
                    .map(|fragment| {
                        let node = read.node(NodeRef {
                            session_id: anchor.session_id,
                            node: fragment.node,
                        })?;
                        let turn = node
                            .turn_id()
                            .ok_or(lattice_session::store::StoreError::InvalidCursor)?;
                        let record = read
                            .turn_record(turn)?
                            .ok_or(lattice_session::store::StoreError::InvalidCursor)?;
                        Ok(HistoryFragmentView {
                            fragment,
                            summary: record.summary(),
                            agent_name: record.config.agent_name,
                        })
                    })
                    .collect::<lattice_session::store::Result<Vec<_>>>()?;
                Ok(HistoryPageView {
                    anchor,
                    fragments,
                    next: page.next,
                })
            })
            .await
            .map_err(LocalError::from)?;
        #[cfg(feature = "test-crash-barriers")]
        {
            // 已解码成功且读租约已释放，单独控制返回时序，不改页内容。
            let marker = serde_json::json!({"anchor":page.anchor,"fragments":page.fragments.len()});
            crate::turn::crash_barrier::schedule(
                &format!("history_ready_{}", page.anchor.head.0),
                marker.clone(),
            )
            .await;
            crate::turn::crash_barrier::schedule(
                &format!("history_return_{}", page.anchor.head.0),
                marker,
            )
            .await;
        }
        Ok(page)
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
            Request::List { after, limit } => Ok(Response::Sessions(
                self.open()
                    .await?
                    .read(move |r| r.list(after, limit as usize))
                    .await?,
            )),
            Request::Log { session_id } => Ok(Response::Log(
                self.open().await?.read(move |r| r.log(session_id)).await?,
            )),
            Request::Turn { target } => {
                let sid = self.target(target).await?;
                Ok(Response::Turn(
                    self.open()
                        .await?
                        .read(move |r| r.turn_detail(sid, target.turn_id))
                        .await?,
                ))
            }
            Request::History {
                anchor,
                cursor,
                max_bytes,
            } => Ok(Response::History(
                self.history_page(anchor, cursor, max_bytes).await?,
            )),
            Request::Query {
                session_id,
                turn_id,
            } => Ok(Response::View(Box::new(
                self.query(session_id, turn_id).await?,
            ))),
            Request::Cancel { target } => {
                let owned = {
                    let state = lock(&self.admission);
                    if let Some(owner) = state.turns.get(&target.turn_id) {
                        if target
                            .session_id
                            .is_some_and(|id| Some(id) != *lock(&owner.session))
                        {
                            return Err(LocalError::new(
                                LocalErrorKind::Conflict,
                                "Turn belongs to another Session",
                            ));
                        }
                        owner.execution_cancel.as_ref().is_some_and(|cancel| {
                            #[cfg(test)]
                            self.gates.pause_cancel_store();
                            cancel.store(true, Ordering::Release);
                            true
                        })
                    } else {
                        false
                    }
                };
                if !owned {
                    let api = self.open().await?;
                    if let Some(turn) = api.read(move |r| r.turn_node(target.turn_id)).await?
                        && target.session_id.is_some_and(|id| id != turn.session_id)
                    {
                        return Err(LocalError::new(
                            LocalErrorKind::Conflict,
                            "Turn belongs to another Session",
                        ));
                    }
                }
                Ok(Response::Cancelled { owned })
            }
            Request::Submit {
                request,
                configuration,
            } => {
                if request.input.len() > 2 * 1024 * 1024 || !request.workspace.is_absolute() {
                    return Err(LocalError::new(
                        LocalErrorKind::BadRequest,
                        "invalid Turn request",
                    ));
                }
                let unresolved_owner = lock(&self.admission)
                    .turns
                    .get(&request.turn_id)
                    .is_some_and(|owner| {
                        !matches!(
                            &*owner.reply.borrow(),
                            Some(Response::Acceptance(
                                Acceptance::Accepted { .. } | Acceptance::Existing { .. }
                            ))
                        )
                    });
                if !unresolved_owner && let Some(turn) = self.find_existing(&request).await? {
                    return Ok(Response::Acceptance(Acceptance::Existing { turn }));
                }
                let mut configuration = configuration;
                for _ in 0..2 {
                    let owner = self.claim(Some(request.clone()), request.session_id, || {
                        Request::Submit {
                            request: request.clone(),
                            configuration: configuration.take(),
                        }
                    })?;
                    let response = self.wait_reply(owner).await?;
                    if !matches!(
                        response,
                        Response::Acceptance(Acceptance::NeedsConfiguration { .. })
                    ) || configuration.is_none()
                    {
                        return Ok(response);
                    }
                    #[cfg(test)]
                    cancellation_tests::pause(&self.gates.retry).await;
                }
                Err(busy())
            }
            request @ Request::Rewind { session_id, .. } => {
                self.wait_reply(self.claim(None, Some(session_id), || request)?)
                    .await
            }
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
            Request::Subscribe { .. } => Err(internal()),
        }
    }

    async fn wait_reply(&self, owner: Arc<Owner>) -> Result<Response> {
        let mut reply = owner.reply.subscribe();
        loop {
            let response = reply.borrow_and_update().clone();
            if let Some(response) = response {
                if !matches!(
                    response,
                    Response::Acceptance(
                        Acceptance::Accepted { .. }
                            | Acceptance::Existing { .. }
                            | Acceptance::ConfirmationUnknown { .. }
                    )
                ) {
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

    async fn finish_observation(
        &self,
        stream: &mut UnixStream,
        writer: &mut ipc::ResponseWriter,
        view: ObservationSnapshot,
    ) -> Result<()> {
        let turn = view.selected_turn.as_ref().map(|t| crate::TurnRef {
            session_id: t.session_id,
            turn_id: t.turn_id,
        });
        let cursor = view.cursor.clone();
        let anchor = view.history.clone();
        writer
            .write(
                stream,
                &Response::Observation(Observation::Snapshot {
                    view: Box::new(view),
                }),
            )
            .await?;
        let mut assembly = super::client::HistoryAssembly::new(anchor.clone());
        let mut next = None;
        loop {
            let page = match self
                .history_page(anchor.clone(), next.clone(), 65_536)
                .await
            {
                Ok(page) => page,
                Err(error) => {
                    writer
                        .write(
                            stream,
                            &Response::HistoryError(HistoryReadError {
                                context: HistoryReadContext::Page {
                                    anchor,
                                    cursor: next,
                                },
                                error,
                            }),
                        )
                        .await?;
                    return Ok(());
                }
            };
            next = page.next.clone();
            assembly.push(page)?;
            if next.is_none() {
                break;
            }
        }
        let history = assembly.finish()?;
        writer
            .write(
                stream,
                &Response::Observation(Observation::Finished {
                    turn,
                    cursor,
                    history,
                }),
            )
            .await
    }

    async fn subscribe(
        &self,
        stream: &mut UnixStream,
        writer: &mut ipc::ResponseWriter,
        target: TurnTarget,
        previous: Option<EventCursor>,
    ) -> Result<()> {
        let _subscription = self.subscriptions.try_acquire().map_err(|_| busy())?;
        let mut changed = self.changed.subscribe();
        let mut stores = self.stores.subscribe();
        let mut closed = self.closed.subscribe();
        let mut view = self.query(target.session_id, Some(target.turn_id)).await?;
        let mut owner = lock(&self.admission).turns.get(&target.turn_id).cloned();
        let mut sequence = view.cursor.sequence;
        let resume = previous.filter(|c| c.epoch == self.epoch).filter(|c| {
            owner
                .as_ref()
                .is_some_and(|o| o.observer.events_after(c.sequence).is_some())
        });
        if let Some(cursor) = resume {
            sequence = cursor.sequence;
            #[cfg(feature = "test-crash-barriers")]
            crate::turn::crash_barrier::schedule(
                "subscription",
                serde_json::json!({
                    "turn_id":target.turn_id,"sequence":sequence
                }),
            )
            .await;
        } else if owner.is_some()
            || view
                .selected_turn
                .as_ref()
                .is_some_and(|t| t.summary.is_none())
        {
            // 新订阅先得到快照，再从快照的序号起接窗口中的事件。已关闭且没有观察
            // 状态时，finish_observation 立即交付同一份快照，这里不重复。
            writer
                .write(
                    stream,
                    &Response::Observation(Observation::Snapshot {
                        view: Box::new(view.clone()),
                    }),
                )
                .await?;
        }
        // 持有 Owner 观察状态时，关闭快照不代表窗口已交付完：交付终态事件后，或观察到 done
        // 之后窗口再次为空，才结束。没有观察状态的订阅按快照结束。
        let mut terminal = false;
        loop {
            let turn_closed = view
                .selected_turn
                .as_ref()
                .is_none_or(|t| t.summary.is_some());
            let Some(current) = owner.clone() else {
                if turn_closed {
                    return self.finish_observation(stream, writer, view).await;
                }
                if !view.owned {
                    if let Some(error) = lock(&self.admission)
                        .diagnostics
                        .get(&target.turn_id)
                        .cloned()
                    {
                        return Err(error);
                    }
                    return Ok(());
                }
                view = self.query(target.session_id, Some(target.turn_id)).await?;
                owner = lock(&self.admission).turns.get(&target.turn_id).cloned();
                continue;
            };
            // 先建立接收端再检查状态；done 是状态检查，不只是唤醒源。
            let mut events_changed = current.observer.subscribe();
            let mut done = current.done.subscribe();
            let exited = done.borrow_and_update().is_some();
            match current.observer.events_after(sequence) {
                None => {
                    view = self.query(target.session_id, Some(target.turn_id)).await?;
                    let retained = lock(&self.admission)
                        .turns
                        .get(&target.turn_id)
                        .is_some_and(|o| Arc::ptr_eq(o, &current));
                    if retained && view.owned {
                        sequence = view.cursor.sequence;
                        writer
                            .write(
                                stream,
                                &Response::Observation(Observation::Snapshot {
                                    view: Box::new(view.clone()),
                                }),
                            )
                            .await?;
                        continue;
                    }
                    // Owner 已退出：owned=false 快照的序号不属于旧窗口，转入快照结束路径。
                }
                Some(events) if !events.is_empty() => {
                    for event in events {
                        sequence = event.sequence;
                        terminal |= matches!(
                            event.event,
                            crate::TurnEvent::TurnCompleted { .. }
                                | crate::TurnEvent::TurnFailed { .. }
                                | crate::TurnEvent::TurnCancelled { .. }
                        );
                        writer
                            .write(
                                stream,
                                &Response::Observation(Observation::Event {
                                    cursor: self.cursor(sequence),
                                    event: event.event,
                                }),
                            )
                            .await?;
                    }
                    if terminal {
                        view = self.query(target.session_id, Some(target.turn_id)).await?;
                    }
                    continue;
                }
                // 已交付终态：该 observer 不会再追加事件。
                Some(_) if terminal && turn_closed => {
                    return self.finish_observation(stream, writer, view).await;
                }
                // done 在本轮读取窗口之前已观察到，此处的空窗口才证明尾部已排空。
                Some(_) if exited => {
                    view = self.query(target.session_id, Some(target.turn_id)).await?;
                }
                Some(_) => {
                    tokio::select! {
                        _ = events_changed.changed() => {},
                        _ = done.changed() => {},
                        _ = changed.changed() => {},
                        _ = stores.changed() => {},
                        _ = closed.changed() => return Ok(()),
                    }
                    view = self.query(target.session_id, Some(target.turn_id)).await?;
                    continue;
                }
            }
            owner = None;
            // 结束快照只在尾部排空后发出；已关闭的 Turn 由 finish_observation 交付快照。
            if !view.owned
                && view
                    .selected_turn
                    .as_ref()
                    .is_some_and(|t| t.summary.is_none())
            {
                writer
                    .write(
                        stream,
                        &Response::Observation(Observation::Snapshot {
                            view: Box::new(view.clone()),
                        }),
                    )
                    .await?;
            }
        }
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
    if let Request::Subscribe { target, cursor } = request {
        if let Err(error) = backend
            .subscribe(&mut stream, &mut writer, target, cursor)
            .await
        {
            writer.write(&mut stream, &Response::Error(error)).await?;
        }
        return Ok(());
    }
    let response = backend
        .dispatch(request)
        .await
        .unwrap_or_else(Response::Error);
    writer.write(&mut stream, &response).await
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
mod cancellation_tests;
#[cfg(test)]
pub(super) use cancellation_tests::Provider;

#[cfg(test)]
mod tests {
    use super::Result;
    use super::*;

    use lattice_session::store::*;

    pub(super) async fn pending_backend() -> (tempfile::TempDir, Arc<Backend>, TurnRequest) {
        let directory = tempfile::tempdir().unwrap();
        let backend = Backend::new(crate::StateLayout::in_state_root(directory.path()));
        let root = crate::LocalWorkspaceRoot::from_path(directory.path()).unwrap();
        let sid = SessionId::parse(&uuid::Uuid::new_v4().simple().to_string()).unwrap();
        let request = TurnRequest {
            session_id: Some(sid),
            turn_id: super::super::new_turn_id(),
            workspace: root.canonical_path().into(),
            input: "recover me".into(),
            agent: Some("local-test".into()),
        };
        let api = backend.stores.acquire(true).await.unwrap();
        let input = request.clone();
        api.blocking(move |api| {
            let guard = api.store.try_lock_session(sid)?;
            api.store.accept_turn(
                &guard,
                AcceptTurn {
                    session: AcceptSession::New { id: sid },
                    turn_id: input.turn_id,
                    project_store_key: root.store_key(),
                    input: input.input,
                    accepted_at: chrono::Utc::now(),
                    agent_selection: AgentSelection::Explicit,
                    config: FrozenTurnConfig {
                        agent_name: "local-test".into(),
                        model: "test".into(),
                        provider: "openai_compatible".into(),
                        system_prompt: None,
                        catalog: ToolCatalogSnapshot::new("test".into(), vec![]).unwrap(),
                        limits: TurnExecutionLimits {
                            timeout_secs: 300,
                            max_model_steps: 32,
                            max_model_attempts: 2,
                        },
                    },
                },
            )?;
            Ok(())
        })
        .await
        .unwrap();
        (directory, backend, request)
    }

    pub(super) async fn close(backend: &Backend, request: &TurnRequest) {
        let io = CoordinatorStoreIo::acquire_api(
            backend.open().await.unwrap(),
            request.session_id.unwrap(),
        )
        .await
        .unwrap();
        io.close_step(lattice_session::store::CloseStep::StopBetweenRequests {
            turn_id: request.turn_id,
            cause: lattice_session::store::StopCause::Cancelled,
        })
        .await
        .unwrap();
    }
    fn owner(backend: &Backend, request: &TurnRequest) -> Arc<Owner> {
        let owner = Arc::new(Owner {
            identity: Some(request.clone()),
            session: Mutex::new(request.session_id),
            reply: watch::channel(None).0,
            done: watch::channel(None).0,
            observer: Arc::default(),
            execution_cancel: Some(Arc::default()),
            core: Mutex::new(None),
            store: Mutex::new(None),
        });
        let mut state = lock(&backend.admission);
        state.turns.insert(request.turn_id, owner.clone());
        state
            .sessions
            .insert(request.session_id.unwrap(), owner.clone());
        state.owners.push(owner.clone());
        owner
    }
    fn target(request: &TurnRequest) -> TurnTarget {
        TurnTarget {
            session_id: request.session_id,
            turn_id: request.turn_id,
        }
    }
    async fn subscribe(
        backend: &Arc<Backend>,
        request: &TurnRequest,
        cursor: Option<EventCursor>,
    ) -> (UnixStream, tokio::task::JoinHandle<Result<()>>) {
        let (server, mut client) = UnixStream::pair().unwrap();
        let task = tokio::spawn(connection(backend.clone(), server));
        ipc::write(
            &mut client,
            &Request::Subscribe {
                target: target(request),
                cursor,
            },
        )
        .await
        .unwrap();
        (client, task)
    }
    async fn response(reader: &mut ipc::ResponseReader, socket: &mut UnixStream) -> Response {
        tokio::time::timeout(Duration::from_secs(5), reader.read(socket))
            .await
            .unwrap()
            .unwrap()
    }
    async fn finished(
        backend: &Arc<Backend>,
        request: &TurnRequest,
    ) -> (ObservationSnapshot, LocalHistory) {
        let (mut socket, task) = subscribe(backend, request, None).await;
        let mut reader = ipc::ResponseReader::default();
        let Response::Observation(Observation::Snapshot { view }) =
            response(&mut reader, &mut socket).await
        else {
            panic!("final snapshot")
        };
        assert!(view.selected_turn.as_ref().unwrap().summary.is_some());
        let Response::Observation(Observation::Finished { history, turn, .. }) =
            response(&mut reader, &mut socket).await
        else {
            panic!("complete history")
        };
        assert_eq!(turn.unwrap().turn_id, request.turn_id);
        assert_eq!(history.anchor, view.history);
        task.await.unwrap().unwrap();
        (*view, history)
    }
    /// 监督者退休的同一顺序：先移出 admission，再置 done；不发 backend.changed。
    fn retire(backend: &Backend, request: &TurnRequest, owner: &Arc<Owner>, result: Result<()>) {
        let mut state = lock(&backend.admission);
        state.turns.remove(&request.turn_id);
        state.sessions.remove(&request.session_id.unwrap());
        state.owners.retain(|o| !Arc::ptr_eq(o, owner));
        owner.done.send_replace(Some(result));
    }
    /// 取自持久关闭详情的终态事件。
    async fn terminal(backend: &Backend, request: &TurnRequest, failed: bool) -> crate::TurnEvent {
        let (session_id, turn_id) = (request.session_id.unwrap(), request.turn_id);
        let mut detail = backend
            .open()
            .await
            .unwrap()
            .read(move |r| r.turn_detail(session_id, turn_id))
            .await
            .unwrap();
        let turn = crate::TurnRef {
            session_id,
            turn_id,
        };
        let step = detail.steps.pop().unwrap();
        let summary = detail.summary.unwrap();
        if failed {
            crate::TurnEvent::TurnFailed {
                turn,
                step,
                summary,
            }
        } else {
            crate::TurnEvent::TurnCompleted {
                turn,
                step,
                summary,
            }
        }
    }
    fn attempt(request: &TurnRequest) -> lancet_protocol::ModelAttemptKey {
        lancet_protocol::ModelAttemptKey {
            turn_id: request.turn_id,
            model_step_ordinal: 0,
            attempt_ordinal: 0,
        }
    }
    /// 订阅取得 Owner 并交付首个快照后，让它唤醒并停在下一次查询中。
    async fn park_subscriber(
        backend: &Arc<Backend>,
        request: &TurnRequest,
    ) -> (
        UnixStream,
        ipc::ResponseReader,
        tokio::task::JoinHandle<Result<()>>,
        oneshot::Sender<()>,
    ) {
        let (mut socket, task) = subscribe(backend, request, None).await;
        let mut reader = ipc::ResponseReader::default();
        let Response::Observation(Observation::Snapshot { view }) =
            response(&mut reader, &mut socket).await
        else {
            panic!("running snapshot")
        };
        assert!(view.owned && view.selected_turn.unwrap().summary.is_none());
        let (entered, release) = cancellation_tests::arm(&backend.gates.snapshot);
        backend.changed.send_modify(|v| *v += 1);
        tokio::time::timeout(Duration::from_secs(5), entered)
            .await
            .unwrap()
            .unwrap();
        (socket, reader, task, release)
    }
    async fn event(reader: &mut ipc::ResponseReader, socket: &mut UnixStream) -> crate::TurnEvent {
        match response(reader, socket).await {
            Response::Observation(Observation::Event { event, .. }) => event,
            _ => panic!("expected event"),
        }
    }
    /// 关闭快照后紧接 Finished，随后连接结束。
    async fn closing(
        reader: &mut ipc::ResponseReader,
        socket: &mut UnixStream,
    ) -> ObservationSnapshot {
        let Response::Observation(Observation::Snapshot { view }) = response(reader, socket).await
        else {
            panic!("closing snapshot")
        };
        assert!(view.selected_turn.as_ref().unwrap().summary.is_some());
        assert!(matches!(
            response(reader, socket).await,
            Response::Observation(Observation::Finished { .. })
        ));
        assert!(reader.read::<Response>(socket).await.is_err());
        *view
    }

    #[tokio::test]
    async fn retained_owner_delivers_tail_and_terminal_before_finished() {
        for failed in [false, true] {
            let (_directory, backend, request) = pending_backend().await;
            let owner = owner(&backend, &request);
            let (mut socket, mut reader, task, release) = park_subscriber(&backend, &request).await;
            // 订阅者停在唤醒后的查询中：关闭、尾部事件、终态与退休全部完成后再恢复。
            let key = attempt(&request);
            let tail = if failed {
                crate::TurnEvent::LlmRetry {
                    key,
                    delay_ms: 1,
                    notice: lattice_session::store::Notice {
                        level: lattice_session::store::NoticeLevel::Warn,
                        text: "provider down".into(),
                    },
                }
            } else {
                crate::TurnEvent::ModelFinished {
                    key,
                    finish_reason: Some("stop".into()),
                    usage: None,
                }
            };
            owner.observer.emit(tail.clone());
            let final_start = failed.then(|| crate::TurnEvent::ModelStarted {
                key: lancet_protocol::ModelAttemptKey {
                    attempt_ordinal: key.attempt_ordinal + 1,
                    ..key
                },
                provider: lattice_conversation::ProviderOrigin::OpenAiCompatible,
                model: "test".into(),
            });
            if let Some(start) = &final_start {
                owner.observer.emit(start.clone());
            }
            close(&backend, &request).await;
            let end = terminal(&backend, &request, failed).await;
            owner.observer.emit(end.clone());
            retire(&backend, &request, &owner, Ok(()));
            release.send(()).unwrap();
            assert_eq!(event(&mut reader, &mut socket).await, tail);
            if let Some(start) = final_start {
                assert_eq!(event(&mut reader, &mut socket).await, start);
            }
            assert_eq!(event(&mut reader, &mut socket).await, end);
            closing(&mut reader, &mut socket).await;
            task.await.unwrap().unwrap();
        }
    }

    #[tokio::test]
    async fn done_without_terminal_drains_window_then_finishes_by_snapshot() {
        let (_directory, backend, request) = pending_backend().await;
        let owner = owner(&backend, &request);
        let (mut socket, task) = subscribe(&backend, &request, None).await;
        let mut reader = ipc::ResponseReader::default();
        assert!(matches!(
            response(&mut reader, &mut socket).await,
            Response::Observation(Observation::Snapshot { .. })
        ));
        // 关闭已提交但执行核心随后失败：只有非终态尾部事件；退休不发 backend.changed，结束须由 done 状态推进。
        close(&backend, &request).await;
        let tail = crate::TurnEvent::ModelFinished {
            key: attempt(&request),
            finish_reason: None,
            usage: None,
        };
        owner.observer.emit(tail.clone());
        assert_eq!(event(&mut reader, &mut socket).await, tail);
        retire(&backend, &request, &owner, Err(internal()));
        closing(&mut reader, &mut socket).await;
        task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn retained_observer_out_of_window_after_exit_finishes_by_snapshot_once() {
        let (_directory, backend, request) = pending_backend().await;
        let owner = owner(&backend, &request);
        let (mut socket, mut reader, task, release) = park_subscriber(&backend, &request).await;
        for _ in 0..70 {
            owner.observer.emit(crate::TurnEvent::ModelFinished {
                key: attempt(&request),
                finish_reason: None,
                usage: None,
            });
        }
        assert!(owner.observer.events_after(0).is_none());
        close(&backend, &request).await;
        retire(&backend, &request, &owner, Ok(()));
        release.send(()).unwrap();
        // owned=false 快照的序号 0 不属于旧窗口：不能对齐后重读，只能以快照结束一次。
        assert!(!closing(&mut reader, &mut socket).await.owned);
        task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn orphaned_turn_allows_probe_and_rewind_without_changing_facts() {
        for rewind in [false, true] {
            let (_directory, backend, old) = pending_backend().await;
            let session_id = old.session_id.unwrap();
            let turn_id = old.turn_id;
            let api = backend.open().await.unwrap();
            let before = api
                .read(move |r| r.turn_detail(session_id, turn_id))
                .await
                .unwrap();
            let mut new = old.clone();
            new.turn_id = super::super::new_turn_id();
            new.input = "new".into();
            let response = backend
                .dispatch(if rewind {
                    Request::Rewind {
                        session_id,
                        target: NodeId(0),
                        workspace: old.workspace.clone(),
                    }
                } else {
                    Request::Submit {
                        request: new.clone(),
                        configuration: None,
                    }
                })
                .await
                .unwrap();
            assert!(matches!(
                response,
                Response::Rewind(_) | Response::Acceptance(Acceptance::NeedsConfiguration { .. })
            ));
            let after = api
                .read(move |r| r.turn_detail(session_id, turn_id))
                .await
                .unwrap();
            assert_eq!(after, before);
            assert_eq!(after.summary.outcome, PublicTurnOutcome::Failed);
            assert!(!after.summary.progress.is_closed());
            assert!(
                api.read(move |r| r.turn_node(new.turn_id))
                    .await
                    .unwrap()
                    .is_none()
            );
            let session = api.read(move |r| r.session(session_id)).await.unwrap();
            assert_eq!(
                session.current,
                if rewind {
                    NodeId(0)
                } else {
                    before.steps[0].node
                }
            );
        }
    }

    #[tokio::test]
    async fn rejected_identity_does_not_replace_original_diagnostic_or_durable_summary() {
        let (_directory, backend, request) = pending_backend().await;
        close(&backend, &request).await;
        let original = finished(&backend, &request).await;
        let diagnostic = LocalError::recovery(RecoveryCause::WorkerFailed);
        lock(&backend.admission)
            .diagnostics
            .insert(request.turn_id, diagnostic.clone());
        let mut bad = request.clone();
        bad.input.push('!');
        assert_eq!(
            backend
                .dispatch(Request::Submit {
                    request: bad,
                    configuration: None
                })
                .await
                .err()
                .unwrap()
                .kind,
            LocalErrorKind::Conflict
        );
        assert_eq!(
            lock(&backend.admission).diagnostics.get(&request.turn_id),
            Some(&diagnostic)
        );
        assert_eq!(finished(&backend, &request).await, original);
    }

    #[tokio::test]
    async fn snapshot_rechecks_owner_claimed_during_capture() {
        let (_directory, backend, request) = pending_backend().await;
        let (entered, release) = cancellation_tests::arm(&backend.gates.snapshot);
        let server = backend.clone();
        let query = request.clone();
        let task =
            tokio::spawn(async move { server.query(query.session_id, Some(query.turn_id)).await });
        entered.await.unwrap();
        let _owner = owner(&backend, &request);
        release.send(()).unwrap();
        assert!(
            task.await.unwrap().unwrap().owned,
            "snapshot must repeat after owner handoff"
        );
    }

    #[tokio::test]
    async fn snapshot_rechecks_owner_retired_during_capture() {
        for explicit_turn in [true, false] {
            let (_directory, backend, request) = pending_backend().await;
            let original = owner(&backend, &request);
            let (entered, release) = cancellation_tests::arm(&backend.gates.snapshot);
            let server = backend.clone();
            let query = request.clone();
            let task = tokio::spawn(async move {
                server
                    .query(query.session_id, explicit_turn.then_some(query.turn_id))
                    .await
            });
            entered.await.unwrap();
            close(&backend, &request).await;
            {
                let mut state = lock(&backend.admission);
                state.turns.remove(&request.turn_id);
                state.sessions.remove(&request.session_id.unwrap());
                state.owners.retain(|owner| !Arc::ptr_eq(owner, &original));
            }
            release.send(()).unwrap();
            let view = task.await.unwrap().unwrap();
            assert!(!view.owned);
            assert!(
                view.selected_turn.unwrap().summary.is_some(),
                "retired Owner must not turn an obsolete Running snapshot into NoOwner"
            );
            assert!(view.active.is_none());
        }
    }

    #[tokio::test]
    async fn subscription_before_acceptance_continuation_keeps_owner() {
        let (directory, backend, request) = cancellation_tests::setup().await;
        let mut provider = Provider::start().await;
        provider.release.send_replace(false);
        let (entered, release) = cancellation_tests::arm(&backend.gates.accepted);
        let server = backend.clone();
        let input = request.clone();
        let config = super::super::tests::frozen(directory.path(), &provider.endpoint);
        let task = tokio::spawn(async move {
            server
                .dispatch(Request::Submit {
                    request: input,
                    configuration: Some(Box::new(config)),
                })
                .await
        });
        entered.await.unwrap();
        tokio::time::timeout(Duration::from_secs(5), &mut provider.arrived)
            .await
            .unwrap()
            .unwrap();
        let view = backend
            .query(request.session_id, Some(request.turn_id))
            .await
            .unwrap();
        assert!(view.owned);
        assert!(view.selected_turn.unwrap().summary.is_none());
        release.send(()).unwrap();
        task.await.unwrap().unwrap();
        provider.finish().await;
        backend.dispatch(Request::Stop).await.unwrap();
        finished(&backend, &request).await;
    }

    #[tokio::test]
    async fn history_failure_after_summary_never_delivers_finished() {
        let (_directory, backend, request) = pending_backend().await;
        close(&backend, &request).await;
        let (entered, release) = cancellation_tests::arm(&backend.gates.history);
        let (mut socket, task) = subscribe(&backend, &request, None).await;
        let mut reader = ipc::ResponseReader::default();
        let Response::Observation(Observation::Snapshot { view }) =
            response(&mut reader, &mut socket).await
        else {
            panic!("summary first")
        };
        assert!(view.selected_turn.unwrap().summary.is_some());
        entered.await.unwrap();
        assert!(!task.is_finished());
        backend
            .open()
            .await
            .unwrap()
            .report_failure(&crate::ApiError::WorkerFailed);
        release.send(()).unwrap();
        assert!(matches!(
            response(&mut reader, &mut socket).await,
            Response::HistoryError(_)
        ));
        task.await.unwrap().unwrap();
        assert!(reader.read::<Response>(&mut socket).await.is_err());
    }

    #[tokio::test]
    async fn commit_fault_quiesces_other_session_until_reader_and_owner_really_retire() {
        use lattice_session::store::TestCommitFault;
        for fault in [
            TestCommitFault::Poisoned,
            TestCommitFault::UnknownBefore,
            TestCommitFault::UnknownAfter,
        ] {
            let (directory, backend, first) = pending_backend().await;
            let mut provider = Provider::start().await;
            provider.release.send_replace(false);
            let second = TurnRequest {
                session_id: None,
                turn_id: super::super::new_turn_id(),
                workspace: first.workspace.clone(),
                input: "second Session".into(),
                agent: first.agent.clone(),
            };
            let response = backend
                .dispatch(Request::Submit {
                    request: second.clone(),
                    configuration: Some(Box::new(super::super::tests::frozen(
                        directory.path(),
                        &provider.endpoint,
                    ))),
                })
                .await
                .unwrap();
            let Response::Acceptance(Acceptance::Accepted { turn }) = response else {
                panic!("accepted")
            };
            tokio::time::timeout(Duration::from_secs(10), &mut provider.arrived)
                .await
                .unwrap()
                .unwrap();
            let owner = lock(&backend.admission).turns[&second.turn_id].clone();
            let (retiring, release_owner) = cancellation_tests::arm(&backend.gates.retire);
            let api = backend.open().await.unwrap();
            let shutdown = api.shutdown();
            let (entered_tx, entered) = oneshot::channel();
            let (release_read, wait) = std::sync::mpsc::channel();
            let reading = api.clone();
            let reader = tokio::spawn(async move {
                reading
                    .read(move |read| {
                        entered_tx.send(()).unwrap();
                        wait.recv_timeout(Duration::from_secs(10)).unwrap();
                        read.list(None, 10)
                    })
                    .await
            });
            entered.await.unwrap();
            let sid = first.session_id.unwrap();
            let first_id = first.turn_id;
            let error = api
                .blocking(move |api| {
                    let guard = api.store.try_lock_session(sid)?;
                    Ok(lattice_session::store::with_test_commit_fault(
                        fault,
                        || {
                            api.store.close_step(
                                &guard,
                                lattice_session::store::CloseStep::StopBetweenRequests {
                                    turn_id: first_id,
                                    cause: StopCause::Cancelled,
                                },
                                chrono::Utc::now(),
                            )
                        },
                    )?)
                })
                .await
                .unwrap_err();
            assert!(
                matches!(error, crate::ApiError::Store(ref e) if matches!(e.as_ref(), StoreError::CommitFailed { .. }))
            );
            assert!(shutdown.load(Ordering::Acquire));
            assert!(
                !owner
                    .execution_cancel
                    .as_ref()
                    .unwrap()
                    .load(Ordering::Acquire),
                "库故障不能写用户取消载体"
            );
            let starts = std::sync::atomic::AtomicUsize::new(0);
            assert!(
                api.with_execution_start(|| starts.fetch_add(1, Ordering::SeqCst))
                    .is_err()
            );
            assert_eq!(starts.load(Ordering::SeqCst), 0);
            assert!(backend.open().await.is_err());
            drop(api);
            tokio::time::timeout(Duration::from_secs(10), retiring)
                .await
                .unwrap()
                .unwrap();
            assert!(owner.done.borrow().is_none());
            release_read.send(()).unwrap();
            reader.await.unwrap().unwrap();
            // completion 已取得、阻塞读取已退出，但监督者尚未移除 Owner。
            let mut pending_reopen = backend.stores.subscribe();
            let premature = tokio::time::timeout(Duration::from_millis(250), async {
                loop {
                    if backend.stores.ready_generation().is_some() {
                        break;
                    }
                    pending_reopen.changed().await.unwrap();
                }
            })
            .await;
            assert!(
                premature.is_err(),
                "旧代不得在 Owner 监督退休 gate 放行前重开"
            );
            release_owner.send(()).unwrap();
            let mut changed = backend.stores.subscribe();
            let reopened = tokio::time::timeout(Duration::from_secs(10), async {
                loop {
                    if let Ok(api) = backend.open().await {
                        break api;
                    }
                    changed.changed().await.unwrap();
                }
            })
            .await
            .unwrap();
            assert_eq!(backend.status().owners, 0);
            assert_eq!(backend.stores.ready_generation(), Some(1));
            assert!(!reopened.shutdown().load(Ordering::Acquire));
            let first_detail = reopened
                .read(move |r| r.turn_detail(sid, first_id))
                .await
                .unwrap();
            assert_eq!(
                first_detail.summary.progress.is_closed(),
                matches!(fault, TestCommitFault::UnknownAfter)
            );
            let second_detail = reopened
                .read(move |r| r.turn_detail(turn.session_id, turn.turn_id))
                .await
                .unwrap();
            assert!(!second_detail.summary.progress.is_closed());
            assert!(matches!(
                second_detail.steps[0].progress,
                lattice_session::store::StepProgress::Requesting
            ));
            assert_eq!(provider.finish().await.len(), 1);
        }
    }
}
