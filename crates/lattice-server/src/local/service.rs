// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 后台进程内服务接口、只读轮次观察句柄与状态感知停机通知。

use super::{
    server::{
        Backend, BackendOperation, CleanupGuard, ObserverLease, Owner, OwnerReply, SubmitOutcome,
        SubmitSource, internal, lock, not_found,
    },
    types::*,
};
use lattice_conversation::{MessageSemantic, NativeMessage, ToolCallState};
use lattice_ipc::{RecoveryNeed, TurnRef};
use lattice_session::store::{Node, NodeRef, SessionUsage, TurnDetail};
use std::sync::{Arc, atomic::Ordering};
use tokio::{net::UnixStream, sync::watch};

/// 进程内已接受轮次的终态结果快照。
#[derive(Clone, Debug, PartialEq)]
pub struct InProcessTurnOutcome {
    /// 会话库中该轮次的持久摘要、用量与步骤详情。
    pub detail: TurnDetail,
    /// 最后一步助手回复中的纯文本正文（不含推理内容与工具调用）。
    pub assistant_text: String,
    /// 本进程内存中记录的该轮执行期存储或内部诊断。
    pub diagnostic: Option<LocalError>,
}

/// 后台进入停机阶段的状态感知通知句柄。
#[derive(Clone, Debug)]
pub struct StopNotification {
    rx: watch::Receiver<bool>,
}

impl StopNotification {
    pub(super) fn new(rx: watch::Receiver<bool>) -> Self {
        Self { rx }
    }

    /// 返回后台当前是否已进入停机阶段。
    pub fn is_stopping(&self) -> bool {
        *self.rx.borrow()
    }

    /// 等待后台进入停机阶段；若订阅或调用时已在停机，或后台已释放，则立即返回。
    pub async fn wait(&mut self) {
        loop {
            if *self.rx.borrow_and_update() {
                return;
            }
            if self.rx.changed().await.is_err() {
                return;
            }
        }
    }
}

/// 绑定固定会话与轮次身份的进程内只读观察句柄。
///
/// 丢弃该句柄只释放会话观察租约，不会取消后台正在执行的轮次。
pub struct InProcessTurn {
    backend: Arc<Backend>,
    session_id: SessionId,
    turn_id: TurnId,
    observer: ObserverLease,
    changed: watch::Receiver<()>,
    done: Option<watch::Receiver<Option<Result<()>>>>,
    last_draft: String,
}

impl std::fmt::Debug for InProcessTurn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InProcessTurn")
            .field("session_id", &self.session_id)
            .field("turn_id", &self.turn_id)
            .field("finished", &self.is_finished())
            .finish_non_exhaustive()
    }
}

impl InProcessTurn {
    pub(super) fn from_owner(
        backend: Arc<Backend>,
        session_id: SessionId,
        turn_id: TurnId,
        owner: &Arc<Owner>,
    ) -> Self {
        let observer = backend.observer(session_id);
        let changed = observer.subscribe();
        let done = Some(owner.done.subscribe());
        Self {
            backend,
            session_id,
            turn_id,
            observer,
            changed,
            done,
            last_draft: String::new(),
        }
    }

    /// 返回本句柄绑定的会话身份。
    pub fn session_id(&self) -> SessionId {
        self.session_id
    }

    /// 返回本句柄绑定的轮次身份。
    pub fn turn_id(&self) -> TurnId {
        self.turn_id
    }

    /// 返回后台当前是否已完成本轮的接管收尾。
    pub fn is_finished(&self) -> bool {
        self.done
            .as_ref()
            .is_none_or(|done| done.borrow().is_some())
    }

    /// 读取本轮当前模型尝试的助手纯文本草稿快照（不含推理内容）。
    ///
    /// 当本轮尚未产出正文、因重试或步骤切换清空草稿，或已进入终态时返回空字符串。
    pub fn draft_text(&self) -> String {
        if self.is_finished() || self.backend.stores.is_stopped() {
            return String::new();
        }
        self.observer
            .assistant_draft(self.turn_id)
            .unwrap_or_default()
    }

    /// 等待并返回下一次助手纯文本草稿快照（重试或步骤切换清空草稿时返回空字符串）；
    /// 本轮收尾结束或会话库停写后返回 `None`。
    pub async fn next_draft(&mut self) -> Option<String> {
        let mut store_changed = self.backend.stores.subscribe();
        loop {
            self.changed.borrow_and_update();
            store_changed.borrow_and_update();
            if self.backend.stores.is_stopped() {
                return None;
            }
            let finished = self
                .done
                .as_mut()
                .is_none_or(|done| done.borrow_and_update().is_some());
            if finished {
                return None;
            }
            if let Some(current) = self.observer.assistant_draft(self.turn_id)
                && current != self.last_draft
            {
                self.last_draft = current.clone();
                return Some(current);
            }
            let done = self.done.as_mut().expect("未结束轮次必有完成通道");
            tokio::select! {
                res = self.changed.changed() => {
                    if res.is_err() {
                        return None;
                    }
                }
                res = done.changed() => {
                    if res.is_err() {
                        return None;
                    }
                }
                _ = store_changed.changed() => {}
            }
        }
    }

    /// 等待本轮接管收尾，并在同一读事务中读取固定轮次的终态详情与最后一步助手正文。
    pub async fn outcome(&mut self) -> Result<InProcessTurnOutcome> {
        if let Some(done) = &mut self.done {
            let mut store_changed = self.backend.stores.subscribe();
            loop {
                if done.borrow_and_update().is_some() {
                    break;
                }
                store_changed.borrow_and_update();
                if self.backend.stores.is_stopped() {
                    break;
                }
                tokio::select! {
                    res = done.changed() => {
                        res.map_err(|_| internal())?;
                    }
                    _ = store_changed.changed() => {}
                }
            }
        }
        let api = self.backend.optional_store().await?.ok_or_else(not_found)?;
        let session_id = self.session_id;
        let turn_id = self.turn_id;
        let (detail, assistant_text) = api
            .read(move |read| {
                let detail = read.turn_detail(session_id, turn_id)?;
                let assistant_text = match detail.steps.last() {
                    Some(last_step) => {
                        let node = read.node(NodeRef {
                            session_id,
                            node: last_step.node,
                        })?;
                        match node {
                            Node::Step {
                                reply: Some(message),
                                ..
                            } => extract_message_text(&message),
                            _ => String::new(),
                        }
                    }
                    None => String::new(),
                };
                Ok((detail, assistant_text))
            })
            .await?;
        let diagnostic = lock(&self.backend.admission)
            .diagnostics
            .get(&turn_id)
            .cloned();
        Ok(InProcessTurnOutcome {
            detail,
            assistant_text,
            diagnostic,
        })
    }
}

/// 后台进程内服务入口；与本地 IPC 分发共用同一套准入、执行与查询实现。
#[derive(Clone)]
pub struct Service {
    backend: Arc<Backend>,
}

impl std::fmt::Debug for Service {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Service").finish_non_exhaustive()
    }
}

impl Service {
    pub(super) fn new(backend: Arc<Backend>) -> Self {
        Self { backend }
    }

    pub(super) async fn configure_with_cleanup(
        &self,
        request: ConfigureRequest,
    ) -> (Result<ConfigureReply>, Option<CleanupGuard>) {
        let cleanup_after_reply = matches!(request.session, ConfigureSession::New(_));
        let result = match self.backend.claim(BackendOperation::Configure { request }) {
            Ok(owner) => match self.backend.wait_owner_reply(owner).await {
                Ok(OwnerReply::Configure(reply)) => Ok(reply),
                Ok(_) => Err(internal()),
                Err(error) => Err(error),
            },
            Err(error) => Err(error),
        };
        let guard = if cleanup_after_reply {
            self.backend.begin_cleanup()
        } else {
            None
        };
        (result, guard)
    }

    /// 创建新会话或更新已有会话设置；新建草稿会话结束时异步触发过期草稿清理。
    pub async fn configure(&self, request: ConfigureRequest) -> Result<ConfigureReply> {
        let (result, cleanup_guard) = self.configure_with_cleanup(request).await;
        if let Some(guard) = cleanup_guard {
            self.backend.spawn_cleanup_drafts(guard);
        }
        result
    }

    async fn submit_raw(
        &self,
        request: TurnRequest,
        source: SubmitSource,
    ) -> Result<(Arc<Owner>, SubmitOutcome)> {
        let owner = self
            .backend
            .claim(BackendOperation::Submit { request, source })?;
        match self.backend.wait_owner_reply(owner.clone()).await? {
            OwnerReply::Submit(outcome) => Ok((owner, outcome)),
            _ => Err(internal()),
        }
    }

    pub(super) async fn submit_ipc(
        &self,
        request: TurnRequest,
        submitter: UnixStream,
    ) -> Result<Acceptance> {
        let (_owner, outcome) = self
            .submit_raw(request, SubmitSource::Ipc(submitter))
            .await?;
        Ok(match outcome {
            SubmitOutcome::Accepted(turn) => Acceptance::Accepted {
                turn: TurnView::from(turn.as_ref()),
            },
            SubmitOutcome::ConfirmationUnknown {
                turn_id,
                session_id,
                ..
            } => Acceptance::ConfirmationUnknown {
                turn_id,
                session_id,
            },
        })
    }

    /// 向后台提交新轮次并返回只读观察句柄；若接受确认不明则返回带找回需求的错误。
    pub async fn submit(&self, request: TurnRequest) -> Result<InProcessTurn> {
        let session_id = request.session;
        let turn_id = request.turn_id;
        let (owner, outcome) = self.submit_raw(request, SubmitSource::InProcess).await?;
        match outcome {
            SubmitOutcome::Accepted(_) => Ok(InProcessTurn::from_owner(
                self.backend.clone(),
                session_id,
                turn_id,
                &owner,
            )),
            SubmitOutcome::ConfirmationUnknown {
                turn_id,
                session_id,
                cause,
            } => Err(confirmation_unknown_error(session_id, turn_id, cause)),
        }
    }

    /// 重新附着观察已持久接受的轮次（在途或已结束均可）。
    pub async fn observe(&self, session_id: SessionId, turn_id: TurnId) -> Result<InProcessTurn> {
        let api = self.backend.optional_store().await?.ok_or_else(not_found)?;
        let node = api.read(move |r| r.turn_node(turn_id)).await?;
        if node.is_none_or(|node| node.session_id != session_id) {
            return Err(not_found());
        }
        let observer = self.backend.observer(session_id);
        let changed = observer.subscribe();
        let done = lock(&self.backend.admission)
            .turns
            .get(&turn_id)
            .filter(|owner| {
                owner.session == Some(session_id) && owner.started.load(Ordering::Acquire)
            })
            .map(|owner| owner.done.subscribe());
        Ok(InProcessTurn {
            backend: self.backend.clone(),
            session_id,
            turn_id,
            observer,
            changed,
            done,
            last_draft: String::new(),
        })
    }

    /// 请求取消指定轮次；当前由本进程接管时置位取消标志并返回 `true`，未接管时核对持久库后返回 `false`。
    pub async fn cancel(&self, turn_id: TurnId) -> Result<bool> {
        let owned = {
            let state = lock(&self.backend.admission);
            state.turns.get(&turn_id).is_some_and(|owner| {
                owner.execution_cancel.store(true, Ordering::Release);
                true
            })
        };
        if !owned && let Some(api) = self.backend.optional_store().await? {
            api.read(move |r| r.turn_node(turn_id)).await?;
        }
        Ok(owned)
    }

    /// 查询指定会话的会话头快照。
    pub async fn head(&self, session_id: SessionId) -> Result<SessionHead> {
        self.backend.session_head(session_id).await
    }

    /// 按轮次身份查询接受状态与详情。
    pub async fn turn(&self, turn_id: TurnId) -> Result<TurnLookup> {
        self.backend.lookup(turn_id).await
    }

    pub(super) async fn usage_by_prefix(&self, session_id: String) -> Result<SessionUsage> {
        Ok(self.backend.open().await?.usage(session_id).await?)
    }

    /// 查询指定会话的当前路径与全会话用量汇总。
    pub async fn usage(&self, session_id: SessionId) -> Result<SessionUsage> {
        self.usage_by_prefix(session_id.to_string()).await
    }

    /// 返回状态感知的后台停机通知句柄。
    pub fn stopping(&self) -> StopNotification {
        StopNotification::new(self.backend.stopping.subscribe())
    }

    /// 返回后台启动时经筛选保留的可用 TLS 信任根 DER 切片。
    pub fn tls_roots_der(&self) -> &[Vec<u8>] {
        self.backend.tls_roots_der()
    }

    /// 供集成测试在指定会话下一轮进入接受事务前挂起（此时 `turn()` 返回 `TurnLookup::InFlight`）。
    #[cfg(any(test, feature = "test-utils"))]
    pub fn arm_before_accept_for_test(
        &self,
        session_id: SessionId,
    ) -> (
        tokio::sync::oneshot::Receiver<()>,
        tokio::sync::oneshot::Sender<()>,
    ) {
        let lease = self.backend.observer(session_id);
        let (raw_entered_tx, raw_entered_rx) = tokio::sync::oneshot::channel();
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        *lease
            .before_accept
            .lock()
            .expect("测试接受前闸门锁不应中毒") = Some((raw_entered_tx, release_rx));
        tokio::spawn(async move {
            let _keep_lease = lease;
            if raw_entered_rx.await.is_ok() {
                let _ = entered_tx.send(());
            }
        });
        (entered_rx, release_tx)
    }

    /// 供集成测试在指定会话下一轮接受事务落盘后注入读回失败，使 `submit` 返回带 `recovery` 的确认不明错误。
    #[cfg(any(test, feature = "test-utils"))]
    pub fn arm_accepted_read_failure_for_test(
        &self,
        session_id: SessionId,
    ) -> (
        tokio::sync::oneshot::Receiver<()>,
        tokio::sync::oneshot::Sender<()>,
    ) {
        let lease = self.backend.observer(session_id);
        let (raw_entered_tx, raw_entered_rx) = tokio::sync::oneshot::channel();
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        *lease
            .accepted_read_failure
            .lock()
            .expect("测试接受后读回闸门锁不应中毒") = Some((raw_entered_tx, release_rx));
        tokio::spawn(async move {
            let _keep_lease = lease;
            if raw_entered_rx.await.is_ok() {
                let _ = entered_tx.send(());
            }
        });
        (entered_rx, release_tx)
    }
}

fn extract_message_text<S: ToolCallState>(message: &NativeMessage<S>) -> String {
    let mut out = String::new();
    for part in &message.parts {
        if let MessageSemantic::Text { text } = &part.semantic {
            out.push_str(text);
        }
    }
    out
}

fn confirmation_unknown_error(
    session_id: SessionId,
    turn_id: TurnId,
    cause: RecoveryCause,
) -> LocalError {
    let (kind, message) = match cause {
        RecoveryCause::WorkerFailed => (
            LocalErrorKind::Internal,
            "后台执行工作任务异常中断，轮次接受结果未确认，请按轮次身份查询找回",
        ),
        RecoveryCause::WriteDisabled => (
            LocalErrorKind::StoreUnavailable,
            "会话库已停写，轮次接受结果未确认，请按轮次身份查询找回",
        ),
        RecoveryCause::CommitFailed => (
            LocalErrorKind::StoreUnavailable,
            "会话库提交或读回未确认，轮次接受结果未确认，请按轮次身份查询找回",
        ),
        RecoveryCause::ReopenFailed => (
            LocalErrorKind::StoreUnavailable,
            "会话库重开失败，轮次接受结果未确认，请按轮次身份查询找回",
        ),
    };
    LocalError {
        recovery: Some(RecoveryNeed {
            turn: Some(TurnRef {
                session_id,
                turn_id,
            }),
            cause,
        }),
        ..LocalError::new(kind, message)
    }
}
