// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use super::{
    FrozenConfiguration, LocalClient, LocalSubscription, client::SubmissionReply, types::*,
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

/// 一个本地观察生命周期；控制与正文任务由句柄管理，不由 UI 的轮询驱动。
pub struct LocalTurn {
    commands: watch::Sender<Commands>,
    abandon: watch::Sender<bool>,
    updates: mpsc::Receiver<LocalUpdate>,
    target: watch::Receiver<Option<TurnId>>,
    task: Option<JoinHandle<DetachReport>>,
}

impl LocalClient {
    /// 只读观察；未指定 Turn 时先固定 Session，再选 active、head，空历史不订阅。
    pub fn observe(
        &self,
        session_id: String,
        turn_id: Option<TurnId>,
        history: HistoryScope,
    ) -> LocalTurn {
        let (mut turn, worker) = LocalTurn::prepare(self.clone(), session_id, turn_id, history);
        turn.task = Some(tokio::spawn(worker.run()));
        turn
    }

    /// 以原身份惰性提交；首轮无配置探测直接返回 Existing 或 Busy 时不调用 load。
    /// 收到 NeedsConfiguration 后最多加载一次，后续准入仍可能因并发变化返回这些结果。
    pub fn start_turn_with<F, Fut>(
        &self,
        request: TurnRequest,
        history: HistoryScope,
        load: F,
    ) -> LocalTurn
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: std::future::Future<Output = Result<FrozenConfiguration>> + Send + 'static,
    {
        let (mut turn, mut worker) = LocalTurn::prepare(
            self.clone(),
            request.session_id.clone(),
            Some(request.turn_id),
            history,
        );
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
        session_id: String,
        turn_id: Option<TurnId>,
        history: HistoryScope,
    ) -> (Self, Worker) {
        let (commands, receiver) = watch::channel(Commands::default());
        let (sender, updates) = mpsc::channel(16);
        let (abandon, _) = watch::channel(false);
        let worker = Worker::new(client, session_id, turn_id, history, receiver, sender);
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

    /// 提交、观察与取消共用的目标；observe(None) 完成首次选择前为 None。
    pub fn turn_id(&self) -> Option<TurnId> {
        *self.target.borrow()
    }

    /// 同步登记原目标取消意图；已有在途请求先收割，重复输入不产生并发 Cancel。
    pub fn cancel(&self) {
        self.abandon.send_replace(true);
        self.commands.send_modify(|commands| commands.cancel += 1);
    }

    /// 重新固定只读视图，不重发输入，也不丢失本句柄已经取得的完成确认。
    pub fn reconnect(&self) {
        self.commands
            .send_modify(|commands| commands.reconnect += 1);
    }

    /// cancel-safe：丢弃本次等待不会丢失半帧或已接收的更新。
    /// 完成/正文结束后仍可 reconnect；正常停止由 detach 或 Drop 决定，不以事件 EOF 判执行结果。
    pub async fn next(&mut self) -> Option<LocalUpdate> {
        self.updates.recv().await
    }

    /// 放弃尚未发送的配置和本地正文。已有取消意图时至多等待五秒交接控制回执；
    /// Taken 后立即返回，不等待 Turn 关闭。普通退出不发送 Cancel。
    pub async fn detach(mut self) -> DetachReport {
        self.abandon.send_replace(true);
        self.commands.send_modify(|commands| commands.detach = true);
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
        self.commands.send_modify(|commands| commands.stop = true);
    }
}

struct BodyRead {
    view: Box<SessionObservation>,
    task: JoinHandle<std::result::Result<LocalHistory, HistoryReadError>>,
}

impl Drop for BodyRead {
    fn drop(&mut self) {
        self.task.abort();
    }
}

type Connection = Result<(
    Option<TurnId>,
    Option<SessionObservation>,
    Option<LocalSubscription>,
)>;

struct Worker {
    client: LocalClient,
    history: HistoryScope,
    commands: watch::Receiver<Commands>,
    updates: mpsc::Sender<LocalUpdate>,
    pending: VecDeque<LocalUpdate>,
    report: DetachReport,
    target: watch::Sender<Option<TurnId>>,
    submission: Option<JoinHandle<Result<SubmissionReply>>>,
    submission_deadline: Option<Instant>,
    /// 已交接提交等待的取消代次；后续明确取消仍有自己的退出回执窗口。
    submission_handoff: Option<u64>,
    connection: Option<JoinHandle<Connection>>,
    subscription: Option<LocalSubscription>,
    body: Option<BodyRead>,
    body_view: Option<Box<SessionObservation>>,
    needs_refix: bool,
    confirmed: Option<(TurnId, String)>,
    cancel_generation: u64,
    cancel_task: Option<(u64, JoinHandle<Result<bool>>)>,
    cancel_attempts: u8,
    live_evidence: bool,
    closed: bool,
    reconnect_generation: u64,
    detach_deadline: Option<Instant>,
    text_open: bool,
    deadline: Instant,
}

impl Worker {
    fn new(
        client: LocalClient,
        session_id: String,
        turn_id: Option<TurnId>,
        history: HistoryScope,
        commands: watch::Receiver<Commands>,
        updates: mpsc::Sender<LocalUpdate>,
    ) -> Self {
        Self {
            client,
            history,
            commands,
            updates,
            pending: VecDeque::new(),
            report: DetachReport {
                session_id,
                turn_id,
                cancel: None,
                cancel_error: None,
                submission: None,
                submission_diagnostic: None,
                not_submitted: None,
                history_error: None,
                state: None,
            },
            target: watch::channel(turn_id).0,
            submission: None,
            submission_deadline: None,
            submission_handoff: None,
            connection: None,
            subscription: None,
            body: None,
            body_view: None,
            needs_refix: false,
            confirmed: None,
            cancel_generation: 0,
            cancel_task: None,
            cancel_attempts: 0,
            live_evidence: false,
            closed: false,
            reconnect_generation: 0,
            detach_deadline: None,
            text_open: false,
            deadline: Instant::now() + Duration::from_secs(crate::STREAM_IDLE_TIMEOUT_SECS),
        }
    }

    fn select(&mut self, selected: Option<TurnId>) {
        self.report.turn_id = selected;
        self.target.send_replace(selected);
    }

    fn connect(&mut self) {
        self.deadline = Instant::now() + Duration::from_secs(crate::STREAM_IDLE_TIMEOUT_SECS);
        let client = self.client.clone();
        let session = self.report.session_id.clone();
        let selected = self.report.turn_id;
        self.connection = Some(tokio::spawn(async move {
            let selected = match selected {
                Some(id) => Some(id),
                None => {
                    let view = client.query(&session, None).await?;
                    if let Some(active) = &view.active_turn {
                        Some(active.turn_id)
                    } else if let Some(head) = &view.head_commit_id {
                        Some(client.read_commit(&session, head).await?.turn_id)
                    } else {
                        return Ok((None, Some(view), None));
                    }
                }
            };
            let subscription = client
                .subscribe(&session, selected.expect("selected Turn"), None)
                .await?;
            Ok((selected, None, Some(subscription)))
        }));
    }

    async fn retire_body(&mut self) {
        if let Some(mut body) = self.body.take() {
            #[cfg(test)]
            if let Some(gate) = &self.client.body_retirement
                && gate.matches(&body.view)
            {
                gate.retiring.notify_one();
                gate.release.notified().await;
                assert!(
                    body.task.is_finished(),
                    "old IPC read must have completed before retirement"
                );
            }
            // abort 后 await 收割：已完成的失败保留，未完成的 socket 读取退场；旧成功不发布。
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
                    view: body.view.clone(),
                    result: Err(error),
                    stale: true,
                });
            }
        }
    }

    async fn reset(&mut self, reason: ResetReason, cursor: Option<EventCursor>) {
        self.text_open = false;
        self.body_view = None;
        if self.pending.len() >= 16 || reason == ResetReason::Reconnect {
            self.pending.retain(|update| {
                !matches!(update, LocalUpdate::Reset { reason: prior, .. } if *prior == reason)
            });
        }
        if self.detach_deadline.is_none() {
            self.pending
                .push_back(LocalUpdate::Reset { reason, cursor });
        }
        self.retire_body().await;
    }

    fn cancel_status(&mut self, status: CancelStatus, error: Option<LocalError>) {
        self.report.cancel = Some(status);
        if error.is_some() || matches!(status, CancelStatus::Taken | CancelStatus::NotTaken) {
            self.report.cancel_error = error;
        }
        if self.detach_deadline.is_none() {
            // UI 堵塞时合并控制显示，完整控制事实仍由本任务持有。
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
        let session = self.report.session_id.clone();
        self.cancel_status(CancelStatus::Pending, None);
        self.cancel_task = Some((
            self.cancel_generation,
            tokio::spawn(async move {
                tokio::time::timeout(Duration::from_secs(5), client.cancel(&session, turn_id))
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

    async fn fixed(
        &mut self,
        view: Box<SessionObservation>,
        cursor: Option<EventCursor>,
        phase: ObservationPhase,
        error: Option<LocalError>,
    ) {
        let identity = match &view.requested_turn {
            Some(TurnObservation::Committed(turn)) => Some((turn.turn_id, turn.commit_id.clone())),
            _ => None,
        };
        if phase == ObservationPhase::Finished && error.is_none() && identity.is_some() {
            self.confirmed = identity.clone();
        }
        let confirmation = if identity.is_some() && identity == self.confirmed {
            CompletionConfirmation::Confirmed
        } else {
            CompletionConfirmation::Unknown
        };
        if phase == ObservationPhase::Live {
            self.live_evidence = true;
        }
        if identity.is_some() {
            self.closed = true;
        }
        self.report.state = Some(LocalState {
            view: view.clone(),
            phase,
            confirmation,
            error: error.clone(),
        });
        if self.detach_deadline.is_some() {
            return;
        }
        if self.pending.len() >= 16 {
            // 旧显示切点可由新固定视图替代；完成回执和已发生的错误不参与合并。
            let lost_events = self
                .pending
                .iter()
                .any(|u| matches!(u, LocalUpdate::Event { .. }));
            self.pending.retain(|u| {
                !matches!(
                    u,
                    LocalUpdate::Event { .. }
                        | LocalUpdate::State {
                            phase: ObservationPhase::Live | ObservationPhase::NoOwner,
                            ..
                        }
                        | LocalUpdate::Reset {
                            reason: ResetReason::Snapshot | ResetReason::Reconnect,
                            ..
                        }
                        | LocalUpdate::History { result: Ok(_), .. }
                )
            });
            if lost_events {
                self.reset(ResetReason::Gap, cursor.clone()).await;
            }
        }
        self.pending.push_back(LocalUpdate::State {
            view: view.clone(),
            cursor: cursor.clone(),
            phase,
            confirmation,
            error,
        });
        self.reset(ResetReason::Snapshot, cursor).await;
        self.needs_refix = false;
        self.body_view = Some(view);
    }

    async fn observed(&mut self, observation: Result<Option<Observation>>) {
        self.deadline = Instant::now() + Duration::from_secs(crate::STREAM_IDLE_TIMEOUT_SECS);
        match observation {
            Ok(Some(Observation::Snapshot {
                view,
                cursor,
                owned,
            })) => {
                self.fixed(
                    view,
                    Some(cursor),
                    if owned {
                        ObservationPhase::Live
                    } else {
                        ObservationPhase::NoOwner
                    },
                    None,
                )
                .await;
                if !owned {
                    self.subscription = None;
                }
            }
            Ok(Some(Observation::Finished {
                view,
                cursor,
                error,
            })) => {
                self.fixed(view, Some(cursor), ObservationPhase::Finished, error)
                    .await;
                self.subscription = None;
            }
            Ok(Some(Observation::Gap { cursor })) => {
                self.reset(ResetReason::Gap, Some(cursor)).await
            }
            Ok(Some(Observation::Event { cursor, event })) => {
                if self.detach_deadline.is_some() || self.needs_refix {
                    return;
                }
                if self.pending.len() >= 16 {
                    self.pending
                        .retain(|u| !matches!(u, LocalUpdate::Event { .. }));
                    self.reset(ResetReason::Gap, Some(cursor)).await;
                    self.needs_refix = true;
                    return;
                }
                let text = match &event {
                    crate::TurnEvent::ModelStarted { .. } => {
                        self.text_open = true;
                        TextAction::Start
                    }
                    crate::TurnEvent::TokenDelta { .. } if !self.text_open => {
                        self.text_open = true;
                        TextAction::StartIncomplete
                    }
                    crate::TurnEvent::TokenDelta { .. } => TextAction::Continue,
                    crate::TurnEvent::ModelFinished { .. } => {
                        self.text_open = false;
                        TextAction::Finish
                    }
                    _ => TextAction::None,
                };
                self.pending.push_back(LocalUpdate::Event {
                    cursor,
                    event,
                    text,
                });
            }
            Ok(None) => {
                self.subscription = None;
                self.pending
                    .push_back(LocalUpdate::Connection(ConnectionStatus::Eof));
            }
            Err(error) => {
                self.subscription = None;
                self.pending
                    .push_back(LocalUpdate::Connection(ConnectionStatus::Error(error)));
            }
        }
    }

    async fn retire_observation(&mut self) {
        // 建连任务可能已返回带 reader 的连接，abort 不会撤销已完成结果。
        // 收割后同样排空已解码控制帧，不能直接丢弃 JoinHandle 的成功返回值。
        if let Some(task) = self.connection.take() {
            task.abort();
            if let Ok(Ok((selected, view, subscription))) = task.await {
                self.select(selected);
                self.subscription = subscription;
                if let Some(view) = view {
                    self.fixed(Box::new(view), None, ObservationPhase::NoOwner, None)
                        .await;
                }
            }
        }
        if let Some(mut subscription) = self.subscription.take() {
            for observation in subscription.close().await {
                self.observed(observation.map(Some)).await;
            }
        }
    }

    async fn handoff_submission(&mut self) {
        self.submission_deadline = None;
        self.submission_handoff = Some(self.cancel_generation);
        // 接受前的 NoOwner 可能早于后台接管。等待结束后重新固定只读观察，
        // 保留提交任务收取晚到回执；abandon 已阻止其继续发送配置。
        self.retire_observation().await;
        if self
            .report
            .state
            .as_ref()
            .is_none_or(|state| state.phase != ObservationPhase::Finished)
        {
            self.reset(ResetReason::Reconnect, None).await;
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
        // 只有后台明确拒绝能结束本次操作；本地 load 和回执传输失败不能冒充拒绝。
        let operation_failed = matches!(&result, Ok(SubmissionReply::BackendFailed(_)));
        let result = match result {
            Ok(SubmissionReply::NotSubmitted(stage)) => {
                self.report.not_submitted = Some(stage);
                self.pending
                    .push_back(LocalUpdate::NotSubmitted { turn_id, stage });
                // 只停止此连接后续配置，probe/另一客户端的接管与 live 事实不变。
                return;
            }
            Ok(SubmissionReply::Accepted(acceptance)) => Ok(acceptance),
            Err(error)
                if self.submission_handoff.is_some() && error.kind == LocalErrorKind::Transport =>
            {
                // 原连接只丢失接受回执；独立观察与取消事实继续有效。
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
        let selected = match result {
            Ok(
                Acceptance::Accepted { turn_id: id }
                | Acceptance::ConfirmationUnknown { turn_id: id },
            ) => {
                self.live_evidence |= id == turn_id;
                Some(id)
            }
            Ok(Acceptance::Existing {
                turn: TurnObservation::Pending(pending),
            }) => {
                self.live_evidence |= pending.turn_id == turn_id;
                Some(pending.turn_id)
            }
            Ok(Acceptance::Existing {
                turn: TurnObservation::Committed(turn),
            }) => {
                self.closed = true;
                self.confirmed = Some((turn.turn_id, turn.commit_id.clone()));
                Some(turn.turn_id)
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
            Ok(Acceptance::NeedsConfiguration) => {
                unreachable!("private handshake consumes configuration request")
            }
        };
        // 接受前建立的连接不证明其切点覆盖这次接管；由客户端重新固定，
        // 不让迟到 NoOwner 截断已接受请求，也不重发配置。
        self.retire_observation().await;
        self.select(selected);
        self.reset(ResetReason::Reconnect, None).await;
        self.connect();
    }

    async fn run(mut self) -> DetachReport {
        self.connect();
        let updates = self.updates.clone();
        loop {
            self.drive_cancel();
            // 不因重复重连/快照在慢 UI 后堆积正文和终态；仍持续收取当前连接的控制事实。
            if self.detach_deadline.is_none() && self.pending.is_empty() {
                let reconnect = self.commands.borrow().reconnect;
                let explicit = reconnect != self.reconnect_generation;
                if explicit || self.needs_refix {
                    self.reconnect_generation = reconnect;
                    #[cfg(test)]
                    if !explicit && let Some(gate) = self.client.refix_retirement.take() {
                        gate.retiring.notify_one();
                        gate.release.notified().await;
                    }
                    self.retire_observation().await;
                    // 收割的 Live 快照只能替换切点；旧连接已关闭，仍须接续订阅。
                    if explicit
                        || self.needs_refix
                        || self
                            .report
                            .state
                            .as_ref()
                            .is_some_and(|state| state.phase == ObservationPhase::Live)
                    {
                        self.reset(ResetReason::Reconnect, None).await;
                        self.needs_refix = false;
                        self.connect();
                    }
                }
                if let Some(view) = self.body_view.take() {
                    let client = self.client.clone();
                    let fixed = view.clone();
                    let history = self.history;
                    self.body = Some(BodyRead {
                        view,
                        task: tokio::spawn(async move {
                            let result = client.read_history(&fixed, history).await;
                            #[cfg(test)]
                            if let Some(gate) = &client.body_retirement
                                && gate.matches(&fixed)
                            {
                                gate.read_done.notify_one();
                            }
                            result
                        }),
                    });
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
                        if self.cancel_generation == 0 && self.submission.is_some() {
                            self.submission_deadline = Some(Instant::now() + Duration::from_secs(5));
                        }
                        self.cancel_generation = command.cancel;
                        self.report.cancel_error = None;
                        self.cancel_attempts = 0;
                        if !self.closed || self.report.cancel != Some(CancelStatus::Taken) {
                            self.cancel_status(CancelStatus::Pending, None);
                        }
                    }
                    if command.detach && self.detach_deadline.is_none() {
                        self.detach_deadline = Some(Instant::now() + Duration::from_secs(5));
                        self.retire_body().await;
                        self.pending.clear();
                    }
                }
                result = async { self.submission.as_mut().expect("submitting").await }, if self.submission.is_some() => {
                    self.submission = None;
                    self.submission_deadline = None;
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
                _ = async { tokio::time::sleep_until(self.submission_deadline.expect("waiting for receipt")).await },
                    if self.submission_deadline.is_some() && self.detach_deadline.is_none() => {
                    self.handoff_submission().await;
                }
                permit = updates.reserve(), if !self.pending.is_empty() => {
                    match permit {
                        Ok(permit) => permit.send(self.pending.pop_front().expect("queued update")),
                        Err(_) => break,
                    }
                }
                result = async { self.connection.as_mut().expect("connecting").await }, if self.connection.is_some() => {
                    self.connection = None;
                    match result.unwrap_or_else(|_| Err(worker_error())) {
                        Ok((selected, view, subscription)) => {
                            self.select(selected);
                            self.subscription = subscription;
                            if let Some(view) = view { self.fixed(Box::new(view), None, ObservationPhase::NoOwner, None).await; }
                        }
                        Err(error) => self.pending.push_back(LocalUpdate::Connection(ConnectionStatus::Error(error))),
                    }
                }
                observation = async { self.subscription.as_mut().expect("subscribed").next().await },
                    if self.subscription.is_some() => {
                    self.observed(observation).await;
                }
                result = async { (&mut self.body.as_mut().expect("reading").task).await }, if self.body.is_some() && self.pending.len() < 16 => {
                    let body = self.body.take().expect("reading");
                    let result = result.unwrap_or_else(|_| Err(HistoryReadError {
                        context: HistoryReadContext::Worker, error: worker_error(),
                    }));
                    self.report.history_error = result.as_ref().err().cloned();
                    self.pending.push_back(LocalUpdate::History { view: body.view.clone(), result: result.map(Box::new), stale: false });
                }
                _ = tokio::time::sleep_until(self.deadline), if self.subscription.is_some() || self.body.is_some() => {
                    if self.subscription.is_some() {
                        self.retire_observation().await;
                        if self.report.state.as_ref().is_none_or(|state| state.phase != ObservationPhase::Finished) {
                            self.pending.push_back(LocalUpdate::Connection(ConnectionStatus::Idle));
                        }
                    }
                    if let Some(mut body) = self.body.take() {
                        body.task.abort();
                        let result = (&mut body.task).await.unwrap_or_else(|error| Err(HistoryReadError {
                            context: HistoryReadContext::Worker,
                            error: if error.is_cancelled() { LocalError::new(LocalErrorKind::Transport, "fixed history read timed out") } else { worker_error() },
                        }));
                        self.report.history_error = result.as_ref().err().cloned();
                        self.pending.push_back(LocalUpdate::History { view: body.view.clone(), result: result.map(Box::new), stale: false });
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

fn worker_error() -> LocalError {
    LocalError::new(LocalErrorKind::Internal, "local worker failed")
}
