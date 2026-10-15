// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use super::{
    FrozenConfiguration, LocalClient, LocalSubscription,
    client::{SubmissionReply, SubscriptionItem, validate_history},
    types::*,
};
use std::{collections::VecDeque, time::Duration};
use tokio::{
    sync::{mpsc, watch},
    task::JoinHandle,
    time::Instant,
};

#[derive(Clone, Copy, Default)]
struct Commands {
    reconnect: u64,
    cancel: u64,
    detach: bool,
    stop: bool,
}
/// 控制、提交、观察及历史分别推进；普通 Drop 不向后台发送 Cancel。
pub struct LocalTurn {
    commands: watch::Sender<Commands>,
    abandon: watch::Sender<bool>,
    updates: mpsc::Receiver<LocalUpdate>,
    target: watch::Receiver<Option<TurnId>>,
    task: Option<JoinHandle<DetachReport>>,
}
impl LocalClient {
    pub fn observe(&self, session_id: Option<SessionId>, turn_id: Option<TurnId>) -> LocalTurn {
        let (mut turn, worker) = LocalTurn::prepare(self.clone(), session_id, turn_id);
        turn.task = Some(tokio::spawn(worker.run()));
        turn
    }
    pub fn start_turn_with<F, Fut>(&self, request: TurnRequest, load: F) -> LocalTurn
    where
        F: FnOnce(String) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = Result<FrozenConfiguration>> + Send + 'static,
    {
        let (mut turn, mut worker) =
            LocalTurn::prepare(self.clone(), request.session_id, Some(request.turn_id));
        let client = self.clone();
        let abandon = turn.abandon.subscribe();
        worker.submission = Some(tokio::spawn(async move {
            client.submit_handshake(request, load, Some(abandon)).await
        }));
        turn.task = Some(tokio::spawn(worker.run()));
        turn
    }
}
impl LocalTurn {
    fn prepare(
        client: LocalClient,
        session: Option<SessionId>,
        turn_id: Option<TurnId>,
    ) -> (Self, Worker) {
        let (commands, receiver) = watch::channel(Commands::default());
        let (sender, updates) = mpsc::channel(16);
        let (abandon, _) = watch::channel(false);
        let worker = Worker::new(client, session, turn_id, receiver, sender);
        (
            Self {
                commands,
                abandon,
                updates,
                target: worker.target.subscribe(),
                task: None,
            },
            worker,
        )
    }
    pub fn turn_id(&self) -> Option<TurnId> {
        *self.target.borrow()
    }
    pub fn cancel(&self) {
        self.abandon.send_replace(true);
        self.commands.send_modify(|c| c.cancel += 1);
    }
    pub fn reconnect(&self) {
        self.commands.send_modify(|c| c.reconnect += 1);
    }
    pub async fn next(&mut self) -> Option<LocalUpdate> {
        self.updates.recv().await
    }
    pub async fn detach(mut self) -> DetachReport {
        self.abandon.send_replace(true);
        self.commands.send_modify(|c| c.detach = true);
        self.task
            .take()
            .expect("owned worker")
            .await
            .expect("local observation worker")
    }
}
impl Drop for LocalTurn {
    fn drop(&mut self) {
        self.abandon.send_replace(true);
        self.commands.send_modify(|c| c.stop = true);
    }
}
struct BodyRead {
    anchor: HistoryAnchor,
    generation: u64,
    root_only: bool,
    task: JoinHandle<std::result::Result<LocalHistory, HistoryReadError>>,
}
impl Drop for BodyRead {
    fn drop(&mut self) {
        self.task.abort();
    }
}
type Connection = Result<(
    Option<TurnId>,
    Option<ObservationSnapshot>,
    Option<LocalSubscription>,
)>;
struct Worker {
    client: LocalClient,
    commands: watch::Receiver<Commands>,
    updates: mpsc::Sender<LocalUpdate>,
    pending: VecDeque<LocalUpdate>,
    report: DetachReport,
    target: watch::Sender<Option<TurnId>>,
    submission: Option<JoinHandle<Result<SubmissionReply>>>,
    submission_deadline: Option<Instant>,
    submission_handoff: Option<u64>,
    connection: Option<JoinHandle<Connection>>,
    subscription: Option<LocalSubscription>,
    progress: Option<watch::Receiver<Instant>>,
    body: Option<BodyRead>,
    body_progress: Option<watch::Receiver<Instant>>,
    generation: u64,
    needs_refix: bool,
    cancel_generation: u64,
    cancel_task: Option<(u64, JoinHandle<Result<bool>>)>,
    cancel_attempts: u8,
    live_evidence: bool,
    closed: bool,
    reconnect_generation: u64,
    detach_deadline: Option<Instant>,
    deadline: Instant,
}
impl Worker {
    fn new(
        client: LocalClient,
        session_id: Option<SessionId>,
        turn_id: Option<TurnId>,
        commands: watch::Receiver<Commands>,
        updates: mpsc::Sender<LocalUpdate>,
    ) -> Self {
        Self {
            client,
            commands,
            updates,
            pending: VecDeque::new(),
            report: DetachReport {
                session_id,
                turn_id,
                submission: None,
                submission_diagnostic: None,
                not_submitted: None,
                history_error: None,
                cancel: None,
                cancel_error: None,
                state: None,
            },
            target: watch::channel(turn_id).0,
            submission: None,
            submission_deadline: None,
            submission_handoff: None,
            connection: None,
            subscription: None,
            progress: None,
            body: None,
            body_progress: None,
            generation: 0,
            needs_refix: false,
            cancel_generation: 0,
            cancel_task: None,
            cancel_attempts: 0,
            live_evidence: false,
            closed: false,
            reconnect_generation: 0,
            detach_deadline: None,
            deadline: Instant::now() + Duration::from_secs(crate::STREAM_IDLE_TIMEOUT_SECS),
        }
    }
    fn select(&mut self, turn: Option<TurnId>) {
        self.report.turn_id = turn;
        self.target.send_replace(turn);
    }
    fn connect(&mut self) {
        self.deadline = Instant::now() + Duration::from_secs(crate::STREAM_IDLE_TIMEOUT_SECS);
        let client = self.client.clone();
        let session_id = self.report.session_id;
        let selected = self.report.turn_id;
        self.connection = Some(tokio::spawn(async move {
            let selected = match selected {
                Some(turn) => Some(turn),
                None => {
                    let view = client.query(session_id, None).await?;
                    if let Some(turn) = &view.selected_turn {
                        Some(turn.turn_id)
                    } else {
                        return Ok((None, Some(view), None));
                    }
                }
            };
            let subscription = client
                .subscribe(
                    TurnTarget {
                        session_id,
                        turn_id: selected.expect("selected Turn"),
                    },
                    None,
                )
                .await?;
            Ok((selected, None, Some(subscription)))
        }));
    }
    async fn retire_body(&mut self) {
        self.body_progress = None;
        if let Some(mut body) = self.body.take() {
            body.task.abort();
            let error = match (&mut body.task).await {
                Ok(Err(error)) => Some(error),
                Err(error) if !error.is_cancelled() => Some(HistoryReadError {
                    context: HistoryReadContext::Worker,
                    error: worker_error(),
                }),
                _ => None,
            };
            if let Some(error) = error {
                self.report.history_error = Some(error.clone());
                self.pending.push_back(LocalUpdate::History {
                    anchor: body.anchor.clone(),
                    generation: body.generation,
                    result: Err(error),
                    stale: true,
                });
            }
        }
    }
    fn cancel_status(&mut self, status: CancelStatus, error: Option<LocalError>) {
        self.report.cancel = Some(status);
        if error.is_some() || matches!(status, CancelStatus::Taken | CancelStatus::NotTaken) {
            self.report.cancel_error = error;
        }
        if self.detach_deadline.is_none() {
            if self.pending.len() >= 16 {
                self.pending
                    .retain(|u| !matches!(u, LocalUpdate::Cancel { .. }));
            }
            if let Some(turn_id) = self.report.turn_id {
                self.pending.push_back(LocalUpdate::Cancel {
                    turn_id,
                    status,
                    error: self.report.cancel_error.clone(),
                });
            }
        }
    }
    fn drive_cancel(&mut self) {
        if self.cancel_generation == 0 || self.cancel_task.is_some() {
            return;
        }
        if self.closed {
            if !matches!(
                self.report.cancel,
                Some(CancelStatus::Taken | CancelStatus::Moot)
            ) {
                self.cancel_status(CancelStatus::Moot, None);
            }
            return;
        }
        let retry = self.cancel_attempts == 1
            && self.live_evidence
            && matches!(
                self.report.cancel,
                Some(CancelStatus::NotTaken | CancelStatus::Unknown)
            );
        if self.cancel_attempts != 0 && !retry {
            return;
        }
        let Some(turn_id) = self.report.turn_id else {
            return;
        };
        self.cancel_attempts += 1;
        let client = self.client.clone();
        let target = TurnTarget {
            session_id: self.report.session_id,
            turn_id,
        };
        self.cancel_status(CancelStatus::Pending, None);
        self.cancel_task = Some((
            self.cancel_generation,
            tokio::spawn(async move {
                tokio::time::timeout(Duration::from_secs(5), client.cancel(target))
                    .await
                    .unwrap_or_else(|_| {
                        Err(LocalError::new(
                            LocalErrorKind::Transport,
                            "cancel acknowledgement unknown",
                        ))
                    })
            }),
        ));
    }
    async fn fixed(&mut self, view: Box<ObservationSnapshot>) {
        self.report.session_id = Some(view.session.session_id);
        self.live_evidence |= view.owned;
        self.closed |= view
            .selected_turn
            .as_ref()
            .is_some_and(|t| t.summary.progress.is_closed() || !view.owned);
        let phase = if view.owned {
            ObservationPhase::Live
        } else {
            ObservationPhase::NoOwner
        };
        self.report.state = Some(LocalState {
            view: view.clone(),
            phase,
            error: None,
        });
        if self.detach_deadline.is_some() {
            return;
        }
        if self.pending.len() >= 16 {
            self.pending.retain(|u| {
                !matches!(
                    u,
                    LocalUpdate::Event { .. }
                        | LocalUpdate::Reset { .. }
                        | LocalUpdate::State { .. }
                        | LocalUpdate::History { result: Ok(_), .. }
                )
            });
        }
        // 先排入状态，再处理正文任务的退休；慢正文不能遮蔽 summary。
        self.pending.push_back(LocalUpdate::State {
            view: view.clone(),
            phase,
            error: None,
        });
        self.retire_body().await;
        self.generation += 1;
        self.pending.push_back(LocalUpdate::Reset {
            view: view.clone(),
            generation: self.generation,
        });
        self.needs_refix = false;
        let client = self.client.clone();
        let (progress, body_progress) = watch::channel(Instant::now());
        self.body_progress = Some(body_progress);
        self.body = Some(BodyRead {
            anchor: view.history.clone(),
            generation: self.generation,
            root_only: view.selected_turn.is_none(),
            task: tokio::spawn(async move {
                client
                    .read_history_with_progress(&view, || {
                        progress.send_replace(Instant::now());
                    })
                    .await
            }),
        });
    }
    async fn observed(&mut self, item: Result<Option<SubscriptionItem>>) {
        self.deadline = Instant::now() + Duration::from_secs(crate::STREAM_IDLE_TIMEOUT_SECS);
        match item {
            Ok(Some(SubscriptionItem::Observation(Observation::Snapshot { view }))) => {
                self.fixed(view).await;
            }
            Ok(Some(SubscriptionItem::Observation(Observation::Diagnostic { error }))) => {
                if let Some(state) = &mut self.report.state {
                    state.error = Some(error.clone());
                    self.pending.push_back(LocalUpdate::State {
                        view: state.view.clone(),
                        phase: state.phase,
                        error: Some(error),
                    });
                }
            }
            Ok(Some(SubscriptionItem::Observation(Observation::Finished {
                turn,
                cursor,
                history,
            }))) => {
                let valid = self.report.state.as_ref().is_some_and(|state| {
                    state.view.history == history.anchor
                        && state.view.cursor == cursor
                        && state.view.selected_turn.as_ref().map(|t| TurnRef {
                            session_id: t.session_id,
                            turn_id: t.turn_id,
                        }) == turn
                        && state
                            .view
                            .selected_turn
                            .as_ref()
                            .is_none_or(|t| t.summary.progress.is_closed() || !state.view.owned)
                });
                if !valid || validate_history(&history).is_err() {
                    self.subscription = None;
                    self.progress = None;
                    self.pending
                        .push_back(LocalUpdate::Connection(ConnectionStatus::Error(
                            LocalError::new(LocalErrorKind::Protocol, "invalid final history"),
                        )));
                    return;
                }
                self.retire_body().await;
                self.report.history_error = None;
                let anchor = history.anchor.clone();
                self.pending.push_back(LocalUpdate::History {
                    anchor: anchor.clone(),
                    generation: self.generation,
                    result: Ok(Box::new(history)),
                    stale: false,
                });
                self.pending.push_back(LocalUpdate::Finished {
                    turn,
                    anchor,
                    generation: self.generation,
                });
                if let Some(state) = &mut self.report.state {
                    state.phase = ObservationPhase::Finished;
                }
                self.subscription = None;
                self.progress = None;
            }
            Ok(Some(SubscriptionItem::HistoryError(error))) => {
                self.retire_body().await;
                self.report.history_error = Some(error.clone());
                if let Some(state) = &self.report.state {
                    self.pending.push_back(LocalUpdate::History {
                        anchor: state.view.history.clone(),
                        generation: self.generation,
                        result: Err(error),
                        stale: false,
                    });
                }
                self.subscription = None;
                self.progress = None;
            }
            Ok(Some(SubscriptionItem::Observation(Observation::Event { cursor, event }))) => {
                if self.detach_deadline.is_some() || self.needs_refix {
                    return;
                }
                if self.pending.len() >= 16 {
                    self.pending
                        .retain(|u| !matches!(u, LocalUpdate::Event { .. }));
                    self.needs_refix = true;
                    return;
                }
                self.pending.push_back(LocalUpdate::Event { cursor, event });
            }
            Ok(None) => {
                self.subscription = None;
                self.progress = None;
                self.pending
                    .push_back(LocalUpdate::Connection(ConnectionStatus::Eof));
            }
            Err(error) => {
                self.subscription = None;
                self.progress = None;
                // 订阅在服务端首次查询时才发现目标未创建，处理同连接结果。
                if !(self.submission.is_some() && target_not_created(&error)) {
                    self.pending
                        .push_back(LocalUpdate::Connection(ConnectionStatus::Error(error)));
                }
            }
        }
    }
    fn install_subscription(&mut self, subscription: Option<LocalSubscription>) {
        self.progress = subscription.as_ref().map(|s| s.progress.clone());
        self.subscription = subscription;
    }
    async fn retire_observation(&mut self) {
        if let Some(task) = self.connection.take() {
            task.abort();
            if let Ok(Ok((selected, view, subscription))) = task.await {
                self.select(selected);
                self.install_subscription(subscription);
                if let Some(view) = view {
                    self.fixed(Box::new(view)).await;
                }
            }
        }
        if let Some(mut subscription) = self.subscription.take() {
            for item in subscription.close().await {
                self.observed(item.map(Some)).await;
            }
        }
        self.progress = None;
    }
    async fn handoff_submission(&mut self) {
        self.submission_deadline = None;
        self.submission_handoff = Some(self.cancel_generation);
        self.retire_observation().await;
        if self
            .report
            .state
            .as_ref()
            .is_none_or(|s| s.phase != ObservationPhase::Finished)
        {
            self.retire_body().await;
            self.needs_refix = false;
            self.connect();
        }
        self.pending.push_back(LocalUpdate::SubmissionUnknown {
            turn_id: self.report.turn_id.expect("submission identity"),
            error: None,
        });
    }
    async fn submitted(&mut self, result: Result<SubmissionReply>) {
        let turn_id = self.report.turn_id.expect("submission identity");
        let operation_failed = matches!(&result, Ok(SubmissionReply::BackendFailed(_)));
        let result = match result {
            Ok(SubmissionReply::NotSubmitted(stage)) => {
                self.report.not_submitted = Some(stage);
                self.pending
                    .push_back(LocalUpdate::NotSubmitted { turn_id, stage });
                return;
            }
            Ok(SubmissionReply::Accepted(acceptance)) => Ok(acceptance),
            Err(error)
                if self.submission_handoff.is_some() && error.kind == LocalErrorKind::Transport =>
            {
                self.report.submission_diagnostic = Some(error.clone());
                self.pending.push_back(LocalUpdate::SubmissionUnknown {
                    turn_id,
                    error: Some(error),
                });
                return;
            }
            Ok(
                SubmissionReply::ConfigurationFailed(error) | SubmissionReply::BackendFailed(error),
            )
            | Err(error) => Err(error),
        };
        self.report.submission = Some(result.clone());
        self.pending.push_back(LocalUpdate::Submission {
            turn_id,
            result: result.clone(),
        });
        match result {
            Ok(Acceptance::Accepted { turn } | Acceptance::Existing { turn }) => {
                self.report.session_id = Some(turn.session_id);
                self.live_evidence |= turn.turn_id == turn_id;
                self.closed |= turn.summary.progress.is_closed();
            }
            Ok(Acceptance::ConfirmationUnknown { session_id, .. }) => {
                self.report.session_id = session_id;
            }
            Err(error) => {
                if operation_failed
                    && matches!(
                        error.kind,
                        LocalErrorKind::BadRequest
                            | LocalErrorKind::NotFound
                            | LocalErrorKind::Conflict
                            | LocalErrorKind::Busy
                            | LocalErrorKind::Stopping
                    )
                {
                    self.closed = true;
                }
                return;
            }
            Ok(Acceptance::NeedsConfiguration { .. }) => {
                unreachable!("handshake consumes configuration request")
            }
        }
        self.retire_observation().await;
        self.retire_body().await;
        self.select(Some(turn_id));
        self.connect();
    }
    async fn run(mut self) -> DetachReport {
        self.connect();
        let updates = self.updates.clone();
        loop {
            self.drive_cancel();
            if self.detach_deadline.is_none() && self.pending.is_empty() {
                let reconnect = self.commands.borrow().reconnect;
                let explicit = reconnect != self.reconnect_generation;
                if explicit || self.needs_refix {
                    self.reconnect_generation = reconnect;
                    self.retire_observation().await;
                    if explicit
                        || self.needs_refix
                        || self
                            .report
                            .state
                            .as_ref()
                            .is_some_and(|s| s.phase == ObservationPhase::Live)
                    {
                        self.retire_body().await;
                        self.needs_refix = false;
                        self.connect();
                    }
                }
            }
            if self.detach_deadline.is_some()
                && (self.cancel_generation == 0
                    || self.report.cancel == Some(CancelStatus::Taken)
                    || (self.cancel_task.is_none()
                        && (self.closed
                            || self.cancel_attempts == 2
                            || self.submission_handoff == Some(self.cancel_generation))))
            {
                break;
            }
            tokio::select! {
                biased;
                changed = self.commands.changed() => {
                    if changed.is_err() { break; }
                    let command = *self.commands.borrow_and_update();
                    if command.stop { break; }
                    if command.cancel != self.cancel_generation {
                        if self.cancel_generation == 0 && self.submission.is_some() { self.submission_deadline = Some(Instant::now() + Duration::from_secs(5)); }
                        self.cancel_generation = command.cancel;
                        self.report.cancel_error = None;
                        self.cancel_attempts = 0;
                        if !self.closed || self.report.cancel != Some(CancelStatus::Taken) { self.cancel_status(CancelStatus::Pending, None); }
                    }
                    if command.detach && self.detach_deadline.is_none() {
                        self.detach_deadline = Some(Instant::now() + Duration::from_secs(5));
                        self.retire_body().await;
                        self.pending.clear();
                    }
                }
                result = async { self.submission.as_mut().expect("submitting").await }, if self.submission.is_some() => {
                    self.submission = None; self.submission_deadline = None;
                    self.submitted(result.unwrap_or_else(|_| Err(worker_error()))).await;
                }
                result = async { (&mut self.cancel_task.as_mut().expect("cancelling").1).await }, if self.cancel_task.is_some() => {
                    let (generation, _) = self.cancel_task.take().expect("cancelling");
                    if generation == self.cancel_generation {
                        match result.unwrap_or_else(|_| Err(worker_error())) {
                            Ok(true) => self.cancel_status(CancelStatus::Taken, None),
                            Ok(false) => self.cancel_status(CancelStatus::NotTaken, None),
                            Err(error) => self.cancel_status(CancelStatus::Unknown, Some(error)),
                        }
                    }
                }
                _ = async { tokio::time::sleep_until(self.detach_deadline.expect("detaching")).await }, if self.detach_deadline.is_some() => {
                    if self.cancel_task.is_some() { self.cancel_status(CancelStatus::Unknown, None); }
                    break;
                }
                _ = async { tokio::time::sleep_until(self.submission_deadline.expect("waiting")).await }, if self.submission_deadline.is_some() && self.detach_deadline.is_none() => { self.handoff_submission().await; }
                permit = updates.reserve(), if !self.pending.is_empty() => {
                    match permit { Ok(permit) => permit.send(self.pending.pop_front().expect("queued")), Err(_) => break }
                }
                result = async { self.connection.as_mut().expect("connecting").await }, if self.connection.is_some() => {
                    self.connection = None;
                    match result.unwrap_or_else(|_| Err(worker_error())) {
                        Ok((selected, view, subscription)) => {
                            self.select(selected); self.install_subscription(subscription);
                            if let Some(view) = view { self.fixed(Box::new(view)).await; }
                        }
                        // 自己的提交尚未有结果时，目标 Turn 或（新会话的）会话库还没有创建是预期的：
                        // 这不是观察故障，接受、找回或交接后会重新订阅。
                        Err(error) if self.submission.is_some() && target_not_created(&error) => {}
                        Err(error) => self.pending.push_back(LocalUpdate::Connection(ConnectionStatus::Error(error))),
                    }
                }
                item = async { self.subscription.as_mut().expect("subscribed").next_item().await }, if self.subscription.is_some() => { self.observed(item).await; }
                progress = async { self.progress.as_mut().expect("progress").changed().await }, if self.progress.is_some() => {
                    if progress.is_err() { self.progress = None; }
                    else { self.deadline = Instant::now() + Duration::from_secs(crate::STREAM_IDLE_TIMEOUT_SECS); }
                }
                progress = async { self.body_progress.as_mut().expect("reading progress").changed().await }, if self.body_progress.is_some() => {
                    if progress.is_ok() { self.deadline = Instant::now() + Duration::from_secs(crate::STREAM_IDLE_TIMEOUT_SECS); }
                    else { self.body_progress = None; }
                }
                result = async { (&mut self.body.as_mut().expect("reading").task).await }, if self.body.is_some() && self.pending.len() < 16 => {
                    let body = self.body.take().expect("reading");
                    self.body_progress = None;
                    let result = match result {
                        Ok(result) => result,
                        Err(_) => Err(HistoryReadError { context: HistoryReadContext::Worker, error: worker_error() }),
                    };
                    let root_finished = body.root_only && result.is_ok();
                    self.report.history_error = result.as_ref().err().cloned();
                    self.pending.push_back(LocalUpdate::History { anchor: body.anchor.clone(), generation: body.generation, result: result.map(Box::new), stale: false });
                    if root_finished {
                        self.pending.push_back(LocalUpdate::Finished { turn: None, anchor: body.anchor.clone(), generation: body.generation });
                        if let Some(state) = &mut self.report.state { state.phase = ObservationPhase::Finished; }
                    }
                }
                _ = tokio::time::sleep_until(self.deadline), if self.subscription.is_some() || self.body.is_some() => {
                    if self.subscription.is_some() {
                        self.retire_observation().await;
                        if self.report.state.as_ref().is_none_or(|s| s.phase != ObservationPhase::Finished) { self.pending.push_back(LocalUpdate::Connection(ConnectionStatus::Idle)); }
                    }
                    if let Some(mut body) = self.body.take() {
                        self.body_progress = None;
                        body.task.abort();
                        let result = match (&mut body.task).await {
                            Ok(result) => result,
                            Err(error) => Err(HistoryReadError { context: HistoryReadContext::Worker,
                                error: if error.is_cancelled() { LocalError::new(LocalErrorKind::Transport, "fixed history read timed out") } else { worker_error() } }),
                        };
                        self.report.history_error = result.as_ref().err().cloned();
                        self.pending.push_back(LocalUpdate::History { anchor: body.anchor.clone(), generation: body.generation, result: result.map(Box::new), stale: false });
                    }
                }
            }
        }
        self.retire_observation().await;
        if let Some(task) = self.submission.take() {
            task.abort();
            let _ = task.await;
        }
        if let Some((_, task)) = self.cancel_task.take() {
            task.abort();
            let _ = task.await;
        }
        self.retire_body().await;
        self.report
    }
}
fn target_not_created(error: &LocalError) -> bool {
    error.kind == LocalErrorKind::NotFound
        || matches!(
            error.store.as_deref(),
            Some(StoreDiagnostic::NotInitialized { .. })
        )
}
fn worker_error() -> LocalError {
    LocalError::new(LocalErrorKind::Internal, "local worker failed")
}
