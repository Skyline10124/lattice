// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! IM 网关运行循环、单对话 FIFO 准入串行化、逐条并行崩溃恢复与有界优雅收尾。

use std::{
    any::Any,
    collections::{BTreeMap, BTreeSet, VecDeque},
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex as StdMutex},
    time::Duration,
};

use chrono::Utc;
use lattice_ipc::{
    ConfigureRequest, ConfigureSession, ConflictReason, LocalError, LocalErrorKind, SessionId,
    TurnId, TurnLookup, TurnRequest, UserInputPart,
};
use lattice_server::{InProcessTurn, Service};
use lattice_store::StateLayout;
use tokio::{
    sync::{mpsc, watch},
    task::JoinSet,
};

use crate::{
    album::{AlbumCollector, BufferedAlbumItem, ClosedAlbum, LATE_ALBUM_FRAGMENT_REPLY},
    command::{
        HELP_REPLY, ImCommand, ParsedInput, UNSUPPORTED_MEDIA_REPLY, cancel_turn_and_format,
        describe_local_error, execute_cancel_command, execute_status_command,
        execute_usage_command, is_authorized_private_message, is_not_found_or_uninitialized,
        parse_text_input,
    },
    config::TelegramGatewayConfig,
    identity::{ChatKey, InboundId, new_session_id, new_turn_id},
    ledger::{InboundRecord, Ledger, LedgerError, LedgerPublicationGuard, LedgerPublicationLatch},
    media::{IncomingMediaItem, MediaBatchOutcome, prepare_media_turn},
    redact::ImRedactor,
    reply::{
        ActiveDrafts, ChatDraftThrottleMap, DraftLease, compose_final_reply, drive_turn_drafts,
        send_text_reliably, wait_stop_signal,
    },
    telegram::{
        LONG_POLL_TIMEOUT_SECS, TELEGRAM_REDELIVERY_WINDOW, TelegramApiError, TelegramIo,
        TelegramMessageContent, TelegramUpdate, TelegramUpdatePayload,
    },
    telegram_http::TelegramHttpClient,
};

const NEW_RESET_REPLY: &str = "已清除当前会话绑定，下一条消息将新建会话";
const NOT_EXECUTED_REPLY: &str = "该消息未被执行";

const INITIAL_POLL_BACKOFF: Duration = Duration::from_millis(200);
const MAX_POLL_BACKOFF: Duration = Duration::from_secs(30);
const INITIAL_RECOVERY_BACKOFF: Duration = Duration::from_millis(50);
const MAX_RECOVERY_BACKOFF: Duration = Duration::from_secs(5);
/// 存在未确认开放相册，或原始长轮询批次非空但全部更新被过滤为零新更新时的空轮询节流上限。
const IDLE_POLL_THROTTLE: Duration = Duration::from_millis(100);

/// IM 网关启动或不可恢复台账错误。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ImGatewayError {
    /// 启动期打开或清理 `<state-root>/lattice/im/ledger` 失败（如文件损坏或权限不合规）。
    #[error("{0}")]
    Ledger(String),
}

impl From<LedgerError> for ImGatewayError {
    fn from(error: LedgerError) -> Self {
        Self::Ledger(error.to_string())
    }
}

/// 经只读配置与凭据检查通过、待启动的 IM 网关实例。
pub struct PreparedImGateway {
    pub(crate) config: TelegramGatewayConfig,
    layout: StateLayout,
    service: Service,
}

impl std::fmt::Debug for PreparedImGateway {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PreparedImGateway")
            .field("config", &self.config)
            .field("lattice_root", &self.layout.lattice_root())
            .finish_non_exhaustive()
    }
}

impl PreparedImGateway {
    pub(crate) fn new(
        config: TelegramGatewayConfig,
        layout: StateLayout,
        service: Service,
    ) -> Self {
        Self {
            config,
            layout,
            service,
        }
    }

    /// 启动真实 Telegram HTTPS 轮询与消息处理循环，直至 `stop` 完成或后台进入停机阶段，
    /// 并等待本实例派生的全部子任务（含台账写入与阻塞地址解析）完整收尾后返回。
    pub async fn run(self, stop: impl Future<Output = ()> + Send) -> Result<(), ImGatewayError> {
        let redactor = ImRedactor::new([self.config.token.clone()]);
        let tls_roots = self.service.tls_roots_der().to_vec();
        let io = Arc::new(TelegramHttpClient::new(
            self.config.token.clone(),
            tls_roots,
            redactor,
        ));
        self.run_with_io(io, stop).await
    }

    pub(crate) async fn run_with_io(
        self,
        io: Arc<dyn TelegramIo>,
        stop: impl Future<Output = ()> + Send,
    ) -> Result<(), ImGatewayError> {
        lattice_server::scoped_component_future(self.run_inner(io, stop, None)).await
    }

    #[cfg(test)]
    pub(crate) async fn run_with_io_and_probe(
        self,
        io: Arc<dyn TelegramIo>,
        stop: impl Future<Output = ()> + Send,
        probe_out: tokio::sync::oneshot::Sender<GatewayDebugProbe>,
    ) -> Result<(), ImGatewayError> {
        lattice_server::scoped_component_future(self.run_inner(io, stop, Some(probe_out))).await
    }

    async fn run_inner(
        self,
        io: Arc<dyn TelegramIo>,
        stop: impl Future<Output = ()> + Send,
        #[cfg_attr(not(test), allow(unused_variables))] probe_out: Option<
            tokio::sync::oneshot::Sender<GatewayDebugProbe>,
        >,
    ) -> Result<(), ImGatewayError> {
        // 0. 绑定当前实例生命周期的台账发布门闩：即使本 future 被宿主强制 abort 丢弃，也会立即封闭滞留的阻塞写入。
        let publication_latch = LedgerPublicationLatch::new();
        let _publication_guard = LedgerPublicationGuard::new(publication_latch.clone());

        // 1. 打开台账并执行滑动窗口清理（仅删除早于 24h+5m 的 Delivered 条目）。
        let ledger = Ledger::open_and_prune_with_latch(
            self.layout.clone(),
            Utc::now(),
            TELEGRAM_REDELIVERY_WINDOW,
            publication_latch,
        )
        .await?;

        let redactor = ImRedactor::new([self.config.token.clone()]);
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let (spawn_tx, mut spawn_rx) = mpsc::unbounded_channel::<BoxTask>();
        let io_for_drain = Arc::clone(&io);
        let in_flight_inbound = Arc::new(StdMutex::new(BTreeSet::new()));
        let chat_queues = Arc::new(ChatAdmissionQueues::default());

        #[cfg(test)]
        let worker_ever_polled = Arc::new(std::sync::atomic::AtomicBool::new(false));
        #[cfg(test)]
        let hold_spawn_rx = Arc::new(std::sync::atomic::AtomicBool::new(false));
        #[cfg(test)]
        let mut held_unpolled_tasks: Vec<BoxTask> = Vec::new();

        let state = Arc::new(GatewayRuntime {
            config: self.config,
            service: self.service.clone(),
            ledger,
            io,
            redactor,
            in_flight_inbound: Arc::clone(&in_flight_inbound),
            chat_queues: Arc::clone(&chat_queues),
            active_drafts: Arc::new(ActiveDrafts::default()),
            draft_throttles: ChatDraftThrottleMap::default(),
            shutdown_rx: shutdown_rx.clone(),
            spawn_tx,
            #[cfg(test)]
            worker_ever_polled: Arc::clone(&worker_ever_polled),
        });

        #[cfg(test)]
        if let Some(tx) = probe_out {
            let _ = tx.send(GatewayDebugProbe {
                runtime_weak: Arc::downgrade(&state),
                in_flight_inbound,
                chat_queues,
                worker_ever_polled,
                hold_spawn_rx: Arc::clone(&hold_spawn_rx),
            });
        }

        let mut join_set = JoinSet::new();

        // 2. 在开始轮询之前，同步为每条遗留 Registered 条目登记对应对话的启动期准入屏障计数，
        //    并为每条遗留 Registered 与 Accepted 各自派生独立的并行恢复任务。
        let pending = state.ledger.pending_records().await;
        for (inbound_id, record) in pending {
            match record {
                InboundRecord::Registered {
                    chat,
                    session,
                    turn,
                } => {
                    let barrier_guard = state.chat_queues.register_startup_barrier(&chat);
                    let rt = Arc::clone(&state);
                    join_set.spawn(lattice_server::scoped_component_future(async move {
                        let mut maybe_guard = Some(barrier_guard);
                        rt.resolve_registered_until_settled(
                            &mut maybe_guard,
                            inbound_id,
                            chat,
                            session,
                            turn,
                        )
                        .await;
                    }));
                }
                InboundRecord::Accepted {
                    chat,
                    session,
                    turn,
                } => {
                    let rt = Arc::clone(&state);
                    join_set.spawn(lattice_server::scoped_component_future(async move {
                        rt.recover_accepted_turn(inbound_id, chat, session, turn)
                            .await;
                    }));
                }
                InboundRecord::Merged { .. } => {}
                InboundRecord::DeliveredTurn { .. } | InboundRecord::DeliveredReset { .. } => {}
            }
        }

        // 3. 不等待恢复任务结束，立即启动 getUpdates 长轮询主循环并纳入监督。
        let poll_rt = Arc::clone(&state);
        let mut poll_task = tokio::spawn(lattice_server::scoped_component_future(async move {
            poll_rt.poll_updates_loop().await;
        }));
        let mut poll_abort_guard = AbortTaskOnDrop(Some(poll_task.abort_handle()));
        let mut poll_done = false;
        let mut first_panic: Option<Box<dyn Any + Send + 'static>> = None;

        let mut backend_stopping = self.service.stopping();
        tokio::pin!(stop);

        // 主监督循环：持续收割子任务与动态派生请求；任一子任务或轮询任务异常退出时立即广播停机并收尾后传播。
        loop {
            tokio::select! {
                _ = &mut stop => {
                    let _ = shutdown_tx.send(true);
                    break;
                }
                _ = backend_stopping.wait() => {
                    let _ = shutdown_tx.send(true);
                    break;
                }
                poll_res = &mut poll_task, if !poll_done => {
                    poll_done = true;
                    let _ = shutdown_tx.send(true);
                    if let Err(join_err) = poll_res
                        && join_err.is_panic()
                    {
                        first_panic = Some(join_err.into_panic());
                    }
                    break;
                }
                Some(task) = spawn_rx.recv() => {
                    #[cfg(test)]
                    if hold_spawn_rx.load(std::sync::atomic::Ordering::SeqCst) {
                        held_unpolled_tasks.push(task);
                        continue;
                    }
                    join_set.spawn(task);
                }
                Some(res) = join_set.join_next(), if !join_set.is_empty() => {
                    if let Err(join_err) = res
                        && join_err.is_panic()
                    {
                        first_panic = Some(join_err.into_panic());
                        let _ = shutdown_tx.send(true);
                        break;
                    }
                }
            }
        }

        // 4. 等待轮询循环、全部动态子任务及收发适配器阻塞任务完整收尾。
        if !poll_done
            && let Err(join_err) = (&mut poll_task).await
            && join_err.is_panic()
            && first_panic.is_none()
        {
            first_panic = Some(join_err.into_panic());
        }
        poll_abort_guard.disarm();
        drop(state);
        #[cfg(test)]
        for task in held_unpolled_tasks.drain(..) {
            join_set.spawn(task);
        }
        while let Ok(task) = spawn_rx.try_recv() {
            join_set.spawn(task);
        }
        while let Some(res) = join_set.join_next().await {
            if let Err(join_err) = res
                && join_err.is_panic()
                && first_panic.is_none()
            {
                first_panic = Some(join_err.into_panic());
            }
            while let Ok(task) = spawn_rx.try_recv() {
                join_set.spawn(task);
            }
        }
        io_for_drain.drain().await;

        if let Some(panic_payload) = first_panic {
            std::panic::resume_unwind(panic_payload);
        }

        Ok(())
    }
}

type BoxTask = Pin<Box<dyn Future<Output = ()> + Send + 'static>>;

/// 确保宿主强制 `abort` 丢弃 `run` future 时，`poll_task` 也与 `JoinSet` 子任务一样被立即中止，不滞留旧实例引用。
struct AbortTaskOnDrop(Option<tokio::task::AbortHandle>);

impl AbortTaskOnDrop {
    fn disarm(&mut self) {
        self.0 = None;
    }
}

impl Drop for AbortTaskOnDrop {
    fn drop(&mut self) {
        if let Some(handle) = self.0.take() {
            handle.abort();
        }
    }
}

/// 在途去重集合的 RAII 守卫：仅持有去重集合引用（不反向持有 `GatewayRuntime`，杜绝引用成环），
/// 无论正常落账、停机取消、未轮询即丢弃还是异常展开，释放时均自动移除去重标记。
pub(crate) struct InFlightGuard {
    in_flight_inbound: Arc<StdMutex<BTreeSet<InboundId>>>,
    inbound_id: Option<InboundId>,
}

impl InFlightGuard {
    #[cfg(test)]
    pub(crate) fn new_for_test(
        in_flight_inbound: Arc<StdMutex<BTreeSet<InboundId>>>,
        inbound_id: InboundId,
    ) -> Self {
        in_flight_inbound
            .lock()
            .expect("在途去重集合锁不应中毒")
            .insert(inbound_id.clone());
        Self {
            in_flight_inbound,
            inbound_id: Some(inbound_id),
        }
    }

    fn clear_now(&mut self) {
        if let Some(id) = self.inbound_id.take() {
            self.in_flight_inbound
                .lock()
                .expect("在途去重集合锁不应中毒")
                .remove(&id);
        }
    }
}

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        self.clear_now();
    }
}

#[cfg(test)]
pub(crate) struct GatewayDebugProbe {
    runtime_weak: std::sync::Weak<GatewayRuntime>,
    in_flight_inbound: Arc<StdMutex<BTreeSet<InboundId>>>,
    chat_queues: Arc<ChatAdmissionQueues>,
    worker_ever_polled: Arc<std::sync::atomic::AtomicBool>,
    hold_spawn_rx: Arc<std::sync::atomic::AtomicBool>,
}

#[cfg(not(test))]
struct GatewayDebugProbe;

#[cfg(test)]
impl GatewayDebugProbe {
    pub(crate) fn is_runtime_dropped(&self) -> bool {
        self.runtime_weak.strong_count() == 0
    }

    pub(crate) fn was_worker_ever_polled(&self) -> bool {
        self.worker_ever_polled
            .load(std::sync::atomic::Ordering::SeqCst)
    }

    pub(crate) fn set_hold_spawn_rx(&self, hold: bool) {
        self.hold_spawn_rx
            .store(hold, std::sync::atomic::Ordering::SeqCst);
    }

    pub(crate) fn in_flight_len(&self) -> usize {
        self.in_flight_inbound
            .lock()
            .expect("在途去重集合锁不应中毒")
            .len()
    }

    pub(crate) fn contains_in_flight(&self, id: &InboundId) -> bool {
        self.in_flight_inbound
            .lock()
            .expect("在途去重集合锁不应中毒")
            .contains(id)
    }

    pub(crate) fn chat_queue_entry_count(&self) -> usize {
        self.chat_queues
            .entries
            .lock()
            .expect("对话准入队列锁不应中毒")
            .len()
    }

    pub(crate) fn enqueue_media_for_test(
        &self,
        inbound_id: InboundId,
        chat: ChatKey,
        file_id: String,
    ) -> bool {
        let Some(rt) = self.runtime_weak.upgrade() else {
            return false;
        };
        rt.enqueue_single_ledger_event(
            inbound_id,
            chat,
            LedgerInboundAction::SubmitMedia(vec![IncomingMediaItem::image(
                file_id,
                Some(64),
                None,
            )]),
        );
        true
    }
}

#[cfg(test)]
static TEST_WORKER_PANIC_INBOUND_IDS: StdMutex<BTreeSet<InboundId>> =
    StdMutex::new(BTreeSet::new());

#[cfg(test)]
pub(crate) fn arm_chat_worker_panic_for_test(inbound_id: InboundId) {
    TEST_WORKER_PANIC_INBOUND_IDS
        .lock()
        .expect("测试准入 panic 集合锁不应中毒")
        .insert(inbound_id);
}

#[cfg(test)]
fn take_chat_worker_panic_for_test(inbound_id: &InboundId) -> bool {
    TEST_WORKER_PANIC_INBOUND_IDS
        .lock()
        .expect("测试准入 panic 集合锁不应中毒")
        .remove(inbound_id)
}

struct QueuedChatAdmission {
    primary_inbound_id: InboundId,
    merged_inbound_ids: Vec<InboundId>,
    chat: ChatKey,
    action: LedgerInboundAction,
    in_flight_guards: Vec<InFlightGuard>,
}

impl QueuedChatAdmission {
    fn clear_all(&mut self) {
        clear_all_guards(&mut self.in_flight_guards);
    }

    fn all_inbound_ids(&self) -> Vec<InboundId> {
        collect_inbound_ids(&self.primary_inbound_id, &self.merged_inbound_ids)
    }
}

fn clear_all_guards(guards: &mut [InFlightGuard]) {
    for guard in guards {
        guard.clear_now();
    }
}

fn collect_inbound_ids(primary: &InboundId, merged: &[InboundId]) -> Vec<InboundId> {
    let mut ids = Vec::with_capacity(1 + merged.len());
    ids.push(primary.clone());
    ids.extend_from_slice(merged);
    ids
}

struct ChatQueueEntry {
    pending_startup: usize,
    startup_tx: watch::Sender<usize>,
    queue: VecDeque<QueuedChatAdmission>,
    worker_running: bool,
}

impl Default for ChatQueueEntry {
    fn default() -> Self {
        let (startup_tx, _rx) = watch::channel(0usize);
        Self {
            pending_startup: 0,
            startup_tx,
            queue: VecDeque::new(),
            worker_running: false,
        }
    }
}

/// 按 `ChatKey` 维护启动期未决 `Registered` 屏障与严格按到达顺序出队的单对话准入队列。
#[derive(Default)]
struct ChatAdmissionQueues {
    entries: StdMutex<BTreeMap<ChatKey, ChatQueueEntry>>,
}

impl ChatAdmissionQueues {
    fn register_startup_barrier(self: &Arc<Self>, chat: &ChatKey) -> StartupBarrierGuard {
        let mut guard = self.entries.lock().expect("对话准入队列锁不应中毒");
        let entry = guard.entry(chat.clone()).or_default();
        entry.pending_startup += 1;
        entry.startup_tx.send_replace(entry.pending_startup);
        drop(guard);
        StartupBarrierGuard {
            queues: Arc::clone(self),
            chat: chat.clone(),
        }
    }

    /// 在分发点同步将事件压入对应对话的 FIFO 队列；若该对话当前无活跃工作任务，
    /// 则在派生任务前立即构造并返回 `ChatWorkerGuard`，保证即使工作 future 在首次 poll 前被丢弃也能清空队列。
    fn enqueue(self: &Arc<Self>, item: QueuedChatAdmission) -> Option<ChatWorkerGuard> {
        let chat = item.chat.clone();
        let mut guard = self.entries.lock().expect("对话准入队列锁不应中毒");
        let entry = guard.entry(chat.clone()).or_default();
        entry.queue.push_back(item);
        let start_worker = !entry.worker_running;
        if start_worker {
            entry.worker_running = true;
        }
        drop(guard);
        start_worker.then(|| ChatWorkerGuard::new(Arc::clone(self), chat))
    }

    async fn wait_startup_settled(&self, chat: &ChatKey, mut stop: watch::Receiver<bool>) -> bool {
        let mut rx = {
            let guard = self.entries.lock().expect("对话准入队列锁不应中毒");
            let Some(entry) = guard.get(chat) else {
                return true;
            };
            if entry.pending_startup == 0 {
                return true;
            }
            entry.startup_tx.subscribe()
        };

        loop {
            if *rx.borrow_and_update() == 0 {
                return true;
            }
            tokio::select! {
                _ = wait_stop_signal(&mut stop) => return false,
                changed = rx.changed() => {
                    if changed.is_err() {
                        return true;
                    }
                }
            }
        }
    }

    fn pop_next_or_finish(
        &self,
        chat: &ChatKey,
        worker_guard: &mut ChatWorkerGuard,
    ) -> Option<QueuedChatAdmission> {
        let mut guard = self.entries.lock().expect("对话准入队列锁不应中毒");
        let entry = guard.get_mut(chat)?;
        if let Some(next) = entry.queue.pop_front() {
            return Some(next);
        }
        entry.worker_running = false;
        if entry.pending_startup == 0 {
            guard.remove(chat);
        }
        worker_guard.disarm();
        None
    }
}

/// 单条启动期 `Registered` 恢复任务的准入屏障守卫：得出确定准入结论（或停机/异常释放）时自动递减屏障计数。
struct StartupBarrierGuard {
    queues: Arc<ChatAdmissionQueues>,
    chat: ChatKey,
}

impl Drop for StartupBarrierGuard {
    fn drop(&mut self) {
        let mut guard = self.queues.entries.lock().expect("对话准入队列锁不应中毒");
        if let Some(entry) = guard.get_mut(&self.chat) {
            entry.pending_startup = entry.pending_startup.saturating_sub(1);
            entry.startup_tx.send_replace(entry.pending_startup);
            if entry.pending_startup == 0 && !entry.worker_running && entry.queue.is_empty() {
                guard.remove(&self.chat);
            }
        }
    }
}

/// 单对话准入工作任务的生命周期守卫：停机或异常退出时自动排空未执行队列（触发 `InFlightGuard` 清理）并复位运行标志。
struct ChatWorkerGuard {
    queues: Arc<ChatAdmissionQueues>,
    chat: Option<ChatKey>,
}

impl ChatWorkerGuard {
    fn new(queues: Arc<ChatAdmissionQueues>, chat: ChatKey) -> Self {
        Self {
            queues,
            chat: Some(chat),
        }
    }

    fn disarm(&mut self) {
        self.chat = None;
    }
}

impl Drop for ChatWorkerGuard {
    fn drop(&mut self) {
        let Some(chat) = self.chat.take() else {
            return;
        };
        let drained = {
            let mut guard = self.queues.entries.lock().expect("对话准入队列锁不应中毒");
            let mut items = VecDeque::new();
            if let Some(entry) = guard.get_mut(&chat) {
                std::mem::swap(&mut items, &mut entry.queue);
                entry.worker_running = false;
                if entry.pending_startup == 0 {
                    guard.remove(&chat);
                }
            }
            items
        };
        // 在锁外释放未执行队列元素，避免 InFlightGuard::drop 嵌套锁顺序问题。
        drop(drained);
    }
}

struct GatewayRuntime {
    config: TelegramGatewayConfig,
    service: Service,
    ledger: Ledger,
    io: Arc<dyn TelegramIo>,
    redactor: ImRedactor,
    in_flight_inbound: Arc<StdMutex<BTreeSet<InboundId>>>,
    chat_queues: Arc<ChatAdmissionQueues>,
    active_drafts: Arc<ActiveDrafts>,
    draft_throttles: ChatDraftThrottleMap,
    shutdown_rx: watch::Receiver<bool>,
    spawn_tx: mpsc::UnboundedSender<BoxTask>,
    #[cfg(test)]
    worker_ever_polled: Arc<std::sync::atomic::AtomicBool>,
}

impl GatewayRuntime {
    fn is_stopping(&self) -> bool {
        *self.shutdown_rx.borrow() || self.service.stopping().is_stopping()
    }

    fn spawn_tracked<F>(&self, fut: F)
    where
        F: Future<Output = ()> + Send + 'static,
    {
        let _ = self
            .spawn_tx
            .send(Box::pin(lattice_server::scoped_component_future(fut)));
    }

    fn ack_update(&self, id: &InboundId) {
        self.io.acknowledge(std::slice::from_ref(id));
    }

    fn ack_updates(&self, ids: &[InboundId]) {
        if !ids.is_empty() {
            self.io.acknowledge(ids);
        }
    }

    fn is_in_flight(&self, inbound_id: &InboundId) -> bool {
        self.in_flight_inbound
            .lock()
            .expect("在途去重集合锁不应中毒")
            .contains(inbound_id)
    }

    fn try_acquire_in_flight(&self, inbound_id: InboundId) -> Option<InFlightGuard> {
        let inserted = self
            .in_flight_inbound
            .lock()
            .expect("在途去重集合锁不应中毒")
            .insert(inbound_id.clone());
        inserted.then(|| InFlightGuard {
            in_flight_inbound: Arc::clone(&self.in_flight_inbound),
            inbound_id: Some(inbound_id),
        })
    }

    async fn poll_updates_loop(self: &Arc<Self>) {
        let mut backoff = INITIAL_POLL_BACKOFF;
        let mut stop = self.shutdown_rx.clone();
        let mut collector = AlbumCollector::default();

        loop {
            if self.is_stopping() {
                return;
            }
            self.enqueue_closed_albums(collector.close_due(tokio::time::Instant::now()));

            let mut poll_fut = std::pin::pin!(self.io.get_updates(LONG_POLL_TIMEOUT_SECS));
            let poll_res = loop {
                if let Some(deadline) = collector.next_deadline() {
                    tokio::select! {
                        biased;
                        _ = wait_stop_signal(&mut stop) => return,
                        _ = tokio::time::sleep_until(deadline) => {
                            self.enqueue_closed_albums(
                                collector.close_due(tokio::time::Instant::now()),
                            );
                        }
                        res = &mut poll_fut => break res,
                    }
                } else {
                    tokio::select! {
                        biased;
                        _ = wait_stop_signal(&mut stop) => return,
                        res = &mut poll_fut => break res,
                    }
                }
            };

            match poll_res {
                Ok(batch) => {
                    backoff = INITIAL_POLL_BACKOFF;
                    let batch_had_updates = batch.had_raw_updates || !batch.updates.is_empty();
                    let mut saw_new_update = false;
                    for update in batch.updates {
                        if self.is_stopping() {
                            return;
                        }
                        if self.dispatch_update(update, &mut collector).await {
                            saw_new_update = true;
                        }
                    }
                    self.enqueue_closed_albums(collector.close_due(tokio::time::Instant::now()));
                    let next_deadline = collector.next_deadline();
                    if !saw_new_update && (batch_had_updates || next_deadline.is_some()) {
                        let throttle = match next_deadline {
                            Some(deadline) => IDLE_POLL_THROTTLE.min(
                                deadline.saturating_duration_since(tokio::time::Instant::now()),
                            ),
                            None => IDLE_POLL_THROTTLE,
                        };
                        if !throttle.is_zero() {
                            tokio::select! {
                                biased;
                                _ = wait_stop_signal(&mut stop) => return,
                                _ = tokio::time::sleep(throttle) => {}
                            }
                        }
                    }
                }
                Err(TelegramApiError::RateLimited {
                    retry_after,
                    diagnostic,
                }) => {
                    let wait = retry_after.unwrap_or(backoff);
                    let wait_ms = u64::try_from(wait.as_millis()).unwrap_or(u64::MAX);
                    tracing::warn!(
                        wait_ms,
                        diagnostic = %diagnostic,
                        "Telegram getUpdates 触发限流，退避后重试"
                    );
                    if !self
                        .sleep_poll_backoff_closing_albums(wait, &mut collector, &mut stop)
                        .await
                    {
                        return;
                    }
                    backoff = backoff.saturating_mul(2).min(MAX_POLL_BACKOFF);
                }
                Err(err) => {
                    let wait_ms = u64::try_from(backoff.as_millis()).unwrap_or(u64::MAX);
                    tracing::warn!(
                        wait_ms,
                        diagnostic = %err.diagnostic(),
                        "Telegram getUpdates 失败，退避后重试"
                    );
                    if !self
                        .sleep_poll_backoff_closing_albums(backoff, &mut collector, &mut stop)
                        .await
                    {
                        return;
                    }
                    backoff = backoff.saturating_mul(2).min(MAX_POLL_BACKOFF);
                }
            }
        }
    }

    async fn sleep_poll_backoff_closing_albums(
        self: &Arc<Self>,
        wait: Duration,
        collector: &mut AlbumCollector,
        stop: &mut watch::Receiver<bool>,
    ) -> bool {
        let backoff_deadline = tokio::time::Instant::now().checked_add(wait);
        loop {
            if self.is_stopping() {
                return false;
            }
            let now = tokio::time::Instant::now();
            self.enqueue_closed_albums(collector.close_due(now));
            if backoff_deadline.is_some_and(|deadline| now >= deadline) {
                return true;
            }
            let next_wake = match (collector.next_deadline(), backoff_deadline) {
                (Some(album_deadline), Some(backoff_until)) => {
                    Some(album_deadline.min(backoff_until))
                }
                (Some(album_deadline), None) => Some(album_deadline),
                (None, Some(backoff_until)) => Some(backoff_until),
                (None, None) => None,
            };
            let Some(wake_at) = next_wake else {
                wait_stop_signal(stop).await;
                return false;
            };
            tokio::select! {
                biased;
                _ = wait_stop_signal(stop) => return false,
                _ = tokio::time::sleep_until(wake_at) => {}
            }
        }
    }

    async fn dispatch_update(
        self: &Arc<Self>,
        update: TelegramUpdate,
        collector: &mut AlbumCollector,
    ) -> bool {
        let update_id = update.update_id;
        let inbound_id = update.inbound_id;
        if self.is_in_flight(&inbound_id) || self.io.is_acknowledged(&inbound_id) {
            return false;
        }

        match update.payload {
            TelegramUpdatePayload::Ignored => {
                self.ack_update(&inbound_id);
                true
            }
            TelegramUpdatePayload::StoppedMessageGeneration {
                chat,
                is_private,
                draft_id,
            } => {
                // 停止事件不写台账、不闭合开放相册，收到即确认。
                self.ack_update(&inbound_id);
                if is_private && let Some(turn_id) = self.active_drafts.lookup(&chat, draft_id) {
                    let rt = Arc::clone(self);
                    self.spawn_tracked(async move {
                        let reply = cancel_turn_and_format(&rt.service, turn_id).await;
                        let _ = send_text_reliably(
                            rt.io.as_ref(),
                            &chat,
                            reply,
                            rt.shutdown_rx.clone(),
                        )
                        .await;
                    });
                }
                true
            }
            TelegramUpdatePayload::Message {
                chat,
                is_private,
                sender,
                media_group_id,
                content,
            } => {
                if !is_authorized_private_message(
                    is_private,
                    sender.as_ref(),
                    &self.config.allowed_senders,
                ) {
                    tracing::debug!(
                        chat = %chat,
                        is_private,
                        "忽略未授权或非私聊入站消息并立即确认"
                    );
                    self.ack_update(&inbound_id);
                    return true;
                }

                match (media_group_id, content) {
                    (
                        Some(group_id),
                        content @ (TelegramMessageContent::Image { .. }
                        | TelegramMessageContent::Unsupported),
                    ) => {
                        if self.ledger.inbound_record(&inbound_id).await.is_some() {
                            self.ack_update(&inbound_id);
                            return true;
                        }
                        if let Some(displaced) =
                            collector.close_if_different_group(&chat, &group_id)
                        {
                            self.enqueue_closed_album(displaced);
                        }
                        if collector.is_recently_closed(&chat, &group_id) {
                            self.ack_update(&inbound_id);
                            let rt = Arc::clone(self);
                            self.spawn_tracked(async move {
                                let _ = send_text_reliably(
                                    rt.io.as_ref(),
                                    &chat,
                                    LATE_ALBUM_FRAGMENT_REPLY,
                                    rt.shutdown_rx.clone(),
                                )
                                .await;
                            });
                            return true;
                        }
                        let Some(in_flight_guard) = self.try_acquire_in_flight(inbound_id.clone())
                        else {
                            return false;
                        };
                        let media_item = match content {
                            TelegramMessageContent::Image {
                                file_id,
                                file_size,
                                caption,
                            } => IncomingMediaItem::image(file_id, file_size, caption),
                            TelegramMessageContent::Unsupported => IncomingMediaItem::unsupported(),
                            TelegramMessageContent::Text(_) => {
                                unreachable!("外层模式已限定为图片或不受支持媒体")
                            }
                        };
                        let (displaced, closed_now) = collector.push_fragment(
                            chat,
                            group_id,
                            BufferedAlbumItem {
                                update_id,
                                inbound_id,
                                item: media_item,
                                in_flight_guard,
                            },
                            tokio::time::Instant::now(),
                        );
                        if let Some(closed) = displaced {
                            self.enqueue_closed_album(closed);
                        }
                        if let Some(closed) = closed_now {
                            self.enqueue_closed_album(closed);
                        }
                        true
                    }
                    (_, content) => {
                        if self.ledger.inbound_record(&inbound_id).await.is_some() {
                            self.ack_update(&inbound_id);
                            return true;
                        }
                        if let Some(closed) = collector.close_for_chat(&chat) {
                            self.enqueue_closed_album(closed);
                        }
                        match content {
                            TelegramMessageContent::Unsupported => {
                                // 不受支持的独立媒体或空消息：不写台账、不建会话，立即确认后回复提示。
                                self.ack_update(&inbound_id);
                                let rt = Arc::clone(self);
                                self.spawn_tracked(async move {
                                    let _ = send_text_reliably(
                                        rt.io.as_ref(),
                                        &chat,
                                        UNSUPPORTED_MEDIA_REPLY,
                                        rt.shutdown_rx.clone(),
                                    )
                                    .await;
                                });
                            }
                            TelegramMessageContent::Image {
                                file_id,
                                file_size,
                                caption,
                            } => {
                                let media_item =
                                    IncomingMediaItem::image(file_id, file_size, caption);
                                self.enqueue_single_ledger_event(
                                    inbound_id,
                                    chat,
                                    LedgerInboundAction::SubmitMedia(vec![media_item]),
                                );
                            }
                            TelegramMessageContent::Text(text) => match parse_text_input(&text) {
                                ParsedInput::Command(ImCommand::Help | ImCommand::Start) => {
                                    self.ack_update(&inbound_id);
                                    let rt = Arc::clone(self);
                                    self.spawn_tracked(async move {
                                        let _ = send_text_reliably(
                                            rt.io.as_ref(),
                                            &chat,
                                            HELP_REPLY,
                                            rt.shutdown_rx.clone(),
                                        )
                                        .await;
                                    });
                                }
                                ParsedInput::Command(
                                    cmd
                                    @ (ImCommand::Cancel | ImCommand::Status | ImCommand::Usage),
                                ) => {
                                    // 不写台账的查询与取消命令收到即确认，回复发出与否不影响 offset。
                                    self.ack_update(&inbound_id);
                                    let rt = Arc::clone(self);
                                    self.spawn_tracked(async move {
                                        let bound = rt.ledger.bound_session(&chat).await;
                                        let reply = match cmd {
                                            ImCommand::Cancel => {
                                                execute_cancel_command(&rt.service, bound)
                                                    .await
                                                    .to_owned()
                                            }
                                            ImCommand::Status => {
                                                execute_status_command(
                                                    &rt.service,
                                                    bound,
                                                    &rt.redactor,
                                                )
                                                .await
                                            }
                                            ImCommand::Usage => {
                                                execute_usage_command(
                                                    &rt.service,
                                                    bound,
                                                    &rt.redactor,
                                                )
                                                .await
                                            }
                                            _ => unreachable!("外层分支已限定命令种类"),
                                        };
                                        let _ = send_text_reliably(
                                            rt.io.as_ref(),
                                            &chat,
                                            &reply,
                                            rt.shutdown_rx.clone(),
                                        )
                                        .await;
                                    });
                                }
                                ParsedInput::Command(ImCommand::New) => {
                                    self.enqueue_single_ledger_event(
                                        inbound_id,
                                        chat,
                                        LedgerInboundAction::ResetBinding,
                                    );
                                }
                                ParsedInput::Message => {
                                    self.enqueue_single_ledger_event(
                                        inbound_id,
                                        chat,
                                        LedgerInboundAction::SubmitMessage(text),
                                    );
                                }
                            },
                        }
                        true
                    }
                }
            }
        }
    }

    fn enqueue_admission(self: &Arc<Self>, item: QueuedChatAdmission) {
        // 在分发点同步排入单对话 FIFO 队列并在派生任务前构造 ChatWorkerGuard，
        // 确保即使工作任务在首次 poll 前被取消丢弃，队列与在途去重标记也会立即释放且不泄漏实例引用。
        let worker_chat = item.chat.clone();
        if let Some(worker_guard) = self.chat_queues.enqueue(item) {
            let rt = Arc::clone(self);
            self.spawn_tracked(async move {
                rt.run_chat_worker(worker_chat, worker_guard).await;
            });
        }
    }

    fn enqueue_single_ledger_event(
        self: &Arc<Self>,
        inbound_id: InboundId,
        chat: ChatKey,
        action: LedgerInboundAction,
    ) {
        let Some(in_flight_guard) = self.try_acquire_in_flight(inbound_id.clone()) else {
            return;
        };
        self.enqueue_admission(QueuedChatAdmission {
            primary_inbound_id: inbound_id,
            merged_inbound_ids: Vec::new(),
            chat,
            action,
            in_flight_guards: vec![in_flight_guard],
        });
    }

    fn enqueue_closed_album(self: &Arc<Self>, closed: ClosedAlbum) {
        self.enqueue_admission(QueuedChatAdmission {
            primary_inbound_id: closed.primary_inbound_id,
            merged_inbound_ids: closed.merged_inbound_ids,
            chat: closed.chat,
            action: LedgerInboundAction::SubmitMedia(closed.media_items),
            in_flight_guards: closed.in_flight_guards,
        });
    }

    fn enqueue_closed_albums(self: &Arc<Self>, closed_albums: Vec<ClosedAlbum>) {
        for closed in closed_albums {
            self.enqueue_closed_album(closed);
        }
    }

    async fn run_chat_worker(self: &Arc<Self>, chat: ChatKey, mut worker_guard: ChatWorkerGuard) {
        #[cfg(test)]
        self.worker_ever_polled
            .store(true, std::sync::atomic::Ordering::SeqCst);

        loop {
            if !self
                .chat_queues
                .wait_startup_settled(&chat, self.shutdown_rx.clone())
                .await
            {
                return;
            }
            if self.is_stopping() {
                return;
            }

            let Some(mut item) = self
                .chat_queues
                .pop_next_or_finish(&chat, &mut worker_guard)
            else {
                return;
            };

            #[cfg(test)]
            if take_chat_worker_panic_for_test(&item.primary_inbound_id) {
                panic!("注入单对话准入子任务 panic 以验证守卫释放与宿主传播");
            }

            // 出队后再核验一次平台确认状态与台账幂等，防止重投事件重复下载或重复回复。
            if self.io.is_acknowledged(&item.primary_inbound_id)
                || self
                    .ledger
                    .inbound_record(&item.primary_inbound_id)
                    .await
                    .is_some()
            {
                let all_ids = item.all_inbound_ids();
                self.ack_updates(&all_ids);
                item.clear_all();
                continue;
            }

            let keep_going = match item.action {
                LedgerInboundAction::ResetBinding => {
                    self.handle_reset_command(
                        &mut item.in_flight_guards,
                        item.primary_inbound_id,
                        item.chat,
                    )
                    .await
                }
                LedgerInboundAction::SubmitMessage(text) => {
                    self.handle_turn_parts(
                        &mut item.in_flight_guards,
                        item.primary_inbound_id,
                        item.merged_inbound_ids,
                        item.chat,
                        vec![UserInputPart::Text { text }],
                    )
                    .await
                }
                LedgerInboundAction::SubmitMedia(ref media_items) => {
                    match prepare_media_turn(
                        self.io.as_ref(),
                        media_items,
                        self.shutdown_rx.clone(),
                    )
                    .await
                    {
                        MediaBatchOutcome::Interrupted => false,
                        MediaBatchOutcome::Rejected(reason) => {
                            let all_ids = item.all_inbound_ids();
                            self.ack_updates(&all_ids);
                            item.clear_all();
                            let chat_for_reply = item.chat;
                            let rt = Arc::clone(self);
                            self.spawn_tracked(async move {
                                let _ = send_text_reliably(
                                    rt.io.as_ref(),
                                    &chat_for_reply,
                                    &reason,
                                    rt.shutdown_rx.clone(),
                                )
                                .await;
                            });
                            !self.is_stopping()
                        }
                        MediaBatchOutcome::Ready(parts) => {
                            self.handle_turn_parts(
                                &mut item.in_flight_guards,
                                item.primary_inbound_id,
                                item.merged_inbound_ids,
                                item.chat,
                                parts,
                            )
                            .await
                        }
                    }
                }
            };
            if !keep_going {
                return;
            }
        }
    }

    async fn handle_reset_command(
        self: &Arc<Self>,
        in_flight_guards: &mut [InFlightGuard],
        inbound_id: InboundId,
        chat: ChatKey,
    ) -> bool {
        let mut backoff = INITIAL_RECOVERY_BACKOFF;
        let mut stop = self.shutdown_rx.clone();
        loop {
            if self.is_stopping() {
                return false;
            }
            match self
                .ledger
                .record_reset(inbound_id.clone(), chat.clone(), Utc::now())
                .await
            {
                Ok(()) => break,
                Err(LedgerError::PublicationClosed) => return false,
                Err(err) => {
                    // 严格保持队首重试，绝不让同 chat 后续消息越过未落账的 /new 乱序执行。
                    tracing::warn!(
                        chat = %chat,
                        error = %err,
                        "写入 /new 台账记录失败，保持单对话队首退避重试"
                    );
                    tokio::select! {
                        _ = wait_stop_signal(&mut stop) => return false,
                        _ = tokio::time::sleep(backoff) => {}
                    }
                    backoff = backoff.saturating_mul(2).min(MAX_RECOVERY_BACKOFF);
                }
            }
        }
        self.ack_update(&inbound_id);
        clear_all_guards(in_flight_guards);

        // 台账重置已完成，确认提示异步投递，不阻塞同 chat 后续消息准入。
        let rt = Arc::clone(self);
        self.spawn_tracked(async move {
            let _ = send_text_reliably(
                rt.io.as_ref(),
                &chat,
                NEW_RESET_REPLY,
                rt.shutdown_rx.clone(),
            )
            .await;
        });
        true
    }

    async fn handle_turn_parts(
        self: &Arc<Self>,
        in_flight_guards: &mut [InFlightGuard],
        primary_inbound_id: InboundId,
        merged_inbound_ids: Vec<InboundId>,
        chat: ChatKey,
        parts: Vec<UserInputPart>,
    ) -> bool {
        let existing_binding = self.ledger.bound_session(&chat).await;
        let is_new = existing_binding.is_none();
        let session_id = existing_binding.unwrap_or_else(new_session_id);
        let turn_id = new_turn_id();

        let mut backoff = INITIAL_RECOVERY_BACKOFF;
        let mut stop = self.shutdown_rx.clone();
        loop {
            if self.is_stopping() {
                return false;
            }
            match self
                .ledger
                .register_turn(
                    primary_inbound_id.clone(),
                    merged_inbound_ids.clone(),
                    chat.clone(),
                    session_id,
                    turn_id,
                    is_new,
                    Utc::now(),
                )
                .await
            {
                Ok(()) => break,
                Err(LedgerError::PublicationClosed) => return false,
                Err(err) => {
                    // 严格保持队首重试，确保同 chat 后续消息或 /new 不会越过当前消息乱序执行。
                    tracing::warn!(
                        chat = %chat,
                        error = %err,
                        "登记入站消息台账失败，保持单对话队首退避重试"
                    );
                    tokio::select! {
                        _ = wait_stop_signal(&mut stop) => return false,
                        _ = tokio::time::sleep(backoff) => {}
                    }
                    backoff = backoff.saturating_mul(2).min(MAX_RECOVERY_BACKOFF);
                }
            }
        }
        // 台账原子落盘成功后先一次性批量确认首条与并入分片，再清除整组内存去重标记。
        let all_ids = collect_inbound_ids(&primary_inbound_id, &merged_inbound_ids);
        self.ack_updates(&all_ids);
        clear_all_guards(in_flight_guards);

        let retry_parts = (!is_new).then(|| parts.clone());
        let mut submit_result = if is_new {
            self.configure_new_and_submit(session_id, turn_id, parts)
                .await
        } else {
            self.submit_only(session_id, turn_id, parts).await
        };

        // 绑定的草稿会话已被后台清理或会话库尚未初始化时，复用台账中已登记的同一个 SessionId、TurnId 与 parts 重建并提交。
        if let Some(parts_for_rebuild) = retry_parts
            && let Err(ref err) = submit_result
            && is_not_found_or_uninitialized(err)
        {
            submit_result = self
                .configure_new_and_submit(session_id, turn_id, parts_for_rebuild)
                .await;
        }

        match submit_result {
            Ok(turn_handle) => {
                if let Err(err) = self.ledger.mark_accepted(&primary_inbound_id).await {
                    tracing::error!(
                        inbound_id = %primary_inbound_id,
                        error = %err,
                        "推进台账 Accepted 失败，保留原相位"
                    );
                }
                let draft_lease = self.active_drafts.register(chat.clone(), turn_id);
                // 得出准入结果后立即在受跟踪任务中异步观察与投递，放行单对话队列处理后续消息。
                let rt = Arc::clone(self);
                self.spawn_tracked(async move {
                    rt.observe_live_turn_and_deliver(
                        primary_inbound_id,
                        chat,
                        turn_handle,
                        draft_lease,
                    )
                    .await;
                });
            }
            Err(err) if err.kind == LocalErrorKind::Stopping || err.recovery.is_some() => {
                // 准入结果未确认：保持在单对话工作循环中等待按 TurnId 查询得出确定结论后再放行后续消息。
                let mut no_startup_guard = None;
                self.resolve_registered_until_settled(
                    &mut no_startup_guard,
                    primary_inbound_id,
                    chat,
                    session_id,
                    turn_id,
                )
                .await;
            }
            Err(err) => {
                // 明确拒绝：立即放行单对话队列，在后台任务中回复拒绝原因并将首条台账记为 Delivered。
                let reply = describe_local_error(&err, &self.redactor);
                let rt = Arc::clone(self);
                self.spawn_tracked(async move {
                    if send_text_reliably(rt.io.as_ref(), &chat, &reply, rt.shutdown_rx.clone())
                        .await
                        .is_ok()
                        && let Err(ledger_err) = rt
                            .ledger
                            .mark_delivered(&primary_inbound_id, Utc::now())
                            .await
                    {
                        tracing::error!(
                            inbound_id = %primary_inbound_id,
                            error = %ledger_err,
                            "拒绝回复发出后更新台账 Delivered 失败"
                        );
                    }
                });
            }
        }
        !self.is_stopping()
    }

    async fn configure_new_and_submit(
        &self,
        session_id: SessionId,
        turn_id: TurnId,
        parts: Vec<UserInputPart>,
    ) -> Result<InProcessTurn, LocalError> {
        let configure_res = self
            .service
            .configure(ConfigureRequest {
                session: ConfigureSession::New(session_id),
                workspace: self.config.workspace.clone(),
                agent: Some(self.config.agent.clone()),
                model: None,
                effort: None,
            })
            .await;
        match configure_res {
            Ok(_) => {}
            Err(err) if err.conflict == Some(ConflictReason::SessionExists) => {}
            Err(err) => return Err(err),
        }
        self.submit_only(session_id, turn_id, parts).await
    }

    async fn submit_only(
        &self,
        session_id: SessionId,
        turn_id: TurnId,
        parts: Vec<UserInputPart>,
    ) -> Result<InProcessTurn, LocalError> {
        self.service
            .submit(TurnRequest {
                turn_id,
                session: session_id,
                workspace: self.config.workspace.clone(),
                parts,
                expected_settings: None,
            })
            .await
    }

    async fn observe_live_turn_and_deliver(
        self: &Arc<Self>,
        inbound_id: InboundId,
        chat: ChatKey,
        mut turn: InProcessTurn,
        draft_lease: DraftLease,
    ) {
        drive_turn_drafts(
            &mut turn,
            &chat,
            draft_lease.draft_id(),
            self.io.as_ref(),
            &self.draft_throttles,
            self.shutdown_rx.clone(),
        )
        .await;
        drop(draft_lease);

        if self.is_stopping() {
            return;
        }

        let mut stop = self.shutdown_rx.clone();
        let outcome_res = tokio::select! {
            _ = wait_stop_signal(&mut stop) => return,
            res = turn.outcome() => res,
        };

        match outcome_res {
            Ok(outcome) => {
                let final_text = compose_final_reply(&outcome, &self.redactor);
                if send_text_reliably(
                    self.io.as_ref(),
                    &chat,
                    &final_text,
                    self.shutdown_rx.clone(),
                )
                .await
                .is_ok()
                    && let Err(err) = self.ledger.mark_delivered(&inbound_id, Utc::now()).await
                {
                    tracing::error!(
                        inbound_id = %inbound_id,
                        error = %err,
                        "最终回复发送后推进台账 Delivered 失败"
                    );
                }
            }
            Err(err) => {
                tracing::warn!(
                    inbound_id = %inbound_id,
                    error = %self.redactor.sanitize_single_line(&err.message),
                    "读取在途轮次终态失败，转入 Accepted 退避恢复循环"
                );
                let session_id = turn.session_id();
                let turn_id = turn.turn_id();
                self.recover_accepted_turn(inbound_id, chat, session_id, turn_id)
                    .await;
            }
        }
    }

    /// 按 `TurnId` 查询恢复 `Registered` 条目；在得出已接受或未被接受的确定结论后立即释放启动屏障并将回复投递解耦到独立受跟踪任务。
    async fn resolve_registered_until_settled(
        self: &Arc<Self>,
        startup_guard: &mut Option<StartupBarrierGuard>,
        inbound_id: InboundId,
        chat: ChatKey,
        session_id: SessionId,
        turn_id: TurnId,
    ) {
        let mut backoff = INITIAL_RECOVERY_BACKOFF;
        let mut stop = self.shutdown_rx.clone();

        loop {
            if self.is_stopping() {
                return;
            }
            let lookup_res = tokio::select! {
                _ = wait_stop_signal(&mut stop) => return,
                res = self.service.turn(turn_id) => res,
            };

            match lookup_res {
                Ok(TurnLookup::Accepted { .. }) => {
                    if let Err(err) = self.ledger.mark_accepted(&inbound_id).await {
                        tracing::error!(
                            inbound_id = %inbound_id,
                            error = %err,
                            "恢复 Registered 推进 Accepted 失败，退避后重试"
                        );
                    } else {
                        // 已确认被后台接受，立即释放对话启动屏障并异步补发最终回复。
                        let _ = startup_guard.take();
                        let rt = Arc::clone(self);
                        self.spawn_tracked(async move {
                            rt.recover_accepted_turn(inbound_id, chat, session_id, turn_id)
                                .await;
                        });
                        return;
                    }
                }
                Ok(TurnLookup::InFlight) => {}
                Err(err) if is_not_found_or_uninitialized(&err) => {
                    // 确定未被后台持久接受：立即释放对话准入屏障，将“该消息未被执行”回复投递与准入判定解耦。
                    let _ = startup_guard.take();
                    let rt = Arc::clone(self);
                    self.spawn_tracked(async move {
                        if send_text_reliably(
                            rt.io.as_ref(),
                            &chat,
                            NOT_EXECUTED_REPLY,
                            rt.shutdown_rx.clone(),
                        )
                        .await
                        .is_ok()
                            && let Err(ledger_err) =
                                rt.ledger.mark_delivered(&inbound_id, Utc::now()).await
                        {
                            tracing::error!(
                                inbound_id = %inbound_id,
                                error = %ledger_err,
                                "恢复未执行消息后推进台账 Delivered 失败"
                            );
                        }
                    });
                    return;
                }
                Err(err) => {
                    tracing::warn!(
                        inbound_id = %inbound_id,
                        error = %self.redactor.sanitize_single_line(&err.message),
                        "查询遗留 Registered 轮次状态暂时失败，保留相位退避重试"
                    );
                }
            }

            tokio::select! {
                _ = wait_stop_signal(&mut stop) => return,
                _ = tokio::time::sleep(backoff) => {}
            }
            backoff = backoff.saturating_mul(2).min(MAX_RECOVERY_BACKOFF);
        }
    }

    /// 恢复 `Accepted` 相位条目：附着观察直至轮次结束（不重发流式草稿），发出最终回复后记 `Delivered`。
    async fn recover_accepted_turn(
        self: &Arc<Self>,
        inbound_id: InboundId,
        chat: ChatKey,
        session_id: SessionId,
        turn_id: TurnId,
    ) {
        let mut backoff = INITIAL_RECOVERY_BACKOFF;
        let mut stop = self.shutdown_rx.clone();

        loop {
            if self.is_stopping() {
                return;
            }
            let observe_res = tokio::select! {
                _ = wait_stop_signal(&mut stop) => return,
                res = self.service.observe(session_id, turn_id) => res,
            };

            match observe_res {
                Ok(mut turn) => {
                    let outcome_res = tokio::select! {
                        _ = wait_stop_signal(&mut stop) => return,
                        res = turn.outcome() => res,
                    };
                    match outcome_res {
                        Ok(outcome) => {
                            let final_text = compose_final_reply(&outcome, &self.redactor);
                            if send_text_reliably(
                                self.io.as_ref(),
                                &chat,
                                &final_text,
                                self.shutdown_rx.clone(),
                            )
                            .await
                            .is_ok()
                            {
                                if let Err(err) =
                                    self.ledger.mark_delivered(&inbound_id, Utc::now()).await
                                {
                                    tracing::error!(
                                        inbound_id = %inbound_id,
                                        error = %err,
                                        "补发 Accepted 回复后推进台账 Delivered 失败"
                                    );
                                } else {
                                    return;
                                }
                            } else {
                                return;
                            }
                        }
                        Err(err) => {
                            tracing::warn!(
                                inbound_id = %inbound_id,
                                error = %self.redactor.sanitize_single_line(&err.message),
                                "等待遗留 Accepted 轮次终态暂时失败，保留相位退避重试"
                            );
                        }
                    }
                }
                Err(err) => {
                    tracing::warn!(
                        inbound_id = %inbound_id,
                        error = %self.redactor.sanitize_single_line(&err.message),
                        "附着观察遗留 Accepted 轮次暂时失败，保留相位退避重试"
                    );
                }
            }

            tokio::select! {
                _ = wait_stop_signal(&mut stop) => return,
                _ = tokio::time::sleep(backoff) => {}
            }
            backoff = backoff.saturating_mul(2).min(MAX_RECOVERY_BACKOFF);
        }
    }
}

enum LedgerInboundAction {
    ResetBinding,
    SubmitMessage(String),
    SubmitMedia(Vec<IncomingMediaItem>),
}
