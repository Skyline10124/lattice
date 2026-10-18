// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use super::{FrozenConfiguration, LocalClient, LocalWatch, client::SubmissionReply, types::*};
use std::{
    collections::{BTreeSet, VecDeque},
    time::Duration,
};
use tokio::{
    sync::{mpsc, oneshot, watch},
    task::JoinHandle,
    time::Instant,
};

#[derive(Clone, Copy, Default)]
struct Commands {
    cancel: u64,
    reconnect: u64,
    detach: bool,
    stop: bool,
}
/// 提交与取消独立于观察队列推进，退出只交接已经登记的意图。
pub struct LocalTurn {
    commands: watch::Sender<Commands>,
    external_view: Option<mpsc::Sender<(ViewUpdate, oneshot::Sender<()>)>>,
    updates: mpsc::Receiver<LocalUpdate>,
    session_id: SessionId,
    turn_id: Option<TurnId>,
    task: Option<JoinHandle<DetachReport>>,
}
impl LocalClient {
    /// 只处理提交与控制回执；前端用 apply_view 交付已有 watch 的消息。
    /// Unknown 之后的确定结果会再次交付回执，供前端开始观察或显示拒绝。
    pub fn start_submission(
        &self,
        request: TurnRequest,
        configuration: Result<FrozenConfiguration>,
    ) -> LocalTurn {
        self.submission(request, configuration, false)
    }
    pub fn start_turn(
        &self,
        request: TurnRequest,
        configuration: Result<FrozenConfiguration>,
    ) -> LocalTurn {
        self.submission(request, configuration, true)
    }
    fn submission(
        &self,
        request: TurnRequest,
        configuration: Result<FrozenConfiguration>,
        own_view: bool,
    ) -> LocalTurn {
        let (mut handle, mut worker) = LocalTurn::prepare(
            self.clone(),
            request.session.id(),
            Some(request.turn_id),
            true,
        );
        worker.own_view = own_view;
        worker.awaiting_user_message = true;
        if !own_view {
            let (sender, receiver) = mpsc::channel(16);
            handle.external_view = Some(sender);
            worker.external_view = Some(receiver);
        }
        worker.pending.push_back(LocalUpdate::Submit {
            update: SubmissionUpdate::Sending {
                turn_id: request.turn_id,
                session_id: request.session.id(),
                new: request.session.is_new(),
            },
        });
        worker.input = Some((request, configuration));
        handle.task = Some(tokio::spawn(worker.run()));
        handle
    }
    pub fn observe(&self, session_id: SessionId) -> LocalTurn {
        let (mut handle, worker) = LocalTurn::prepare(self.clone(), session_id, None, false);
        handle.task = Some(tokio::spawn(worker.run()));
        handle
    }
    pub fn follow_turn(&self, turn: TurnDetail) -> LocalTurn {
        let (mut handle, mut worker) =
            LocalTurn::prepare(self.clone(), turn.session_id, Some(turn.turn_id), true);
        worker.report.summary = Some(turn.summary);
        handle.task = Some(tokio::spawn(worker.run()));
        handle
    }
}
impl LocalTurn {
    /// 按原顺序交付本会话的 head、entry、text，供取消与结束判定；不再转发。
    /// 自带 watch 的句柄忽略此输入；句柄结束后调用无效果。
    pub async fn apply_view(&self, update: ViewUpdate) {
        if let Some(sender) = &self.external_view {
            let (applied, done) = oneshot::channel();
            if sender.send((update, applied)).await.is_ok() {
                let _ = done.await;
            }
        }
    }
    fn prepare(
        client: LocalClient,
        session_id: SessionId,
        turn_id: Option<TurnId>,
        following: bool,
    ) -> (Self, Worker) {
        let (commands, receiver) = watch::channel(Commands::default());
        let (sender, updates) = mpsc::channel(16);
        let worker = Worker::new(client, session_id, turn_id, following, receiver, sender);
        (
            Self {
                commands,
                external_view: None,
                updates,
                session_id,
                turn_id,
                task: None,
            },
            worker,
        )
    }
    pub fn session_id(&self) -> SessionId {
        self.session_id
    }
    pub fn turn_id(&self) -> Option<TurnId> {
        self.turn_id
    }
    pub fn cancel(&self) {
        self.commands.send_modify(|c| c.cancel += 1);
    }
    pub fn reconnect(&self) {
        self.commands.send_modify(|c| c.reconnect += 1);
    }
    pub async fn next(&mut self) -> Option<LocalUpdate> {
        self.updates.recv().await
    }
    pub async fn detach(mut self) -> DetachReport {
        self.commands.send_modify(|c| c.detach = true);
        self.task
            .take()
            .expect("local worker")
            .await
            .expect("local worker panicked")
    }
}
impl Drop for LocalTurn {
    fn drop(&mut self) {
        self.commands.send_modify(|c| c.stop = true);
    }
}

struct Worker {
    client: LocalClient,
    commands: watch::Receiver<Commands>,
    updates: mpsc::Sender<LocalUpdate>,
    pending: VecDeque<LocalUpdate>,
    report: DetachReport,
    following: bool,
    own_view: bool,
    external_view: Option<mpsc::Receiver<(ViewUpdate, oneshot::Sender<()>)>>,
    input: Option<(TurnRequest, Result<FrozenConfiguration>)>,
    submission: Option<JoinHandle<SubmissionReply>>,
    recovery: Option<JoinHandle<Result<TurnDetail>>>,
    connection: Option<JoinHandle<Result<LocalWatch>>>,
    view: Option<LocalWatch>,
    progress: Option<watch::Receiver<Instant>>,
    nodes: BTreeSet<NodeId>,
    awaiting_user_message: bool,
    head_after_user_message: bool,
    seen_position: bool,
    observed_end: bool,
    finished: bool,
    live_evidence: bool,
    cancel_generation: u64,
    cancel_attempts: u8,
    cancel_task: Option<(u64, JoinHandle<Result<bool>>)>,
    reconnect_generation: u64,
    submission_deadline: Option<Instant>,
    handoff: Option<u64>,
    detach_deadline: Option<Instant>,
    idle_deadline: Instant,
}
impl Worker {
    fn new(
        client: LocalClient,
        session_id: SessionId,
        turn_id: Option<TurnId>,
        following: bool,
        commands: watch::Receiver<Commands>,
        updates: mpsc::Sender<LocalUpdate>,
    ) -> Self {
        Self {
            client,
            commands,
            updates,
            following,
            own_view: true,
            external_view: None,
            pending: VecDeque::new(),
            report: DetachReport {
                session_id,
                turn_id,
                submission: None,
                submission_diagnostic: None,
                cancel: None,
                cancel_error: None,
                head: None,
                summary: None,
            },
            input: None,
            submission: None,
            recovery: None,
            connection: None,
            view: None,
            progress: None,
            nodes: BTreeSet::new(),
            awaiting_user_message: false,
            head_after_user_message: false,
            seen_position: false,
            observed_end: false,
            finished: false,
            live_evidence: false,
            cancel_generation: 0,
            cancel_attempts: 0,
            cancel_task: None,
            reconnect_generation: 0,
            submission_deadline: None,
            handoff: None,
            detach_deadline: None,
            idle_deadline: Instant::now() + Duration::from_secs(crate::STREAM_IDLE_TIMEOUT_SECS),
        }
    }
    fn receipt(&mut self, result: SubmissionResult, recovered: bool) {
        let first_receipt = self.report.submission.is_none();
        let late_resolution =
            !self.own_view
                && self.report.submission.as_ref().is_some_and(|receipt| {
                    matches!(receipt.result, SubmissionResult::Unknown { .. })
                })
                && !matches!(result, SubmissionResult::Unknown { .. });
        let receipt = SubmissionReceipt {
            turn_id: self.report.turn_id.expect("submission identity"),
            result,
            recovered,
        };
        self.report.submission = Some(receipt.clone());
        // 自带视图的 CLI 仍只交付一条回执，迟到接受由后续视图推进。
        if first_receipt || late_resolution {
            self.pending.push_back(LocalUpdate::Submit {
                update: SubmissionUpdate::Receipt(receipt),
            });
        }
    }
    fn connect(&mut self) {
        self.finished = false;
        if !self.own_view {
            self.finished = self.observed_end;
            return;
        }
        self.observed_end = false;
        self.view = None;
        self.progress = None;
        self.seen_position = false;
        self.nodes.clear();
        let client = self.client.clone();
        let sid = self.report.session_id;
        self.connection = Some(tokio::spawn(async move { client.watch(sid).await }));
        self.idle_deadline = Instant::now() + Duration::from_secs(crate::STREAM_IDLE_TIMEOUT_SECS);
    }
    fn recover(&mut self) {
        let client = self.client.clone();
        let turn_id = self.report.turn_id.expect("submission identity");
        self.recovery = Some(tokio::spawn(
            async move { client.wait_for_turn(turn_id).await },
        ));
    }
    fn submitted(&mut self, reply: SubmissionReply) {
        match reply {
            SubmissionReply::Accepted(Acceptance::Accepted { turn }) => {
                self.accepted(turn, false);
            }
            SubmissionReply::Rejected(error) => {
                self.receipt(SubmissionResult::Rejected { error }, false);
                self.finished = true;
            }
            SubmissionReply::Accepted(Acceptance::ConfirmationUnknown { .. }) => self.recover(),
            SubmissionReply::Disconnected(error) => {
                self.report.submission_diagnostic = Some(error);
                self.recover();
            }
        }
    }
    fn accepted(&mut self, turn: TurnView, recovered: bool) {
        // 外部视图可能先于接受回执确认结局，不能用接受时的旧投影覆盖它。
        if self.own_view || !self.observed_end {
            self.report.summary = Some(turn.summary.clone());
        }
        self.receipt(SubmissionResult::Accepted { turn }, recovered);
        self.connect();
    }
    fn cancellation(&mut self, status: CancelStatus, error: Option<LocalError>) {
        self.report.cancel = Some(status);
        if error.is_some() || matches!(status, CancelStatus::Taken | CancelStatus::NotTaken) {
            self.report.cancel_error = error;
        }
        if matches!(
            status,
            CancelStatus::Taken | CancelStatus::NotTaken | CancelStatus::Unknown
        ) && let Some(turn_id) = self.report.turn_id
        {
            self.pending.push_back(LocalUpdate::Cancel {
                turn_id,
                status,
                owned: match status {
                    CancelStatus::Taken => Some(true),
                    CancelStatus::NotTaken => Some(false),
                    _ => None,
                },
                error: self.report.cancel_error.clone(),
            });
        }
    }
    fn drive_cancel(&mut self) {
        if self.cancel_generation == 0 || self.cancel_task.is_some() {
            return;
        }
        if self.finished {
            if self.report.cancel != Some(CancelStatus::Taken) {
                self.report.cancel = Some(CancelStatus::Moot);
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
        self.report.cancel = Some(CancelStatus::Pending);
        self.cancel_task = Some((
            self.cancel_generation,
            tokio::spawn(async move {
                tokio::time::timeout(Duration::from_secs(5), client.cancel(turn_id))
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
    fn observed(&mut self, update: ViewUpdate, forward: bool) {
        self.idle_deadline = Instant::now() + Duration::from_secs(crate::STREAM_IDLE_TIMEOUT_SECS);
        let mut ended = false;
        let include = match &update {
            ViewUpdate::Head(head) => {
                self.head_after_user_message = !self.awaiting_user_message;
                // 同一位置也可能已经提交了新值，旧批次的条目不能确认这条头。
                self.seen_position = head.position == NodeId(0);
                if head.path_replaced {
                    self.nodes.clear();
                }
                self.live_evidence |= head.owner && head.turn_id == self.report.turn_id;
                self.report.head = Some(head.clone());
                true
            }
            ViewUpdate::Entry { entry } => {
                let content = match entry.as_ref() {
                    ConversationEntry::UserMessage { content, .. }
                    | ConversationEntry::Step { content, .. } => content,
                };
                let selected = content.turn_id() == self.report.turn_id;
                if selected {
                    if matches!(entry.as_ref(), ConversationEntry::UserMessage { .. }) {
                        self.awaiting_user_message = false;
                    }
                    self.nodes.insert(entry.node());
                    if let ConversationEntry::Step {
                        summary: Some(summary),
                        ..
                    } = entry.as_ref()
                    {
                        ended = summary.progress.is_closed();
                        self.report.summary = Some(summary.clone());
                    }
                }
                if self
                    .report
                    .head
                    .as_ref()
                    .is_some_and(|h| h.position == entry.node())
                {
                    self.seen_position = true;
                }
                !self.following || selected
            }
            ViewUpdate::Text(text) => !self.following || self.nodes.contains(&text.node),
        };
        if include && forward {
            self.pending.push_back(update.into());
        }
        // 首条 head 先于正文到达；无 Owner 也必须先读完它指向的条目。
        let no_owner = self.seen_position
            && self
                .report
                .head
                .as_ref()
                .is_some_and(|h| !h.owner || h.turn_id != self.report.turn_id);
        let resolved = self.submission.is_none() && self.recovery.is_none();
        let selected_head = self
            .report
            .head
            .as_ref()
            .is_some_and(|h| h.turn_id == self.report.turn_id);
        // 提交回执与 watch 没有跨连接顺序；只有本轮 UserMessage 能排除积压的旧头。
        // 本轮的头本身已经属于接受后的视图，其余头必须晚于这条 UserMessage。
        let current_view =
            !self.awaiting_user_message && (self.head_after_user_message || selected_head);
        if ended || (no_owner && current_view && (resolved || selected_head)) {
            self.observed_end = true;
        }
        if self.following && resolved && self.observed_end {
            self.finished = true;
            self.view = None;
        }
    }
    async fn run(mut self) -> DetachReport {
        if let Some((request, configuration)) = self.input.take() {
            if self.commands.borrow().cancel > 0 || self.commands.borrow().stop {
                self.receipt(SubmissionResult::NotAccepted, false);
                self.finished = true;
            } else {
                match configuration {
                    Ok(configuration) => {
                        let client = self.client.clone();
                        self.submission = Some(tokio::spawn(async move {
                            client.submit_once(request, configuration).await
                        }));
                    }
                    Err(error) => {
                        self.receipt(SubmissionResult::Rejected { error }, false);
                        self.finished = true;
                    }
                }
            }
        } else {
            self.connect();
        }
        let updates = self.updates.clone();
        loop {
            self.drive_cancel();
            if self.following
                && self.finished
                && self.pending.is_empty()
                && self.submission.is_none()
                && self.recovery.is_none()
                && self.cancel_task.is_none()
            {
                break;
            }
            if self.detach_deadline.is_some()
                && (self.cancel_generation == 0
                    || self.report.cancel == Some(CancelStatus::Taken)
                    || (self.cancel_task.is_none()
                        && (self.finished
                            || self.cancel_attempts == 2
                            || self.handoff == Some(self.cancel_generation))))
            {
                break;
            }
            tokio::select! {
                biased;
                changed = self.commands.changed() => {
                    if changed.is_err() { break }
                    let command = *self.commands.borrow_and_update();
                    if command.stop { break }
                    if command.cancel != self.cancel_generation {
                        if self.cancel_generation == 0 && self.submission.is_some() { self.submission_deadline = Some(Instant::now() + Duration::from_secs(5)); }
                        self.cancel_generation = command.cancel; self.cancel_attempts = 0; self.report.cancel_error = None;
                        self.report.cancel = Some(CancelStatus::Pending);
                    }
                    if command.reconnect != self.reconnect_generation {
                        self.reconnect_generation = command.reconnect; self.finished = false;
                        if let Some(task) = self.connection.take() { task.abort(); }
                        self.connect();
                    }
                    if command.detach && self.detach_deadline.is_none() { self.detach_deadline = Some(Instant::now() + Duration::from_secs(5)); }
                }
                result = async { self.submission.as_mut().expect("submitting").await }, if self.submission.is_some() => {
                    self.submission = None; self.submission_deadline = None;
                    self.submitted(result.unwrap_or_else(|_| SubmissionReply::Disconnected(worker_error())));
                }
                result = async { self.recovery.as_mut().expect("recovering").await }, if self.recovery.is_some() => {
                    self.recovery = None;
                    match result.unwrap_or_else(|_| Err(worker_error())) {
                        Ok(turn) => {
                            self.accepted(TurnView::from(&turn), true);
                        }
                        Err(error) if error.kind == LocalErrorKind::NotFound => { self.receipt(SubmissionResult::NotAccepted, true); self.finished = true; }
                        Err(error) => { self.receipt(SubmissionResult::Unknown { error: Some(error) }, true); self.finished = true; }
                    }
                }
                result = async { (&mut self.cancel_task.as_mut().expect("cancelling").1).await }, if self.cancel_task.is_some() => {
                    let (generation, _) = self.cancel_task.take().expect("cancelling");
                    if generation == self.cancel_generation { match result.unwrap_or_else(|_| Err(worker_error())) {
                        Ok(true) => self.cancellation(CancelStatus::Taken, None),
                        Ok(false) => self.cancellation(CancelStatus::NotTaken, None),
                        Err(error) => self.cancellation(CancelStatus::Unknown, Some(error)),
                    }}
                }
                _ = async { tokio::time::sleep_until(self.submission_deadline.expect("submission deadline")).await }, if self.submission_deadline.is_some() => {
                    self.submission_deadline = None; self.handoff = Some(self.cancel_generation);
                    self.receipt(SubmissionResult::Unknown { error: None }, false);
                    // 原提交连接仍在，只开始视图；绝不以查询或重发代替这条连接的回执。
                    self.connect();
                }
                _ = async { tokio::time::sleep_until(self.detach_deadline.expect("detach deadline")).await }, if self.detach_deadline.is_some() => {
                    if self.cancel_task.is_some() { self.cancellation(CancelStatus::Unknown, None); }
                    break
                }
                permit = updates.reserve(), if !self.pending.is_empty() && self.detach_deadline.is_none() => {
                    match permit { Ok(permit) => permit.send(self.pending.pop_front().expect("queued")), Err(_) => break }
                }
                result = async { self.connection.as_mut().expect("connecting").await }, if self.connection.is_some() => {
                    self.connection = None;
                    match result.unwrap_or_else(|_| Err(worker_error())) {
                        Ok(view) => { self.progress = Some(view.progress.clone()); self.view = Some(view); },
                        Err(error) => { self.pending.push_back(LocalUpdate::Connection { status: ConnectionStatus::Error(error) }); self.finished = true; }
                    }
                }
                item = async { self.view.as_mut().expect("watching").next().await }, if self.view.is_some() && self.pending.len() < 32 => {
                    match item {
                        Ok(Some(update)) => self.observed(update, true),
                        Ok(None) => { self.view = None; self.pending.push_back(LocalUpdate::Connection { status: ConnectionStatus::Eof }); self.finished = true; }
                        Err(error) => {
                            self.view = None;
                            if !(self.submission.is_some() && (error.kind == LocalErrorKind::NotFound || matches!(error.store.as_deref(), Some(StoreDiagnostic::NotInitialized { .. })))) {
                                self.pending.push_back(if matches!(error.kind, LocalErrorKind::Transport | LocalErrorKind::Protocol | LocalErrorKind::NotRunning) {
                                    LocalUpdate::Connection { status: ConnectionStatus::Error(error) }
                                } else { LocalUpdate::Error { error } });
                                self.finished = true;
                            }
                        }
                    }
                }
                item = async { self.external_view.as_mut().expect("external view").recv().await }, if self.external_view.is_some() => {
                    if let Some((update, applied)) = item { self.observed(update, false); let _ = applied.send(()); }
                    else { self.external_view = None; }
                }
                progress = async { self.progress.as_mut().expect("watching progress").changed().await }, if self.progress.is_some() && self.view.is_some() => {
                    if progress.is_err() { self.progress = None; }
                    if progress.is_ok() { self.idle_deadline = Instant::now() + Duration::from_secs(crate::STREAM_IDLE_TIMEOUT_SECS); }
                }
                _ = tokio::time::sleep_until(self.idle_deadline), if self.view.is_some() && self.report.head.as_ref().is_none_or(|h| h.owner) => {
                    self.view = None; self.pending.push_back(LocalUpdate::Connection { status: ConnectionStatus::Idle }); self.finished = true;
                }
            }
        }
        if self.input.is_some() || self.submission.is_some() || self.recovery.is_some() {
            self.receipt(SubmissionResult::Unknown { error: None }, false);
        }
        if let Some(task) = self.submission.take() {
            task.abort();
            let _ = task.await;
        }
        if let Some(task) = self.recovery.take() {
            task.abort();
            let _ = task.await;
        }
        if let Some(task) = self.connection.take() {
            task.abort();
            let _ = task.await;
        }
        if let Some((_, task)) = self.cancel_task.take() {
            task.abort();
            let _ = task.await;
        }
        self.report
    }
}
fn worker_error() -> LocalError {
    LocalError::new(LocalErrorKind::Internal, "local worker failed")
}
