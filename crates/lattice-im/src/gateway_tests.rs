// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! `lattice-im` 端到端集成测试：通过 `TelegramIo` 假平台收发器 + `serve_for_test_with_service`
//! 真实进程内 `Service` + 临时状态根 + 本地假 HTTP 模型 + 外部 IPC `LocalClient`，覆盖全部核心行为。

use std::{
    collections::VecDeque,
    ffi::OsStr,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    sync::{Arc, Mutex as StdMutex},
    time::Duration,
};

use chrono::Utc;
use lattice_ipc::{
    ConfigureRequest, ConfigureSession, LocalErrorKind, LocalUpdate, PublicTurnOutcome,
    SubmissionResult, SubmissionUpdate, TurnRequest,
};
use lattice_local::LocalClient;
use lattice_server::{
    DrainOutcome, ImAuthStore, LocalWorkspaceRoot, ServeExit, Service, TELEGRAM_PLATFORM,
    serve_for_test_with_service,
};
use lattice_store::{PrivateDirectory, StateLayout};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::{Notify, mpsc, oneshot},
};

use crate::{
    ImBuildOutcome,
    command::{HELP_REPLY, NON_TEXT_MESSAGE_REPLY, cancel_turn_and_format},
    identity::{ChatKey, InboundId, new_session_id, new_turn_id},
    ledger::{InboundRecord, Ledger},
    prepare_im_gateway,
    telegram::{
        OffsetTracker, RawTelegramUpdate, TELEGRAM_REDELIVERY_WINDOW, TelegramApiError, TelegramIo,
        TelegramUpdate, chat_key_from_id, inbound_id_from_update_id,
    },
};

const ALLOWED_UID: i64 = 7001;
const OTHER_UID: i64 = 7002;
const PRIVATE_CHAT_ID: i64 = 7001;

async fn bounded<T>(future: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(10), future)
        .await
        .expect("集成测试步骤应在 10 秒上限内完成")
}

/// 可编程的假 `TelegramIo`：记录每次 `getUpdates` 传入的 `offset`、全部草稿与全部消息，并支持注入发送阻塞或报错。
type MatchingSendGate = (String, oneshot::Sender<()>, oneshot::Receiver<()>);

struct FakeTelegramIo {
    batches: StdMutex<VecDeque<Result<Vec<TelegramUpdate>, TelegramApiError>>>,
    batch_notify: Notify,
    offsets: StdMutex<OffsetTracker>,
    observed_offsets: StdMutex<Vec<Option<i64>>>,
    poll_timestamps: StdMutex<Vec<tokio::time::Instant>>,
    offset_notify: Notify,
    sent_messages: StdMutex<Vec<(ChatKey, String)>>,
    message_notify: Notify,
    sent_drafts: StdMutex<Vec<(ChatKey, i64, String, bool)>>,
    draft_timestamps: StdMutex<Vec<tokio::time::Instant>>,
    draft_notify: Notify,
    send_overrides: StdMutex<VecDeque<Result<(), TelegramApiError>>>,
    block_send_gate: StdMutex<Option<oneshot::Receiver<()>>>,
    matching_send_gate: StdMutex<Option<MatchingSendGate>>,
    drain_gate: StdMutex<Option<(oneshot::Sender<()>, oneshot::Receiver<()>)>>,
    panic_on_next_poll: std::sync::atomic::AtomicBool,
    send_entered_count: std::sync::atomic::AtomicUsize,
    send_entered_notify: Notify,
    slow_draft_behavior: StdMutex<Option<(Duration, Result<(), TelegramApiError>)>>,
    draft_entered_count: std::sync::atomic::AtomicUsize,
    draft_entered_notify: Notify,
}

impl Default for FakeTelegramIo {
    fn default() -> Self {
        Self {
            batches: StdMutex::new(VecDeque::new()),
            batch_notify: Notify::new(),
            offsets: StdMutex::new(OffsetTracker::default()),
            observed_offsets: StdMutex::new(Vec::new()),
            poll_timestamps: StdMutex::new(Vec::new()),
            offset_notify: Notify::new(),
            sent_messages: StdMutex::new(Vec::new()),
            message_notify: Notify::new(),
            sent_drafts: StdMutex::new(Vec::new()),
            draft_timestamps: StdMutex::new(Vec::new()),
            draft_notify: Notify::new(),
            send_overrides: StdMutex::new(VecDeque::new()),
            block_send_gate: StdMutex::new(None),
            matching_send_gate: StdMutex::new(None),
            drain_gate: StdMutex::new(None),
            panic_on_next_poll: std::sync::atomic::AtomicBool::new(false),
            send_entered_count: std::sync::atomic::AtomicUsize::new(0),
            send_entered_notify: Notify::new(),
            slow_draft_behavior: StdMutex::new(None),
            draft_entered_count: std::sync::atomic::AtomicUsize::new(0),
            draft_entered_notify: Notify::new(),
        }
    }
}

impl FakeTelegramIo {
    fn push_updates(&self, updates: Vec<TelegramUpdate>) {
        self.batches.lock().unwrap().push_back(Ok(updates));
        self.batch_notify.notify_waiters();
    }

    fn push_send_error(&self, err: TelegramApiError) {
        self.send_overrides.lock().unwrap().push_back(Err(err));
    }

    fn push_poll_error(&self, err: TelegramApiError) {
        self.batches.lock().unwrap().push_back(Err(err));
        self.batch_notify.notify_waiters();
    }

    fn arm_panic_on_next_poll(&self) {
        self.panic_on_next_poll
            .store(true, std::sync::atomic::Ordering::SeqCst);
        self.batch_notify.notify_waiters();
    }

    fn install_send_gate(&self) -> oneshot::Sender<()> {
        let (tx, rx) = oneshot::channel();
        *self.block_send_gate.lock().unwrap() = Some(rx);
        tx
    }

    fn install_matching_send_gate(
        &self,
        target_text: &str,
    ) -> (oneshot::Receiver<()>, oneshot::Sender<()>) {
        let (entered_tx, entered_rx) = oneshot::channel();
        let (release_tx, release_rx) = oneshot::channel();
        *self.matching_send_gate.lock().unwrap() =
            Some((target_text.to_owned(), entered_tx, release_rx));
        (entered_rx, release_tx)
    }

    fn install_drain_gate(&self) -> (oneshot::Receiver<()>, oneshot::Sender<()>) {
        let (entered_tx, entered_rx) = oneshot::channel();
        let (release_tx, release_rx) = oneshot::channel();
        *self.drain_gate.lock().unwrap() = Some((entered_tx, release_rx));
        (entered_rx, release_tx)
    }

    async fn wait_message_count(&self, count: usize) -> Vec<(ChatKey, String)> {
        bounded(async {
            loop {
                let notified = self.message_notify.notified();
                {
                    let guard = self.sent_messages.lock().unwrap();
                    if guard.len() >= count {
                        return guard.clone();
                    }
                }
                notified.await;
            }
        })
        .await
    }

    async fn wait_draft_count(&self, count: usize) -> Vec<(ChatKey, i64, String, bool)> {
        bounded(async {
            loop {
                let notified = self.draft_notify.notified();
                {
                    let guard = self.sent_drafts.lock().unwrap();
                    if guard.len() >= count {
                        return guard.clone();
                    }
                }
                notified.await;
            }
        })
        .await
    }

    async fn wait_offset(&self, expected: Option<i64>) {
        bounded(async {
            loop {
                let notified = self.offset_notify.notified();
                if self.observed_offsets.lock().unwrap().contains(&expected) {
                    return;
                }
                // 模拟当前挂起的长轮询超时返回空列表，使网关携带最新确认的 offset 发起下一轮轮询。
                {
                    let mut batches = self.batches.lock().unwrap();
                    if batches.is_empty() {
                        batches.push_back(Ok(Vec::new()));
                        self.batch_notify.notify_waiters();
                    }
                }
                tokio::select! {
                    _ = notified => {}
                    _ = tokio::time::sleep(Duration::from_millis(20)) => {}
                }
            }
        })
        .await;
    }

    async fn wait_poll_count(&self, count: usize) -> Vec<Option<i64>> {
        bounded(async {
            loop {
                let notified = self.offset_notify.notified();
                {
                    let guard = self.observed_offsets.lock().unwrap();
                    if guard.len() >= count {
                        return guard.clone();
                    }
                }
                notified.await;
            }
        })
        .await
    }

    fn set_slow_draft_behavior(&self, delay: Duration, result: Result<(), TelegramApiError>) {
        *self.slow_draft_behavior.lock().unwrap() = Some((delay, result));
    }

    fn draft_entered_count(&self) -> usize {
        self.draft_entered_count
            .load(std::sync::atomic::Ordering::SeqCst)
    }

    async fn wait_draft_entered(&self, min_count: usize) {
        tokio::time::timeout(Duration::from_secs(120), async {
            loop {
                let notified = self.draft_entered_notify.notified();
                if self.draft_entered_count() >= min_count {
                    return;
                }
                notified.await;
            }
        })
        .await
        .expect("等待 send_message_draft 进入超时");
    }

    async fn wait_send_entered(&self, min_count: usize) {
        bounded(async {
            loop {
                let notified = self.send_entered_notify.notified();
                if self
                    .send_entered_count
                    .load(std::sync::atomic::Ordering::SeqCst)
                    >= min_count
                {
                    return;
                }
                notified.await;
            }
        })
        .await;
    }
}

#[async_trait::async_trait]
impl TelegramIo for FakeTelegramIo {
    async fn get_updates(
        &self,
        _timeout_secs: u32,
    ) -> Result<Vec<TelegramUpdate>, TelegramApiError> {
        let offset = self.offsets.lock().unwrap().next_offset();
        {
            self.poll_timestamps
                .lock()
                .unwrap()
                .push(tokio::time::Instant::now());
            let mut guard = self.observed_offsets.lock().unwrap();
            guard.push(offset);
        }
        self.offset_notify.notify_waiters();

        loop {
            if self
                .panic_on_next_poll
                .swap(false, std::sync::atomic::Ordering::SeqCst)
            {
                panic!("注入轮询任务 panic 以验证组件监督传播");
            }
            let notified = self.batch_notify.notified();
            if let Some(batch) = self.batches.lock().unwrap().pop_front() {
                return batch.map(|updates| {
                    let mut offsets = self.offsets.lock().unwrap();
                    let mut seen_in_batch = std::collections::BTreeSet::new();
                    updates
                        .into_iter()
                        .filter(|update| {
                            seen_in_batch.insert(update.inbound_id.clone())
                                && offsets.record_received_inbound(&update.inbound_id)
                        })
                        .collect()
                });
            }
            notified.await;
        }
    }

    fn acknowledge(&self, inbound_id: &InboundId) {
        self.offsets.lock().unwrap().ack_inbound(inbound_id);
        self.offset_notify.notify_waiters();
    }

    fn is_acknowledged(&self, inbound_id: &InboundId) -> bool {
        self.offsets.lock().unwrap().is_acked_inbound(inbound_id)
    }

    async fn send_message(&self, chat: &ChatKey, text: &str) -> Result<(), TelegramApiError> {
        self.send_entered_count
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.send_entered_notify.notify_waiters();
        let maybe_gate = self.block_send_gate.lock().unwrap().take();
        if let Some(gate) = maybe_gate {
            let _ = gate.await;
        }
        let matching_gate = {
            let mut guard = self.matching_send_gate.lock().unwrap();
            if guard
                .as_ref()
                .is_some_and(|(expected, _, _)| expected == text)
            {
                guard.take()
            } else {
                None
            }
        };
        if let Some((_, entered_tx, release_rx)) = matching_gate {
            let _ = entered_tx.send(());
            let _ = release_rx.await;
        }
        if let Some(override_res) = self.send_overrides.lock().unwrap().pop_front() {
            override_res?;
        }
        self.sent_messages
            .lock()
            .unwrap()
            .push((chat.clone(), text.to_owned()));
        self.message_notify.notify_waiters();
        Ok(())
    }

    async fn send_message_draft(
        &self,
        chat: &ChatKey,
        draft_id: i64,
        text: &str,
        can_stop: bool,
    ) -> Result<(), TelegramApiError> {
        self.draft_entered_count
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.draft_entered_notify.notify_waiters();
        let slow = self.slow_draft_behavior.lock().unwrap().clone();
        if let Some((delay, result)) = slow {
            tokio::time::sleep(delay).await;
            result?;
        }
        self.draft_timestamps
            .lock()
            .unwrap()
            .push(tokio::time::Instant::now());
        self.sent_drafts
            .lock()
            .unwrap()
            .push((chat.clone(), draft_id, text.to_owned(), can_stop));
        self.draft_notify.notify_waiters();
        Ok(())
    }

    async fn drain(&self) {
        let maybe_gate = self.drain_gate.lock().unwrap().take();
        if let Some((entered_tx, release_rx)) = maybe_gate {
            let _ = entered_tx.send(());
            let _ = release_rx.await;
        }
    }
}

fn private_text_update(update_id: i64, chat_id: i64, sender_id: i64, text: &str) -> TelegramUpdate {
    let raw: RawTelegramUpdate = serde_json::from_value(serde_json::json!({
        "update_id": update_id,
        "message": {
            "message_id": update_id,
            "from": {"id": sender_id},
            "chat": {"id": chat_id, "type": "private"},
            "text": text
        }
    }))
    .expect("构造私聊文本更新应成功");
    raw.into_domain()
}

fn stop_draft_update(update_id: i64, chat_id: i64, draft_id: i64) -> TelegramUpdate {
    let raw: RawTelegramUpdate = serde_json::from_value(serde_json::json!({
        "update_id": update_id,
        "stopped_message_generation": {
            "chat": {"id": chat_id, "type": "private"},
            "draft_id": draft_id
        }
    }))
    .expect("构造停止生成更新应成功");
    raw.into_domain()
}

/// 读取一次假 provider 收到的 HTTP 请求体字节。
async fn read_http_request(socket: &mut tokio::net::TcpStream) -> String {
    let mut headers = Vec::new();
    while !headers.ends_with(b"\r\n\r\n") {
        headers.push(socket.read_u8().await.expect("读取 HTTP 请求头应成功"));
    }
    let header_str = String::from_utf8(headers).expect("HTTP 头应为 UTF-8");
    let content_len: usize = header_str
        .lines()
        .find_map(|line| {
            line.to_ascii_lowercase()
                .strip_prefix("content-length:")
                .map(|v| v.trim().parse().expect("解析 Content-Length 应成功"))
        })
        .unwrap_or(0);
    let mut body = vec![0_u8; content_len];
    socket
        .read_exact(&mut body)
        .await
        .expect("读取 HTTP 请求体应成功");
    String::from_utf8(body).expect("请求体应为 UTF-8")
}

/// 向假 provider 连接写入完整 SSE 响应（包含隐藏推理块、正文块与可选用量）。
async fn write_sse_complete(
    socket: &mut tokio::net::TcpStream,
    reasoning: Option<&str>,
    content: &str,
    usage_tokens: Option<(u64, u64)>,
) {
    let mut payload = String::new();
    if let Some(r) = reasoning {
        let escaped_r = serde_json::to_string(r).unwrap();
        payload.push_str(&format!(
            "data: {{\"choices\":[{{\"delta\":{{\"reasoning_content\":{escaped_r}}},\"finish_reason\":null}}]}}\n\n"
        ));
    }
    let escaped_c = serde_json::to_string(content).unwrap();
    payload.push_str(&format!(
        "data: {{\"choices\":[{{\"delta\":{{\"content\":{escaped_c}}},\"finish_reason\":null}}]}}\n\n"
    ));
    if let Some((prompt, completion)) = usage_tokens {
        payload.push_str(&format!(
            "data: {{\"choices\":[{{\"delta\":{{}},\"finish_reason\":\"stop\"}}],\"usage\":{{\"prompt_tokens\":{prompt},\"completion_tokens\":{completion}}}}}\n\n"
        ));
    } else {
        payload.push_str("data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n");
    }
    payload.push_str("data: [DONE]\n\n");

    let header = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        payload.len()
    );
    socket.write_all(header.as_bytes()).await.unwrap();
    socket.write_all(payload.as_bytes()).await.unwrap();
    socket.flush().await.unwrap();
}

/// 测试夹具：启动真实 `serve_for_test_with_service`，并写好 `providers.toml`、agent profile、`im/config.toml` 与 `im/auth.json`。
struct ImTestHarness {
    _dir: tempfile::TempDir,
    config_dir: PathBuf,
    workspace: PathBuf,
    layout: StateLayout,
    service: Service,
    listener: TcpListener,
    daemon_stop_tx: mpsc::Sender<()>,
    daemon_task: tokio::task::JoinHandle<lattice_ipc::Result<ServeExit>>,
}

impl ImTestHarness {
    async fn start(allowed_senders: &[i64]) -> Self {
        let dir = tempfile::tempdir().expect("创建临时目录应成功");
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("绑定本地假模型端口应成功");
        let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());

        let home_dir = dir.path().to_path_buf();
        let config_dir = home_dir.join(".lattice");
        let agent_path = config_dir.join("agents/local-test/agent.toml");
        std::fs::create_dir_all(agent_path.parent().unwrap()).unwrap();
        std::fs::write(
            &agent_path,
            "schema_version = 1\n[agent]\nname = 'local-test'\nmodel = 'local/local-test'\n[prompt]\nsystem = 'test'\n",
        )
        .unwrap();
        std::fs::set_permissions(&config_dir, std::fs::Permissions::from_mode(0o700)).unwrap();

        let cfg_priv = PrivateDirectory::open_or_create(&config_dir).unwrap();
        cfg_priv
            .replace(
                OsStr::new("providers.toml"),
                format!(
                    "[providers.local]\nprotocol='chat-completions'\nbase_url='{endpoint}'\nauth='none'\n"
                )
                .as_bytes(),
            )
            .unwrap();

        let workspace_raw = home_dir.join("workspace");
        std::fs::create_dir_all(&workspace_raw).unwrap();
        let workspace = LocalWorkspaceRoot::from_path(&workspace_raw)
            .unwrap()
            .canonical_path()
            .to_path_buf();

        let im_dir = cfg_priv.child(OsStr::new("im"), true).unwrap().unwrap();
        let senders_str = allowed_senders
            .iter()
            .map(i64::to_string)
            .collect::<Vec<_>>()
            .join(", ");
        let im_config = format!(
            "[telegram]\nallowed_senders = [{senders_str}]\nworkspace = '{}'\nagent = 'local-test'\n",
            workspace.display()
        );
        im_dir
            .replace(OsStr::new("config.toml"), im_config.as_bytes())
            .unwrap();

        ImAuthStore::new(config_dir.clone())
            .set(TELEGRAM_PLATFORM, "999888:INTEGRATION_BOT_SECRET")
            .unwrap();

        let layout = StateLayout::in_state_root(&home_dir);
        let (daemon_stop_tx, daemon_stop_rx) = mpsc::channel(2);
        let (service_tx, service_rx) = oneshot::channel();
        let serving_layout = layout.clone();
        let daemon_task = tokio::spawn(async move {
            serve_for_test_with_service(
                serving_layout,
                Some(home_dir),
                Duration::from_secs(30),
                daemon_stop_rx,
                service_tx,
            )
            .await
        });

        let service = bounded(service_rx)
            .await
            .expect("应成功获取真实进程内 Service");

        Self {
            _dir: dir,
            config_dir,
            workspace,
            layout,
            service,
            listener,
            daemon_stop_tx,
            daemon_task,
        }
    }

    fn spawn_gateway(
        &self,
        io: Arc<FakeTelegramIo>,
    ) -> (
        oneshot::Sender<()>,
        tokio::task::JoinHandle<Result<(), crate::ImGatewayError>>,
    ) {
        let prepared =
            match prepare_im_gateway(&self.config_dir, self.layout.clone(), self.service.clone()) {
                ImBuildOutcome::Prepared(gw) => gw,
                other => panic!("测试网关构造必须返回 Prepared，实际: {other:?}"),
            };
        let (stop_tx, stop_rx) = oneshot::channel::<()>();
        let handle = tokio::spawn(async move {
            prepared
                .run_with_io(io, async move {
                    let _ = stop_rx.await;
                })
                .await
        });
        (stop_tx, handle)
    }

    fn spawn_gateway_with_probe(
        &self,
        io: Arc<FakeTelegramIo>,
    ) -> (
        oneshot::Sender<()>,
        tokio::task::JoinHandle<Result<(), crate::ImGatewayError>>,
        oneshot::Receiver<crate::gateway::GatewayDebugProbe>,
    ) {
        let prepared =
            match prepare_im_gateway(&self.config_dir, self.layout.clone(), self.service.clone()) {
                ImBuildOutcome::Prepared(gw) => gw,
                other => panic!("测试网关构造必须返回 Prepared，实际: {other:?}"),
            };
        let (stop_tx, stop_rx) = oneshot::channel::<()>();
        let (probe_tx, probe_rx) = oneshot::channel();
        let handle = tokio::spawn(async move {
            prepared
                .run_with_io_and_probe(
                    io,
                    async move {
                        let _ = stop_rx.await;
                    },
                    probe_tx,
                )
                .await
        });
        (stop_tx, handle, probe_rx)
    }

    async fn shutdown(self) {
        let _ = self.daemon_stop_tx.send(()).await;
        let exit = bounded(self.daemon_task)
            .await
            .expect("后台任务不应崩溃")
            .expect("后台正常退出不应报错");
        assert_eq!(exit.drain, DrainOutcome::Completed);
        drop(exit.instance);
    }
}

// 场景 1 & 11：普通消息完整闭环（configure(New) -> submit -> Accepted -> 不含隐藏推理的流式草稿 -> 最终回复 -> Delivered）
// 同时验证草稿带 can_stop=true 与 stopped_message_generation 停止按钮取消。
#[tokio::test]
async fn full_turn_lifecycle_streams_clean_draft_and_stop_button_cancels_active_turn() {
    let harness = ImTestHarness::start(&[ALLOWED_UID]).await;
    let io = Arc::new(FakeTelegramIo::default());
    let (gw_stop, gw_task) = harness.spawn_gateway(Arc::clone(&io));

    // 第 1 轮：带流式草稿闸门的正常完成轮次
    let (release_finish_tx, release_finish_rx) = oneshot::channel();
    let listener = &harness.listener;
    let model_round1 = async {
        let (mut sock, _) = bounded(listener.accept()).await.unwrap();
        let req_body = read_http_request(&mut sock).await;
        assert!(req_body.contains("你好世界"), "模型请求应包含用户消息文本");

        let chunk1 = "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"绝密思考内容\"},\"finish_reason\":null}]}\n\ndata: {\"choices\":[{\"delta\":{\"content\":\"流式草稿前半段\"},\"finish_reason\":null}]}\n\n";
        let chunk2 = "data: {\"choices\":[{\"delta\":{\"content\":\"与后半段\"},\"finish_reason\":null}]}\n\ndata: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n";
        let total_len = chunk1.len() + chunk2.len();
        let header = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {total_len}\r\nConnection: close\r\n\r\n"
        );
        sock.write_all(header.as_bytes()).await.unwrap();
        sock.write_all(chunk1.as_bytes()).await.unwrap();
        sock.flush().await.unwrap();

        bounded(release_finish_rx).await.unwrap();
        sock.write_all(chunk2.as_bytes()).await.unwrap();
        sock.flush().await.unwrap();
    };

    io.push_updates(vec![private_text_update(
        100,
        PRIVATE_CHAT_ID,
        ALLOWED_UID,
        "你好世界",
    )]);

    let (_, drafts) = tokio::join!(model_round1, async {
        let drafts = io.wait_draft_count(1).await;
        assert_eq!(drafts[0].2, "流式草稿前半段", "草稿只应包含助手纯文本");
        assert!(
            !drafts[0].2.contains("绝密思考内容"),
            "流式草稿严禁泄露隐藏推理内容"
        );
        assert!(drafts[0].3, "流式草稿必须设置 can_stop=true");
        assert_ne!(drafts[0].1, 0, "draft_id 必须为非零值");
        release_finish_tx.send(()).unwrap();
        drafts
    });

    let messages = io.wait_message_count(1).await;
    assert_eq!(messages[0].1, "流式草稿前半段与后半段");
    io.wait_offset(Some(101)).await;

    // 已结束轮次的旧 draft_id 收到停止按钮事件：仅确认更新推进 offset，不发多余报错
    io.push_updates(vec![stop_draft_update(101, PRIVATE_CHAT_ID, drafts[0].1)]);
    io.wait_offset(Some(102)).await;
    assert_eq!(
        io.sent_messages.lock().unwrap().len(),
        1,
        "已结束轮次的旧停止按钮事件不应触发多余回复"
    );

    // 第 2 轮：流式进行中点击草稿停止按钮触发取消
    io.push_updates(vec![private_text_update(
        102,
        PRIVATE_CHAT_ID,
        ALLOWED_UID,
        "请长篇回答",
    )]);

    let (mut sock2, _) = bounded(harness.listener.accept()).await.unwrap();
    let _ = read_http_request(&mut sock2).await;
    // 发送分块传输头与首段草稿并保持连接打开等待取消
    let chunk_head =
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n";
    let sse_draft =
        "data: {\"choices\":[{\"delta\":{\"content\":\"生成一半\"},\"finish_reason\":null}]}\n\n";
    sock2.write_all(chunk_head.as_bytes()).await.unwrap();
    sock2
        .write_all(format!("{:X}\r\n{sse_draft}\r\n", sse_draft.len()).as_bytes())
        .await
        .unwrap();
    sock2.flush().await.unwrap();

    let all_drafts = io.wait_draft_count(2).await;
    let active_draft_id = all_drafts.last().unwrap().1;
    io.push_updates(vec![stop_draft_update(
        103,
        PRIVATE_CHAT_ID,
        active_draft_id,
    )]);

    // 应收到取消回执与带取消说明的最终回复
    let msgs_after_stop = io.wait_message_count(3).await;
    let texts: Vec<&str> = msgs_after_stop[1..]
        .iter()
        .map(|(_, t)| t.as_str())
        .collect();
    assert!(
        texts.contains(&"已取消，等待这一轮结束"),
        "点击停止按钮应收到取消确认: {texts:?}"
    );
    assert!(
        texts.iter().any(|t| t.contains("［本轮已取消")),
        "被取消轮次的最终回复应附带取消结局说明: {texts:?}"
    );
    drop(sock2);

    let _ = gw_stop.send(());
    bounded(gw_task).await.unwrap().unwrap();

    // 核对台账两条普通消息均已转为 DeliveredTurn
    let ledger = Ledger::open_and_prune(
        harness.layout.clone(),
        Utc::now(),
        TELEGRAM_REDELIVERY_WINDOW,
    )
    .await
    .unwrap();
    assert!(matches!(
        ledger.inbound_record(&inbound_id_from_update_id(100)).await,
        Some(InboundRecord::DeliveredTurn { .. })
    ));
    assert!(matches!(
        ledger.inbound_record(&inbound_id_from_update_id(102)).await,
        Some(InboundRecord::DeliveredTurn { .. })
    ));

    harness.shutdown().await;
}

// 场景 2、3、8 & 10：
// - 重复 InboundId 不重复提交；同一 chat 连发两条只建一个会话；
// - /new 清除绑定并落账，重投同一 /new 更新不会清掉之后才建立的新绑定；
// - 无文本消息（含带 caption 的图片消息）、/help、/start（含参数）、未授权与群组消息不建会话不落账，
//   且在 sendMessage 被阻塞尚未发出时 offset 照常立即推进。
#[tokio::test]
async fn deduplicates_inbound_id_preserves_single_session_and_handles_new_and_non_ledger_events() {
    let harness = ImTestHarness::start(&[ALLOWED_UID]).await;
    let io = Arc::new(FakeTelegramIo::default());
    let (gw_stop, gw_task) = harness.spawn_gateway(Arc::clone(&io));
    let chat_key = chat_key_from_id(PRIVATE_CHAT_ID);

    // 1. 验证未授权、群组消息、带 caption 图片消息、/help、/start payload 在回复阻塞时仍立即确认推进 offset
    let unblock_send = io.install_send_gate();
    let group_update: RawTelegramUpdate = serde_json::from_value(serde_json::json!({
        "update_id": 200,
        "message": {
            "message_id": 1,
            "from": {"id": ALLOWED_UID},
            "chat": {"id": -100555, "type": "supergroup"},
            "text": "群聊消息"
        }
    }))
    .unwrap();
    let stranger_update = private_text_update(201, PRIVATE_CHAT_ID, OTHER_UID, "陌生人消息");
    let captioned_photo: RawTelegramUpdate = serde_json::from_value(serde_json::json!({
        "update_id": 202,
        "message": {
            "message_id": 2,
            "from": {"id": ALLOWED_UID},
            "chat": {"id": PRIVATE_CHAT_ID, "type": "private"},
            "photo": [{"file_id": "p1", "file_unique_id": "u1", "width": 100, "height": 100}],
            "caption": "图片说明不是 text"
        }
    }))
    .unwrap();
    let help_update = private_text_update(203, PRIVATE_CHAT_ID, ALLOWED_UID, "/help");
    let start_update =
        private_text_update(204, PRIVATE_CHAT_ID, ALLOWED_UID, "/start invite_token_xyz");

    io.push_updates(vec![
        group_update.into_domain(),
        stranger_update,
        captioned_photo.into_domain(),
        help_update,
        start_update,
    ]);

    // 即使 sendMessage 被闸门挡住尚未返回，全部 5 条不写台账事件的确认也必须使下一次轮询 offset 推进到 205
    io.wait_send_entered(1).await;
    io.wait_offset(Some(205)).await;
    let _ = unblock_send.send(());

    let initial_replies = io.wait_message_count(3).await;
    let reply_texts: Vec<&str> = initial_replies.iter().map(|(_, t)| t.as_str()).collect();
    assert!(reply_texts.contains(&NON_TEXT_MESSAGE_REPLY));
    assert_eq!(
        reply_texts.iter().filter(|&&t| t == HELP_REPLY).count(),
        2,
        "/help 与 /start payload 必须返回完全相同的命令说明，且未授权/群组消息不发回复"
    );

    // 2. 同一 chat 先发两条相同 InboundId（210）：在途去重只调一次模型；第一轮完成后再发 [重投 210, 新消息 211]：复用同一会话
    io.push_updates(vec![
        private_text_update(210, PRIVATE_CHAT_ID, ALLOWED_UID, "第一问"),
        private_text_update(210, PRIVATE_CHAT_ID, ALLOWED_UID, "第一问"),
    ]);

    let (mut s1, _) = bounded(harness.listener.accept()).await.unwrap();
    let req1 = read_http_request(&mut s1).await;
    assert!(req1.contains("第一问"));
    write_sse_complete(&mut s1, None, "第一答", Some((10, 5))).await;

    let msgs_after_one = io.wait_message_count(4).await;
    assert_eq!(msgs_after_one[3].1, "第一答");

    // 同时投递已落账的 210 与新消息 211（含表外斜杠文本 `/unknown`）：210 不重跑，211 复用同一会话历史
    io.push_updates(vec![
        private_text_update(210, PRIVATE_CHAT_ID, ALLOWED_UID, "第一问"),
        private_text_update(211, PRIVATE_CHAT_ID, ALLOWED_UID, "/unknown 视为普通第二问"),
    ]);

    let (mut s2, _) = bounded(harness.listener.accept()).await.unwrap();
    let req2 = read_http_request(&mut s2).await;
    assert!(
        req2.contains("第一答") && req2.contains("/unknown 视为普通第二问"),
        "同一 chat 第二条消息必须复用同一会话历史，且表外斜杠文本作为普通消息发送"
    );
    write_sse_complete(&mut s2, None, "第二答", Some((25, 8))).await;

    let msgs_after_two = io.wait_message_count(5).await;
    assert_eq!(msgs_after_two[4].1, "第二答");
    io.wait_offset(Some(212)).await;

    // 3. 在同一批次中流水线同时投递 [/new (220), 新会话首问 (221), 同批紧随第二问 (222)]：
    //    必须严格按到达顺序执行：220 先清掉旧绑定 -> 221 建立唯一新会话并进入在途执行 ->
    //    222 在 221 尚未结束前复用同一个新 SessionId 提交并收到 Busy（绝不额外新建第二个会话）！
    io.push_updates(vec![
        private_text_update(220, PRIVATE_CHAT_ID, ALLOWED_UID, "/new"),
        private_text_update(221, PRIVATE_CHAT_ID, ALLOWED_UID, "新会话首问"),
        private_text_update(222, PRIVATE_CHAT_ID, ALLOWED_UID, "同批紧随第二问"),
    ]);
    let (mut s3, _) = bounded(harness.listener.accept()).await.unwrap();
    let req3 = read_http_request(&mut s3).await;
    assert!(
        !req3.contains("第一答") && req3.contains("新会话首问"),
        "/new 与新消息同批到达时必须先清除旧绑定再以新会话提交 221"
    );
    // 在 221 仍挂在流式响应未完成时，/new 提示与 222 的 Busy 回复均已发出
    let mid_msgs = io.wait_message_count(7).await;
    let mid_slice: Vec<&str> = mid_msgs[5..7].iter().map(|(_, t)| t.as_str()).collect();
    assert!(
        mid_slice.contains(&"已清除当前会话绑定，下一条消息将新建会话")
            && mid_slice.contains(&"正在忙，可发 /cancel"),
        "同批紧随其后的第二条不同消息必须复用 221 创建的同一会话并收到 Busy: {mid_slice:?}"
    );
    write_sse_complete(&mut s3, None, "新会话首答", Some((12, 4))).await;
    let after_s3_msgs = io.wait_message_count(8).await;
    assert_eq!(after_s3_msgs[7].1, "新会话首答");

    // 核验 221 与 222 在台账中记录的是完全相同的 SessionId
    {
        let check_ledger = Ledger::open_and_prune(
            harness.layout.clone(),
            Utc::now(),
            TELEGRAM_REDELIVERY_WINDOW,
        )
        .await
        .unwrap();
        let sid_221 = match check_ledger
            .inbound_record(&inbound_id_from_update_id(221))
            .await
        {
            Some(
                InboundRecord::DeliveredTurn { session, .. }
                | InboundRecord::Accepted { session, .. },
            ) => session,
            other => panic!("221 应已绑定会话，实际: {other:?}"),
        };
        let sid_222 = match check_ledger
            .inbound_record(&inbound_id_from_update_id(222))
            .await
        {
            Some(
                InboundRecord::DeliveredTurn { session, .. }
                | InboundRecord::Registered { session, .. },
            ) => session,
            other => panic!("222 应已绑定会话，实际: {other:?}"),
        };
        assert_eq!(
            sid_221, sid_222,
            "未完成首轮前同批到达的两条新消息必须绑定同一个 SessionId"
        );
    }

    // 重投旧的 /new（update_id = 220），然后查询 /status 验证新绑定未被清除
    io.push_updates(vec![
        private_text_update(220, PRIVATE_CHAT_ID, ALLOWED_UID, "/new"),
        private_text_update(223, PRIVATE_CHAT_ID, ALLOWED_UID, "/status"),
    ]);
    let msgs_after_redeliver = io.wait_message_count(9).await;
    assert!(
        msgs_after_redeliver[8].1.contains("会话："),
        "重投已落账的 /new 绝不能清掉之后才建立的新绑定: {}",
        msgs_after_redeliver[8].1
    );

    let _ = gw_stop.send(());
    bounded(gw_task).await.unwrap().unwrap();

    // 重新挂载新实例（全新 OffsetTracker）并重投旧普通消息 210 与旧 /new（220）：验证跨实例台账去重同样不重跑、不清掉新绑定
    let io_remount = Arc::new(FakeTelegramIo::default());
    let (gw_stop_remount, gw_task_remount) = harness.spawn_gateway(Arc::clone(&io_remount));
    io_remount.push_updates(vec![
        private_text_update(210, PRIVATE_CHAT_ID, ALLOWED_UID, "第一问"),
        private_text_update(220, PRIVATE_CHAT_ID, ALLOWED_UID, "/new"),
        private_text_update(224, PRIVATE_CHAT_ID, ALLOWED_UID, "/status"),
    ]);
    let remount_msgs = io_remount.wait_message_count(1).await;
    assert!(
        remount_msgs[0].1.contains("会话："),
        "跨重启重投已落账的 /new 必须命中台账去重并保留新绑定: {}",
        remount_msgs[0].1
    );
    io_remount.wait_offset(Some(225)).await;
    let _ = gw_stop_remount.send(());
    bounded(gw_task_remount).await.unwrap().unwrap();

    let ledger = Ledger::open_and_prune(
        harness.layout.clone(),
        Utc::now(),
        TELEGRAM_REDELIVERY_WINDOW,
    )
    .await
    .unwrap();
    assert!(
        ledger.bound_session(&chat_key).await.is_some(),
        "重投旧 /new 后新会话绑定必须保留在台账中"
    );
    assert_eq!(
        ledger.inbound_record(&inbound_id_from_update_id(202)).await,
        None,
        "无文本消息不得写入台账"
    );
    assert_eq!(
        ledger.inbound_record(&inbound_id_from_update_id(203)).await,
        None,
        "/help 不得写入台账"
    );

    harness.shutdown().await;
}

// 场景 4 & 9：
// - 绑定的草稿会话不存在导致首次 submit 返回 NotFound 时，IM 用原登记 SessionId 重建并成功提交；
// - /status 与 /usage 端到端查询真实会话状态与四行用量摘要；
// - sendMessage 返回 403 平台拒收时不再重试并将台账推进为 Delivered。
#[tokio::test]
async fn rebuilds_cleaned_draft_session_with_same_id_and_serves_status_usage_and_403_rejection() {
    let harness = ImTestHarness::start(&[ALLOWED_UID]).await;
    let chat_key = chat_key_from_id(PRIVATE_CHAT_ID);
    let preset_session_id = new_session_id();

    // 预先在台账中写入一个后台库中不存在的绑定 SessionId（模拟草稿会话已被后台清理）
    {
        let ledger = Ledger::open_and_prune(
            harness.layout.clone(),
            Utc::now(),
            TELEGRAM_REDELIVERY_WINDOW,
        )
        .await
        .unwrap();
        let dummy_inbound = inbound_id_from_update_id(1);
        let dummy_turn = new_turn_id();
        ledger
            .register_turn(
                dummy_inbound.clone(),
                chat_key.clone(),
                preset_session_id,
                dummy_turn,
                true,
            )
            .await
            .unwrap();
        ledger
            .mark_delivered(&dummy_inbound, Utc::now())
            .await
            .unwrap();
    }

    let io = Arc::new(FakeTelegramIo::default());
    let (gw_stop, gw_task) = harness.spawn_gateway(Arc::clone(&io));

    // 草稿会话在后台不存在时，未重建前发 /status 与 /usage 应报告没有绑定的会话
    io.push_updates(vec![
        private_text_update(300, PRIVATE_CHAT_ID, ALLOWED_UID, "/status"),
        private_text_update(301, PRIVATE_CHAT_ID, ALLOWED_UID, "/usage"),
    ]);
    let pre_msgs = io.wait_message_count(2).await;
    assert_eq!(pre_msgs[0].1, "当前没有绑定的会话");
    assert_eq!(pre_msgs[1].1, "当前没有绑定的会话");

    // 发送普通消息：首次 submit 返回 NotFound，网关应使用同一个 preset_session_id 重建并成功提交
    io.push_updates(vec![private_text_update(
        302,
        PRIVATE_CHAT_ID,
        ALLOWED_UID,
        "重建草稿会话测试",
    )]);
    let (mut sock, _) = bounded(harness.listener.accept()).await.unwrap();
    let _ = read_http_request(&mut sock).await;
    write_sse_complete(&mut sock, None, "重建成功回复", Some((120, 30))).await;

    let post_msgs = io.wait_message_count(3).await;
    assert_eq!(post_msgs[2].1, "重建成功回复");

    // 查询真实后台确认重建会话使用的正是 preset_session_id
    let head = harness.service.head(preset_session_id).await.unwrap();
    assert_eq!(head.session_id, preset_session_id);

    // 端到端验证 /status 与 /usage 四行输出
    io.push_updates(vec![private_text_update(
        303,
        PRIVATE_CHAT_ID,
        ALLOWED_UID,
        "/status",
    )]);
    let status_msgs = io.wait_message_count(4).await;
    let status_reply = &status_msgs[3].1;
    assert!(status_reply.contains(&preset_session_id.to_string()));
    assert!(status_reply.contains("状态：空闲"));
    assert!(status_reply.contains("Agent：local-test"));

    io.push_updates(vec![private_text_update(
        304,
        PRIVATE_CHAT_ID,
        ALLOWED_UID,
        "/usage",
    )]);
    let query_msgs = io.wait_message_count(5).await;
    let usage_reply = &query_msgs[4].1;

    let usage_lines: Vec<&str> = usage_reply.lines().collect();
    assert_eq!(usage_lines.len(), 4, "/usage 端到端回复必须严格四行");
    assert_eq!(
        usage_lines[0],
        format!("会话：{preset_session_id}；当前路径：1 轮")
    );
    assert_eq!(usage_lines[1], "模型：local/local-test");
    assert!(usage_lines[2].starts_with("当前路径合计：输入 120；"));
    assert!(usage_lines[3].starts_with("全会话合计：输入 120；"));

    // 验证最终回复遇到 403 平台拒收时：不重试且台账记为 DeliveredTurn
    io.push_send_error(TelegramApiError::Rejected {
        status: 403,
        diagnostic: "用户拉黑了机器人".into(),
    });
    io.push_updates(vec![private_text_update(
        305,
        PRIVATE_CHAT_ID,
        ALLOWED_UID,
        "触发 403 拒收",
    )]);
    let (mut sock_403, _) = bounded(harness.listener.accept()).await.unwrap();
    let _ = read_http_request(&mut sock_403).await;
    write_sse_complete(&mut sock_403, None, "这条回复会被 403 拒收", None).await;

    // 轮询停止并检查台账：305 应转为 DeliveredTurn
    bounded(async {
        loop {
            let l = Ledger::open_and_prune(
                harness.layout.clone(),
                Utc::now(),
                TELEGRAM_REDELIVERY_WINDOW,
            )
            .await
            .unwrap();
            if matches!(
                l.inbound_record(&inbound_id_from_update_id(305)).await,
                Some(InboundRecord::DeliveredTurn { .. })
            ) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;

    let _ = gw_stop.send(());
    bounded(gw_task).await.unwrap().unwrap();
    harness.shutdown().await;
}

// 场景 5：崩溃恢复全分支 + 恢复不阻塞轮询 + 同 chat 多条 Registered 并行恢复（回复阻塞不卡后续判定） + 未决 InFlight 准入顺序屏障
#[tokio::test]
async fn recovers_all_ledger_phases_without_blocking_polling_and_orders_same_chat_after_registered()
{
    let harness = ImTestHarness::start(&[ALLOWED_UID, OTHER_UID]).await;
    let chat_a = chat_key_from_id(PRIVATE_CHAT_ID);
    let chat_b = chat_key_from_id(8002);
    let chat_c = chat_key_from_id(OTHER_UID);

    // 在真实后台创建两个会话：
    // - session_a 上提交一轮已结束的 turn_reg_accepted（台账故意停在 Registered，测试 Registered -> Accepted -> 最终回复）
    // - session_b 上提交一轮已结束的 turn_accepted（台账停在 Accepted，测试 Accepted 补发最终回复）
    // - 另写入一条后台不存在的 turn_reg_missing（台账停在 Registered，测试回复“该消息未被执行”）
    let session_a = new_session_id();
    let session_b = new_session_id();
    let session_c = new_session_id();
    for sid in [session_a, session_b, session_c] {
        harness
            .service
            .configure(ConfigureRequest {
                session: ConfigureSession::New(sid),
                workspace: harness.workspace.clone(),
                agent: Some("local-test".parse().unwrap()),
                model: None,
                effort: None,
            })
            .await
            .unwrap();
    }

    let turn_reg_accepted = new_turn_id();
    let submit_a = harness.service.submit(TurnRequest {
        turn_id: turn_reg_accepted,
        session: session_a,
        workspace: harness.workspace.clone(),
        input: "遗留已接受但台账停在 Registered".into(),
        expected_settings: None,
    });
    let model_a = async {
        let (mut sock, _) = bounded(harness.listener.accept()).await.unwrap();
        let _ = read_http_request(&mut sock).await;
        write_sse_complete(&mut sock, None, "恢复出的 Registered 回复", None).await;
    };
    let (turn_a_res, ()) = tokio::join!(submit_a, model_a);
    let mut handle_a = turn_a_res.unwrap();
    let _ = bounded(handle_a.outcome()).await.unwrap();

    let turn_accepted = new_turn_id();
    let submit_b = harness.service.submit(TurnRequest {
        turn_id: turn_accepted,
        session: session_b,
        workspace: harness.workspace.clone(),
        input: "遗留 Accepted 待补发".into(),
        expected_settings: None,
    });
    let model_b = async {
        let (mut sock, _) = bounded(harness.listener.accept()).await.unwrap();
        let _ = read_http_request(&mut sock).await;
        write_sse_complete(&mut sock, None, "补发的 Accepted 回复", None).await;
    };
    let (turn_b_res, ()) = tokio::join!(submit_b, model_b);
    let mut handle_b = turn_b_res.unwrap();
    let _ = bounded(handle_b.outcome()).await.unwrap();

    // 在 session_a 上再启动一轮挂起不结束的在途轮次 turn_hanging，模拟重新挂载时旧轮仍在后台运行！
    let turn_hanging = new_turn_id();
    let hanging_handle = harness
        .service
        .submit(TurnRequest {
            turn_id: turn_hanging,
            session: session_a,
            workspace: harness.workspace.clone(),
            input: "挂起中的长轮次".into(),
            expected_settings: None,
        })
        .await
        .unwrap();
    drop(hanging_handle); // 丢弃观察句柄不取消后台执行
    let (mut hanging_sock, _) = bounded(harness.listener.accept()).await.unwrap();
    let _ = read_http_request(&mut hanging_sock).await;
    hanging_sock
        .write_all(
            b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n",
        )
        .await
        .unwrap();
    hanging_sock.flush().await.unwrap();

    // 在 session_c 上挂起一轮真实处于准入途中（TurnLookup::InFlight）的轮次 turn_c_inflight！
    let (entered_c_accept, release_c_accept) =
        harness.service.arm_before_accept_for_test(session_c);
    let turn_c_inflight = new_turn_id();
    let svc_c = harness.service.clone();
    let ws_c = harness.workspace.clone();
    let inflight_submit_task = tokio::spawn(async move {
        svc_c
            .submit(TurnRequest {
                turn_id: turn_c_inflight,
                session: session_c,
                workspace: ws_c,
                input: "准入途中的遗留 Registered 轮次".into(),
                expected_settings: None,
            })
            .await
    });
    bounded(entered_c_accept)
        .await
        .expect("turn_c_inflight 应停在 before_accept 闸门");
    assert_eq!(
        harness.service.turn(turn_c_inflight).await.unwrap(),
        lattice_ipc::TurnLookup::InFlight
    );

    let turn_c_missing = new_turn_id();
    let id_c_missing = InboundId::from_platform_id("tg", "401");
    let id_reg_accepted = InboundId::from_platform_id("tg", "402");
    let id_accepted_done = InboundId::from_platform_id("tg", "403");
    let id_accepted_hanging = InboundId::from_platform_id("tg", "404");
    let id_c_inflight = InboundId::from_platform_id("tg", "405");

    {
        let ledger = Ledger::open_and_prune(
            harness.layout.clone(),
            Utc::now(),
            TELEGRAM_REDELIVERY_WINDOW,
        )
        .await
        .unwrap();
        // chat_c 同时拥有两条遗留 Registered：前一条 NotFound（故意卡住其回复发送），后一条 InFlight！
        ledger
            .register_turn(
                id_c_missing.clone(),
                chat_c.clone(),
                session_c,
                turn_c_missing,
                true,
            )
            .await
            .unwrap();
        ledger
            .register_turn(
                id_c_inflight.clone(),
                chat_c.clone(),
                session_c,
                turn_c_inflight,
                false,
            )
            .await
            .unwrap();

        ledger
            .register_turn(
                id_reg_accepted.clone(),
                chat_b.clone(),
                session_a,
                turn_reg_accepted,
                false,
            )
            .await
            .unwrap();
        ledger
            .register_turn(
                id_accepted_done.clone(),
                chat_b.clone(),
                session_b,
                turn_accepted,
                true,
            )
            .await
            .unwrap();
        ledger.mark_accepted(&id_accepted_done).await.unwrap();

        ledger
            .register_turn(
                id_accepted_hanging.clone(),
                chat_a.clone(),
                session_a,
                turn_hanging,
                true,
            )
            .await
            .unwrap();
        ledger.mark_accepted(&id_accepted_hanging).await.unwrap();
    }

    let io = Arc::new(FakeTelegramIo::default());
    // 拦截并卡住第一条 Registered (id_c_missing) 的“该消息未被执行”发送，证明它不会阻塞第二条 Registered 查询或屏障放行！
    let (not_exec_send_entered, release_not_exec_send) =
        io.install_matching_send_gate("该消息未被执行");
    let (gw_stop, gw_task) = harness.spawn_gateway(Arc::clone(&io));

    // 1. 网关不等待恢复结束，立即开始 getUpdates 轮询；且 id_c_missing 已判定 NotFound 并进入被拦截的回复发送
    let _ = io.wait_poll_count(1).await;
    bounded(not_exec_send_entered)
        .await
        .expect("id_c_missing 应立即完成 NotFound 判定并进入异步回复发送");

    // 已完成的另外两条遗留回复（Registered->Accepted 与 Accepted）正常发出
    let initial_recovered = io.wait_message_count(2).await;
    let initial_texts: Vec<&str> = initial_recovered.iter().map(|(_, t)| t.as_str()).collect();
    assert!(initial_texts.contains(&"恢复出的 Registered 回复"));
    assert!(initial_texts.contains(&"补发的 Accepted 回复"));

    // 2. 在 id_c_inflight 仍处于 TurnLookup::InFlight 且 id_c_missing 回复仍被网络阻塞期间：
    //    - 向 chat_a 发 /cancel (410)：立即生效取消 turn_hanging，不被任何恢复阻塞！
    //    - 向 chat_c 同时发 [普通新消息 (411), /status (412)]：/status 立即返回，而普通消息 411 严格排在未决 Registered 屏障之后！
    io.push_updates(vec![
        private_text_update(410, PRIVATE_CHAT_ID, ALLOWED_UID, "/cancel"),
        private_text_update(411, OTHER_UID, OTHER_UID, "排在未决 Registered 后的新消息"),
        private_text_update(412, OTHER_UID, OTHER_UID, "/status"),
    ]);

    // 等待 /cancel 的两条消息（确认 + 取消结局）与 chat_c 的 /status 回复（共 2 + 3 = 5 条）
    let mid_msgs = io.wait_message_count(5).await;
    let mid_texts: Vec<&str> = mid_msgs[2..].iter().map(|(_, t)| t.as_str()).collect();
    assert!(
        mid_texts.contains(&"已取消，等待这一轮结束"),
        "未决 Registered 恢复期间 /cancel 必须立即可用: {mid_texts:?}"
    );
    assert!(
        mid_texts.iter().any(|t| t.contains("［本轮已取消")),
        "turn_hanging 取消后应补发取消结局: {mid_texts:?}"
    );
    assert!(
        mid_texts.iter().any(|t| t.contains("会话：")),
        "chat_c 仍有未决 Registered 时 /status 必须立即响应: {mid_texts:?}"
    );
    drop(hanging_sock);

    // 核验此时 411 仍被 chat_c 的未决 InFlight 屏障挡在队列中（尚未写入台账，offset 也未越过 411）！
    {
        let snap = Ledger::open_and_prune(
            harness.layout.clone(),
            Utc::now(),
            TELEGRAM_REDELIVERY_WINDOW,
        )
        .await
        .unwrap();
        assert_eq!(
            snap.inbound_record(&inbound_id_from_update_id(411)).await,
            None,
            "同 chat 存在未决 InFlight Registered 时，新普通消息 411 绝不能提前落账或提交"
        );
    }

    // 3. 注意此时仍不放行 id_c_missing 的“该消息未被执行”发送闸门！
    //    仅放行 turn_c_inflight 的接受事务，验证第二条 Registered 并行得出 Accepted 后，chat_c 屏障立即放行 411！
    release_c_accept.send(()).unwrap();
    let _ = bounded(inflight_submit_task).await.unwrap().unwrap();

    // 先处理 turn_c_inflight 的模型请求并保持流式挂起，使紧随放行的 411 立即命中 Busy 并推进 offset 到 413！
    let (mut sock_c_inflight, _) = bounded(harness.listener.accept()).await.unwrap();
    let req_c1 = read_http_request(&mut sock_c_inflight).await;
    assert!(req_c1.contains("准入途中的遗留 Registered 轮次"));
    sock_c_inflight
        .write_all(
            b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n",
        )
        .await
        .unwrap();
    sock_c_inflight.flush().await.unwrap();

    // 尽管前一条 NotFound 的回复发送仍被阻塞，411 已被放行落账、确认 offset(413) 并收到 Busy！
    io.wait_offset(Some(413)).await;
    let after_unblock_msgs = io.wait_message_count(6).await;
    assert_eq!(
        after_unblock_msgs[5].1, "正在忙，可发 /cancel",
        "前一条 Registered 回复发送卡住不得阻塞后一条 Registered 恢复或屏障释放"
    );

    // 完成 turn_c_inflight 流式响应并放行第一条 NotFound 回复发送
    let done_chunk = "data: {\"choices\":[{\"delta\":{\"content\":\"InFlight 恢复完成\"},\"finish_reason\":\"stop\"}]}\n\n";
    let done_eof = "data: [DONE]\n\n";
    for frame in [done_chunk, done_eof] {
        sock_c_inflight
            .write_all(format!("{:X}\r\n{frame}\r\n", frame.len()).as_bytes())
            .await
            .unwrap();
    }
    sock_c_inflight.write_all(b"0\r\n\r\n").await.unwrap();
    sock_c_inflight.flush().await.unwrap();
    release_not_exec_send.send(()).unwrap();

    let final_msgs = io.wait_message_count(8).await;
    let tail_texts: Vec<&str> = final_msgs[6..].iter().map(|(_, t)| t.as_str()).collect();
    assert!(tail_texts.contains(&"该消息未被执行"));
    assert!(tail_texts.contains(&"InFlight 恢复完成"));

    let _ = gw_stop.send(());
    bounded(gw_task).await.unwrap().unwrap();
    harness.shutdown().await;
}

// 场景 5(c) & 7(d)：当会话库暂不可用（如 sessions.redb 权限被破坏无法打开）时，
// - 遗留 Registered 查询报错时保留原 Registered 相位不误标为未被执行；
// - /cancel 查询会话头报错时回复“取消结果未知，可发 /status 查看”。
#[tokio::test]
async fn preserves_registered_phase_and_reports_unknown_cancel_when_store_unavailable() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().to_path_buf();
    let config_dir = home.join(".lattice");
    std::fs::create_dir_all(&config_dir).unwrap();
    std::fs::set_permissions(&config_dir, std::fs::Permissions::from_mode(0o700)).unwrap();

    let workspace = home.join("ws");
    std::fs::create_dir_all(&workspace).unwrap();
    let canonical_ws = std::fs::canonicalize(&workspace).unwrap();

    let priv_cfg = PrivateDirectory::open_or_create(&config_dir).unwrap();
    let im_dir = priv_cfg.child(OsStr::new("im"), true).unwrap().unwrap();
    im_dir
        .replace(
            OsStr::new("config.toml"),
            format!(
                "[telegram]\nallowed_senders = [{ALLOWED_UID}]\nworkspace = '{}'\nagent = 'a'\n",
                canonical_ws.display()
            )
            .as_bytes(),
        )
        .unwrap();
    ImAuthStore::new(config_dir.clone())
        .set(TELEGRAM_PLATFORM, "tok-store-err")
        .unwrap();

    let layout = StateLayout::in_state_root(&home);
    // 故意在 lattice.redb 位置放入一个目录，令后台打开会话库返回存储错误而非 NotFound
    std::fs::create_dir_all(layout.lattice_root().join("lattice.redb")).unwrap();
    std::fs::set_permissions(
        layout.lattice_root(),
        std::fs::Permissions::from_mode(0o700),
    )
    .unwrap();

    let (daemon_stop_tx, daemon_stop_rx) = mpsc::channel(2);
    let (svc_tx, svc_rx) = oneshot::channel();
    let serving_layout = layout.clone();
    let daemon_task = tokio::spawn(async move {
        serve_for_test_with_service(
            serving_layout,
            Some(home),
            Duration::from_secs(10),
            daemon_stop_rx,
            svc_tx,
        )
        .await
    });
    let service = bounded(svc_rx).await.unwrap();

    let chat_key = chat_key_from_id(PRIVATE_CHAT_ID);
    let sid = new_session_id();
    let tid = new_turn_id();
    let reg_id = inbound_id_from_update_id(500);
    {
        let ledger = Ledger::open_and_prune(layout.clone(), Utc::now(), TELEGRAM_REDELIVERY_WINDOW)
            .await
            .unwrap();
        ledger
            .register_turn(reg_id.clone(), chat_key, sid, tid, true)
            .await
            .unwrap();
    }

    // 确认此时 service.turn 返回非 NotFound 错误
    let turn_err = service.turn(tid).await.expect_err("损坏库路径应报错");
    assert_ne!(turn_err.kind, LocalErrorKind::NotFound);

    let io = Arc::new(FakeTelegramIo::default());
    let prepared = match prepare_im_gateway(&config_dir, layout.clone(), service.clone()) {
        ImBuildOutcome::Prepared(p) => p,
        other => panic!("应返回 Prepared: {other:?}"),
    };
    let (gw_stop_tx, gw_stop_rx) = oneshot::channel::<()>();
    let io_clone = Arc::clone(&io);
    let gw_task = tokio::spawn(async move {
        prepared
            .run_with_io(io_clone, async move {
                let _ = gw_stop_rx.await;
            })
            .await
    });

    // 发送 /cancel：后台 head(sid) 报错，应回复“取消结果未知，可发 /status 查看”
    io.push_updates(vec![private_text_update(
        501,
        PRIVATE_CHAT_ID,
        ALLOWED_UID,
        "/cancel",
    )]);
    let msgs = io.wait_message_count(1).await;
    assert_eq!(msgs[0].1, "取消结果未知，可发 /status 查看");

    let _ = gw_stop_tx.send(());
    bounded(gw_task).await.unwrap().unwrap();

    // 核对遗留 Registered 条目未因暂时性库错误被篡改，仍保留为 Registered！
    let ledger = Ledger::open_and_prune(layout, Utc::now(), TELEGRAM_REDELIVERY_WINDOW)
        .await
        .unwrap();
    assert!(
        matches!(
            ledger.inbound_record(&reg_id).await,
            Some(InboundRecord::Registered { .. })
        ),
        "查询报错时 Registered 条目必须保留原相位以便后续重试"
    );

    let _ = daemon_stop_tx.send(()).await;
    let exit = bounded(daemon_task).await.unwrap().unwrap();
    drop(exit.instance);
}

// 场景 6 & 7：
// - IM 在途轮次期间再发普通消息立即收到 Busy（“正在忙，可发 /cancel”）；
// - /new 后旧会话轮次继续推送草稿，此时 /cancel 只看当前绑定会话（回复“没有进行中的轮”），旧会话用草稿停止按钮取消；
// - 外部 IPC LocalClient 占用同一绑定会话时，IM 发普通消息收到 Busy，发 /cancel 成功取消 LocalClient 的轮次；
// - 已结束轮次直接调用取消辅助返回“这一轮已结束或不在执行中”。
#[tokio::test]
async fn handles_busy_and_cancel_across_im_new_session_and_external_local_client() {
    let harness = ImTestHarness::start(&[ALLOWED_UID]).await;
    let io = Arc::new(FakeTelegramIo::default());
    let (gw_stop, gw_task) = harness.spawn_gateway(Arc::clone(&io));
    let chat_key = chat_key_from_id(PRIVATE_CHAT_ID);

    // 1. 无绑定会话时 /cancel -> “没有进行中的轮”
    io.push_updates(vec![private_text_update(
        600,
        PRIVATE_CHAT_ID,
        ALLOWED_UID,
        "/cancel",
    )]);
    assert_eq!(io.wait_message_count(1).await[0].1, "没有进行中的轮");

    // 2. 发起第 1 轮并挂在流式输出中，推一条草稿
    io.push_updates(vec![private_text_update(
        601,
        PRIVATE_CHAT_ID,
        ALLOWED_UID,
        "占用会话的慢请求",
    )]);
    let (mut sock_old, _) = bounded(harness.listener.accept()).await.unwrap();
    let _ = read_http_request(&mut sock_old).await;
    let sse_draft = "data: {\"choices\":[{\"delta\":{\"content\":\"旧会话流式草稿\"},\"finish_reason\":null}]}\n\n";
    sock_old
        .write_all(
            b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n",
        )
        .await
        .unwrap();
    sock_old
        .write_all(format!("{:X}\r\n{sse_draft}\r\n", sse_draft.len()).as_bytes())
        .await
        .unwrap();
    sock_old.flush().await.unwrap();

    let drafts = io.wait_draft_count(1).await;
    let old_draft_id = drafts[0].1;

    // 3. 在途期间同一 chat 再发普通消息 -> 立即收到 Busy：“正在忙，可发 /cancel”
    io.push_updates(vec![private_text_update(
        602,
        PRIVATE_CHAT_ID,
        ALLOWED_UID,
        "第二条插队消息",
    )]);
    assert_eq!(
        io.wait_message_count(2).await[1].1,
        "正在忙，可发 /cancel",
        "观察与回复在准入后并行执行，第二条普通消息必须立即收到后台 Busy"
    );

    // 4. 发送 /new 解绑当前 chat，随后发送 /cancel：当前无绑定，不应误取消旧会话轮次！
    io.push_updates(vec![private_text_update(
        603,
        PRIVATE_CHAT_ID,
        ALLOWED_UID,
        "/new",
    )]);
    let _ = io.wait_message_count(3).await;

    io.push_updates(vec![private_text_update(
        604,
        PRIVATE_CHAT_ID,
        ALLOWED_UID,
        "/cancel",
    )]);
    assert_eq!(
        io.wait_message_count(4).await[3].1,
        "没有进行中的轮",
        "/new 后 /cancel 只作用于当前绑定会话，不得取消已解绑的旧会话轮次"
    );

    // 用旧草稿的停止按钮精确取消旧会话轮次！
    io.push_updates(vec![stop_draft_update(605, PRIVATE_CHAT_ID, old_draft_id)]);
    let msgs_after_old_stop = io.wait_message_count(6).await;
    let old_stop_texts: Vec<&str> = msgs_after_old_stop[4..]
        .iter()
        .map(|(_, t)| t.as_str())
        .collect();
    assert!(old_stop_texts.contains(&"已取消，等待这一轮结束"));
    assert!(old_stop_texts.iter().any(|t| t.contains("［本轮已取消")));
    drop(sock_old);

    // 5. 建立新会话完成一轮，再用外部 IPC LocalClient 在同一绑定会话上发起一轮并挂起
    io.push_updates(vec![private_text_update(
        606,
        PRIVATE_CHAT_ID,
        ALLOWED_UID,
        "建立共享会话",
    )]);
    let (mut sock_init, _) = bounded(harness.listener.accept()).await.unwrap();
    let _ = read_http_request(&mut sock_init).await;
    write_sse_complete(&mut sock_init, None, "共享会话已就绪", None).await;
    let _ = io.wait_message_count(7).await;

    let bound_sid = Ledger::open_and_prune(
        harness.layout.clone(),
        Utc::now(),
        TELEGRAM_REDELIVERY_WINDOW,
    )
    .await
    .unwrap()
    .bound_session(&chat_key)
    .await
    .expect("应存在已绑定会话");

    // 对已结束轮次核对“这一轮已结束或不在执行中”分支
    let finished_tid = new_turn_id();
    assert_eq!(
        cancel_turn_and_format(&harness.service, finished_tid).await,
        "这一轮已结束或不在执行中"
    );

    // 外部 IPC LocalClient 在 bound_sid 上发起一轮并挂起
    let ipc_client = LocalClient::new(harness.layout.clone());
    let ipc_tid = new_turn_id();
    let mut local_turn = ipc_client.start_turn(TurnRequest {
        turn_id: ipc_tid,
        session: bound_sid,
        workspace: harness.workspace.clone(),
        input: "来自 CLI/TUI 的并发请求".into(),
        expected_settings: None,
    });

    let (mut sock_ipc, _) = bounded(harness.listener.accept()).await.unwrap();
    let _ = read_http_request(&mut sock_ipc).await;
    sock_ipc
        .write_all(
            b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n",
        )
        .await
        .unwrap();
    sock_ipc.flush().await.unwrap();

    // 等待 LocalTurn 确认已被后台接受
    bounded(async {
        while let Some(upd) = local_turn.next().await {
            if matches!(
                upd,
                LocalUpdate::Submit {
                    update: SubmissionUpdate::Receipt(ref r),
                } if matches!(r.result, SubmissionResult::Accepted { .. })
            ) {
                break;
            }
        }
    })
    .await;

    // IM 在该会话上发普通消息 -> 收到 Busy
    io.push_updates(vec![private_text_update(
        607,
        PRIVATE_CHAT_ID,
        ALLOWED_UID,
        "IM 撞上外部 IPC Busy",
    )]);
    assert_eq!(
        io.wait_message_count(8).await[7].1,
        "正在忙，可发 /cancel",
        "外部 LocalClient 正在执行同一会话时 IM 必须收到 Busy"
    );

    // IM 发 /cancel -> 成功取消外部 LocalClient 发起的轮次！
    io.push_updates(vec![private_text_update(
        608,
        PRIVATE_CHAT_ID,
        ALLOWED_UID,
        "/cancel",
    )]);
    assert_eq!(
        io.wait_message_count(9).await[8].1,
        "已取消，等待这一轮结束",
        "IM /cancel 必须能取消外部 LocalClient 在绑定会话上发起的轮次"
    );

    bounded(async { while local_turn.next().await.is_some() {} }).await;
    let report = bounded(local_turn.detach()).await;
    assert_eq!(
        report.summary.map(|s| s.outcome),
        Some(PublicTurnOutcome::Cancelled),
        "外部 LocalClient 应观察到该轮已被取消"
    );
    drop(sock_ipc);

    let _ = gw_stop.send(());
    bounded(gw_task).await.unwrap().unwrap();
    harness.shutdown().await;
}

// 场景 10：进程内 submit 返回带 recovery 的确认不明错误（ConfirmationUnknown）时保留同一 TurnId 找回，
// 以及后台进入停机（Stopping）时保留 Registered 相位并随 service.stopping() 自动收尾。
#[tokio::test]
async fn recovers_live_confirmation_unknown_without_duplicate_turn_and_stops_cleanly_on_backend_stopping()
 {
    let harness = ImTestHarness::start(&[ALLOWED_UID]).await;
    let chat_key = chat_key_from_id(PRIVATE_CHAT_ID);
    let preset_sid = new_session_id();

    harness
        .service
        .configure(ConfigureRequest {
            session: ConfigureSession::New(preset_sid),
            workspace: harness.workspace.clone(),
            agent: Some("local-test".parse().unwrap()),
            model: None,
            effort: None,
        })
        .await
        .unwrap();
    {
        let ledger = Ledger::open_and_prune(
            harness.layout.clone(),
            Utc::now(),
            TELEGRAM_REDELIVERY_WINDOW,
        )
        .await
        .unwrap();
        let seed_id = InboundId::from_platform_id("tg", "seed-bind");
        ledger
            .register_turn(
                seed_id.clone(),
                chat_key.clone(),
                preset_sid,
                new_turn_id(),
                true,
            )
            .await
            .unwrap();
        ledger.mark_delivered(&seed_id, Utc::now()).await.unwrap();
    }

    // 1. 注入接受后读回失败，使真实 Service::submit 返回带 recovery 的确认不明错误
    let (hit_read_fail, release_read_fail) = harness
        .service
        .arm_accepted_read_failure_for_test(preset_sid);

    let io = Arc::new(FakeTelegramIo::default());
    let (gw_stop, gw_task) = harness.spawn_gateway(Arc::clone(&io));

    io.push_updates(vec![private_text_update(
        700,
        PRIVATE_CHAT_ID,
        ALLOWED_UID,
        "触发确认不明的真实请求",
    )]);

    bounded(hit_read_fail)
        .await
        .expect("Service::submit 应进入接受后读回故障注入点");
    let registered_tid = {
        let snap = Ledger::open_and_prune(
            harness.layout.clone(),
            Utc::now(),
            TELEGRAM_REDELIVERY_WINDOW,
        )
        .await
        .unwrap();
        match snap.inbound_record(&inbound_id_from_update_id(700)).await {
            Some(InboundRecord::Registered { turn, .. }) => turn,
            other => panic!("确认不明解除前条目必须处于 Registered，实际: {other:?}"),
        }
    };
    release_read_fail.send(()).unwrap();

    // 真实后台已完成接受事务并在读回故障后收束终态；网关不得新建第二个 TurnId，而是按原 TurnId 找回 Accepted 并补发终态回复！
    let msgs = io.wait_message_count(1).await;
    assert!(
        msgs[0].1.contains("［本轮执行失败"),
        "确认不明找回后应补发该轮终态回复: {}",
        msgs[0].1
    );

    // 等待后台恢复任务完成 mark_delivered 落盘，验证 TurnId 未变且推进为 DeliveredTurn
    bounded(async {
        loop {
            let snap = Ledger::open_and_prune(
                harness.layout.clone(),
                Utc::now(),
                TELEGRAM_REDELIVERY_WINDOW,
            )
            .await
            .unwrap();
            if let Some(InboundRecord::DeliveredTurn { turn, .. }) =
                snap.inbound_record(&inbound_id_from_update_id(700)).await
            {
                assert_eq!(
                    turn, registered_tid,
                    "找回后落盘的 TurnId 必须保持原登记值不变"
                );
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await;

    // 2. 验证真实 Service::submit 返回 Stopping 时：条目保留在 Registered（不误标 Delivered、不回错误消息），且网关随 service.stopping() 自动收尾退出
    let (ledger_replace_entered, release_ledger_replace, _replace_finished) =
        crate::ledger::arm_replace_gate_for_test(harness.layout.lattice_root().to_path_buf());
    io.push_updates(vec![private_text_update(
        701,
        PRIVATE_CHAT_ID,
        ALLOWED_UID,
        "停机途中提交的新消息",
    )]);
    bounded(ledger_replace_entered)
        .await
        .expect("第二条消息应进入 register_turn 阻塞点");

    // 在 register_turn 落盘途中触发后台停机，随后放行落盘，使紧接其后的 Service::submit 确定命中 Stopping
    let _ = harness.daemon_stop_tx.send(()).await;
    let mut stopping_wait = harness.service.stopping();
    bounded(stopping_wait.wait()).await;
    release_ledger_replace.send(()).unwrap();

    bounded(gw_task)
        .await
        .expect("网关任务不应 panic")
        .expect("响应后台停机应返回 Ok(())");
    drop(gw_stop);
    let _ = bounded(harness.daemon_task)
        .await
        .expect("后台任务不应 panic")
        .expect("后台应正常停机");

    let final_snap = Ledger::open_and_prune(
        harness.layout.clone(),
        Utc::now(),
        TELEGRAM_REDELIVERY_WINDOW,
    )
    .await
    .unwrap();
    assert!(
        matches!(
            final_snap
                .inbound_record(&inbound_id_from_update_id(701))
                .await,
            Some(InboundRecord::Registered { .. })
        ),
        "Service::submit 返回 Stopping 时必须保留 Registered 相位供下次启动恢复"
    );
    assert_eq!(
        io.sent_messages.lock().unwrap().len(),
        1,
        "Stopping 不得被当作常规错误向用户发送拒绝消息"
    );
}

// 场景 11：真实 Service + InProcessTurn 虚拟时间流式草稿节流/保活/截尾，以及重新挂载后旧实例停止按钮（含 draft_id=1）不误取消新轮次
#[tokio::test(start_paused = true)]
async fn isolates_draft_ids_across_remounts_and_streams_throttled_keepalive_drafts_from_real_service()
 {
    let harness = ImTestHarness::start(&[ALLOWED_UID]).await;
    let io1 = Arc::new(FakeTelegramIo::default());
    let (gw_stop1, gw_task1) = harness.spawn_gateway(Arc::clone(&io1));

    io1.push_updates(vec![private_text_update(
        800,
        PRIVATE_CHAT_ID,
        ALLOWED_UID,
        "测试真实流式草稿节流与保活",
    )]);

    let (mut sock1, _) = bounded(harness.listener.accept()).await.unwrap();
    let _ = read_http_request(&mut sock1).await;
    sock1
        .write_all(
            b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n",
        )
        .await
        .unwrap();

    // 首段增量立即触发第一条草稿
    let chunk1 = "data: {\"choices\":[{\"delta\":{\"content\":\"首段实时文本\"},\"finish_reason\":null}]}\n\n";
    sock1
        .write_all(format!("{:X}\r\n{chunk1}\r\n", chunk1.len()).as_bytes())
        .await
        .unwrap();
    sock1.flush().await.unwrap();

    let drafts1 = io1.wait_draft_count(1).await;
    let inst1_draft_id = drafts1[0].1;
    assert_eq!(drafts1[0].2, "首段实时文本");
    assert!(drafts1[0].3);
    let t_first_draft = io1.draft_timestamps.lock().unwrap()[0];

    // 1 秒节流窗口内连续推送两段 SSE 增量（总长度超过 4096 字符），验证合并且仅保留末尾 4096 字符
    let chunk2 =
        "data: {\"choices\":[{\"delta\":{\"content\":\"中间增量\"},\"finish_reason\":null}]}\n\n";
    let tail_4096 = "末".repeat(4096);
    let chunk3 = format!(
        "data: {{\"choices\":[{{\"delta\":{{\"content\":\"{tail_4096}\"}},\"finish_reason\":null}}]}}\n\n"
    );
    for frame in [chunk2, chunk3.as_str()] {
        sock1
            .write_all(format!("{:X}\r\n{frame}\r\n", frame.len()).as_bytes())
            .await
            .unwrap();
    }
    sock1.flush().await.unwrap();

    // 先给环回 TCP 读取留出若干让步轮次（虚拟时间仅推进 50ms，仍在 1 秒节流窗口内）
    for _ in 0..20 {
        tokio::task::yield_now().await;
    }
    tokio::time::advance(Duration::from_millis(50)).await;
    for _ in 0..20 {
        tokio::task::yield_now().await;
    }
    assert_eq!(
        io1.sent_drafts.lock().unwrap().len(),
        1,
        "1 秒节流窗口内不得提前发送第二条草稿"
    );

    // 虚拟时间补足至满 1 秒，触发合并后的第二条末尾 4096 字草稿
    tokio::time::advance(Duration::from_millis(950)).await;
    let drafts2 = io1.wait_draft_count(2).await;
    assert_eq!(drafts2[1].2.chars().count(), 4096);
    assert_eq!(drafts2[1].2, tail_4096);
    let t_second_draft = io1.draft_timestamps.lock().unwrap()[1];
    assert!(
        t_second_draft.duration_since(t_first_draft) >= Duration::from_secs(1),
        "同一 chat 两份草稿间隔不得小于 1 秒"
    );

    // 无新 SSE 增量，推进虚拟时间 25 秒触发保活草稿刷新
    tokio::time::advance(Duration::from_secs(25)).await;
    let drafts3 = io1.wait_draft_count(3).await;
    assert_eq!(drafts3[2].1, inst1_draft_id);

    // 结束第 1 轮并关闭第 1 个网关实例（模拟 IM 重新挂载）
    let done_frame =
        "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n";
    sock1
        .write_all(format!("{:X}\r\n{done_frame}\r\n0\r\n\r\n", done_frame.len()).as_bytes())
        .await
        .unwrap();
    sock1.flush().await.unwrap();
    let _ = io1.wait_message_count(1).await;
    let _ = gw_stop1.send(());
    bounded(gw_task1).await.unwrap().unwrap();

    // 重新挂载第 2 个网关实例并发起新轮次
    let io2 = Arc::new(FakeTelegramIo::default());
    let (gw_stop2, gw_task2) = harness.spawn_gateway(Arc::clone(&io2));
    io2.push_updates(vec![private_text_update(
        810,
        PRIVATE_CHAT_ID,
        ALLOWED_UID,
        "重挂后新实例轮次",
    )]);

    let (mut sock2, _) = bounded(harness.listener.accept()).await.unwrap();
    let _ = read_http_request(&mut sock2).await;
    let c_new =
        "data: {\"choices\":[{\"delta\":{\"content\":\"新实例草稿\"},\"finish_reason\":null}]}\n\n";
    sock2
        .write_all(
            b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n",
        )
        .await
        .unwrap();
    sock2
        .write_all(format!("{:X}\r\n{c_new}\r\n", c_new.len()).as_bytes())
        .await
        .unwrap();
    sock2.flush().await.unwrap();

    let inst2_drafts = io2.wait_draft_count(1).await;
    let inst2_draft_id = inst2_drafts[0].1;
    assert_ne!(
        inst2_draft_id, inst1_draft_id,
        "重新挂载的新实例必须分配独立的随机草稿编号"
    );

    // 投递迟到的旧实例停止按钮（含固定起点 draft_id=1 与旧实例真实 inst1_draft_id）：只确认更新，绝不取消新轮次！
    io2.push_updates(vec![
        stop_draft_update(811, PRIVATE_CHAT_ID, 1),
        stop_draft_update(812, PRIVATE_CHAT_ID, inst1_draft_id),
    ]);
    io2.wait_offset(Some(813)).await;

    // 新轮次照常完成输出，未被误取消
    sock2
        .write_all(format!("{:X}\r\n{done_frame}\r\n0\r\n\r\n", done_frame.len()).as_bytes())
        .await
        .unwrap();
    sock2.flush().await.unwrap();
    let final_msgs = io2.wait_message_count(1).await;
    assert_eq!(
        final_msgs[0].1, "新实例草稿",
        "跨实例旧停止按钮绝不能误取消新实例正在执行的轮次"
    );

    let _ = gw_stop2.send(());
    bounded(gw_task2).await.unwrap().unwrap();
    harness.shutdown().await;
}

// 场景 12a：getUpdates 遇到 HTTP 429 时完整遵守超过 30 秒的服务端 retry_after，不被本地退避上限截断
#[tokio::test(start_paused = true)]
async fn poll_updates_honors_unclamped_429_retry_after() {
    let harness = ImTestHarness::start(&[ALLOWED_UID]).await;
    let io = Arc::new(FakeTelegramIo::default());

    io.push_poll_error(TelegramApiError::RateLimited {
        retry_after: Some(Duration::from_secs(90)),
        diagnostic: "429 Too Many Requests".into(),
    });

    let (gw_stop, gw_task) = harness.spawn_gateway(Arc::clone(&io));
    let _ = io.wait_poll_count(1).await;
    let t_poll_0 = io.poll_timestamps.lock().unwrap()[0];

    tokio::time::advance(Duration::from_secs(89)).await;
    tokio::task::yield_now().await;
    assert_eq!(
        io.observed_offsets.lock().unwrap().len(),
        1,
        "getUpdates 429 retry_after=90s 未届满前不得提前重试"
    );

    tokio::time::advance(Duration::from_secs(1)).await;
    let _ = io.wait_poll_count(2).await;
    let t_poll_1 = io.poll_timestamps.lock().unwrap()[1];
    assert_eq!(
        t_poll_1.duration_since(t_poll_0),
        Duration::from_secs(90),
        "getUpdates 429 必须精确遵守服务端 90s 等待要求"
    );

    let _ = gw_stop.send(());
    bounded(gw_task).await.unwrap().unwrap();
    harness.shutdown().await;
}

// 场景 12b：停机等待在途台账阻塞写入与收发适配器 drain 完成、以及子任务 panic 向宿主传播
#[tokio::test]
async fn waits_for_inflight_ledger_and_io_drain_on_stop_and_propagates_subtask_panic() {
    let harness = ImTestHarness::start(&[ALLOWED_UID]).await;
    let io = Arc::new(FakeTelegramIo::default());
    let (gw_stop, mut gw_task) = harness.spawn_gateway(Arc::clone(&io));
    let _ = io.wait_poll_count(1).await;

    // 武装台账阻塞替换闸门与 TelegramIo::drain 闸门，验证 stop 必须等待二者全部完成后 run 才返回
    let (ledger_replace_entered, release_ledger_replace, _replace_finished) =
        crate::ledger::arm_replace_gate_for_test(harness.layout.lattice_root().to_path_buf());
    let (io_drain_entered, release_io_drain) = io.install_drain_gate();

    io.push_updates(vec![private_text_update(
        900,
        PRIVATE_CHAT_ID,
        ALLOWED_UID,
        "停机前正在写入台账的消息",
    )]);

    bounded(ledger_replace_entered)
        .await
        .expect("入站消息应进入台账阻塞写入替换点");

    // 立即发出网关停止信号：台账阻塞写入尚未放行前，gw_task 绝不能提前返回！
    let _ = gw_stop.send(());
    tokio::task::yield_now().await;
    assert!(
        !gw_task.is_finished(),
        "在途台账阻塞写入完成前 PreparedImGateway::run 不得返回"
    );

    // 放行台账阻塞写入（并处理随后的模型连接），随后应卡在 io.drain() 闸门处
    release_ledger_replace.send(()).unwrap();
    let (mut sock, _) = bounded(harness.listener.accept()).await.unwrap();
    let _ = read_http_request(&mut sock).await;
    write_sse_complete(&mut sock, None, "停机收尾回复", None).await;

    bounded(io_drain_entered)
        .await
        .expect("子任务收尾后应进入 TelegramIo::drain 等待");
    tokio::task::yield_now().await;
    assert!(
        !gw_task.is_finished(),
        "TelegramIo::drain 未完成前 PreparedImGateway::run 不得返回"
    );

    release_io_drain.send(()).unwrap();
    bounded(&mut gw_task)
        .await
        .expect("网关任务不应 panic")
        .expect("全部阻塞任务完成后应正常返回 Ok(())");

    // 验证停机前在途写入的台账记录确实落盘保留，未丢失
    let reopened = Ledger::open_and_prune(
        harness.layout.clone(),
        Utc::now(),
        TELEGRAM_REDELIVERY_WINDOW,
    )
    .await
    .unwrap();
    assert!(
        reopened
            .inbound_record(&inbound_id_from_update_id(900))
            .await
            .is_some(),
        "停机等待收尾后在途写入的台账条目必须已持久落盘"
    );

    // 3. 验证内部轮询或子任务发生 panic 时，run 收束并向宿主传播 panic（不挂死）
    let panic_io = Arc::new(FakeTelegramIo::default());
    panic_io.arm_panic_on_next_poll();
    let (_panic_stop, panic_gw_task) = harness.spawn_gateway(panic_io);
    let join_err = bounded(panic_gw_task)
        .await
        .expect_err("内部轮询任务 panic 必须传播给宿主 JoinHandle");
    assert!(
        join_err.is_panic(),
        "宿主等待组件任务时必须捕获到 panic 以便执行故障隔离"
    );

    harness.shutdown().await;
}

// 场景 13：宿主强制 abort 旧实例时，滞留阻塞线程池中的旧台账写入受发布门闩拦截，绝不覆盖新实例数据且不残留临时文件
#[tokio::test]
async fn aborted_gateway_cannot_overwrite_new_instance_ledger_via_inflight_blocking_replace() {
    let harness = ImTestHarness::start(&[ALLOWED_UID]).await;
    let io1 = Arc::new(FakeTelegramIo::default());
    let (_gw_stop1, gw_task1) = harness.spawn_gateway(Arc::clone(&io1));
    let _ = io1.wait_poll_count(1).await;

    let (ledger_replace_entered, release_ledger_replace, replace_finished) =
        crate::ledger::arm_replace_gate_for_test(harness.layout.lattice_root().to_path_buf());
    io1.push_updates(vec![private_text_update(
        1000,
        PRIVATE_CHAT_ID,
        ALLOWED_UID,
        "旧实例阻塞在途写入的消息",
    )]);
    bounded(ledger_replace_entered)
        .await
        .expect("旧实例应进入台账阻塞写入闸门");

    // 模拟收尾超时后宿主强制中止旧实例任务：此时旧实例的 spawn_blocking 仍停在闸门内
    gw_task1.abort();
    let abort_err = bounded(gw_task1)
        .await
        .expect_err("强制 abort 的旧实例 JoinHandle 应返回取消错误");
    assert!(abort_err.is_cancelled());

    // 立即挂载新实例并正常提交、完成一条新消息落账
    let io2 = Arc::new(FakeTelegramIo::default());
    let (gw_stop2, gw_task2) = harness.spawn_gateway(Arc::clone(&io2));
    io2.push_updates(vec![private_text_update(
        1001,
        PRIVATE_CHAT_ID,
        ALLOWED_UID,
        "新实例正常处理的消息",
    )]);
    let (mut sock2, _) = bounded(harness.listener.accept()).await.unwrap();
    let _ = read_http_request(&mut sock2).await;
    write_sse_complete(&mut sock2, None, "新实例最终回复", None).await;
    let msgs2 = io2.wait_message_count(1).await;
    assert_eq!(msgs2[0].1, "新实例最终回复");

    // 等待新实例完成 mark_delivered 落盘
    bounded(async {
        loop {
            let snap = Ledger::open_and_prune(
                harness.layout.clone(),
                Utc::now(),
                TELEGRAM_REDELIVERY_WINDOW,
            )
            .await
            .unwrap();
            if matches!(
                snap.inbound_record(&inbound_id_from_update_id(1001)).await,
                Some(InboundRecord::DeliveredTurn { .. })
            ) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await;

    // 现在放行旧实例滞留的 spawn_blocking 线程，并显式等待该阻塞任务执行完毕！发布门闩已随旧实例丢弃而封闭，必须拦截 renameat
    release_ledger_replace.send(()).unwrap();
    bounded(replace_finished)
        .await
        .expect("滞留的旧实例阻塞写入线程应在门闩拦截并清理临时文件后正常退出");

    let _ = gw_stop2.send(());
    bounded(gw_task2).await.unwrap().unwrap();

    let final_snap = Ledger::open_and_prune(
        harness.layout.clone(),
        Utc::now(),
        TELEGRAM_REDELIVERY_WINDOW,
    )
    .await
    .unwrap();
    assert!(
        matches!(
            final_snap
                .inbound_record(&inbound_id_from_update_id(1001))
                .await,
            Some(InboundRecord::DeliveredTurn { .. })
        ),
        "滞后的旧实例阻塞写入绝不能覆盖新实例已提交的台账条目"
    );
    assert_eq!(
        final_snap
            .inbound_record(&inbound_id_from_update_id(1000))
            .await,
        None,
        "已退场旧实例的未发布快照不得被写入台账"
    );

    let im_state_dir = harness.layout.lattice_root().join("im");
    let entries: Vec<String> = std::fs::read_dir(&im_state_dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        entries,
        vec!["ledger".to_string()],
        "被门闩拦截的临时文件必须自动清理，不得遗留 .tmp 文件"
    );

    harness.shutdown().await;
}

// 场景 14：台账写入临时失败时保持单对话队首退避重试，同批后续消息绝不越过未落账的 /new 乱序执行
#[tokio::test]
async fn transient_ledger_write_error_keeps_queue_head_and_preserves_fifo_arrival_order() {
    let harness = ImTestHarness::start(&[ALLOWED_UID]).await;
    let chat = chat_key_from_id(PRIVATE_CHAT_ID);
    let io = Arc::new(FakeTelegramIo::default());
    let (gw_stop, gw_task) = harness.spawn_gateway(Arc::clone(&io));

    // 1. 先跑一轮建立旧会话绑定
    io.push_updates(vec![private_text_update(
        1100,
        PRIVATE_CHAT_ID,
        ALLOWED_UID,
        "建立旧会话绑定",
    )]);
    let (mut sock0, _) = bounded(harness.listener.accept()).await.unwrap();
    let _ = read_http_request(&mut sock0).await;
    write_sse_complete(&mut sock0, None, "旧会话回复", None).await;
    let _ = io.wait_message_count(1).await;

    // 显式等待首轮消息 1100 的 mark_delivered 落盘完成，排除与后续故障注入的并发竞争
    let old_sid = bounded(async {
        loop {
            let snap = Ledger::open_and_prune(
                harness.layout.clone(),
                Utc::now(),
                TELEGRAM_REDELIVERY_WINDOW,
            )
            .await
            .unwrap();
            if matches!(
                snap.inbound_record(&inbound_id_from_update_id(1100)).await,
                Some(InboundRecord::DeliveredTurn { .. })
            ) {
                break snap
                    .bound_session(&chat)
                    .await
                    .expect("应已建立初始会话绑定");
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await;

    // 2. 定向针对 /new (1101) 注入单次临时台账写入故障，并在同一批次送入 [/new, 新消息]：
    //    /new 首次落账失败时必须守住队首重试成功后再放行新消息，确保新消息进入新会话而非旧会话！
    let fault_hit_rx = crate::ledger::arm_transient_write_error_for_test(
        harness.layout.lattice_root().to_path_buf(),
        inbound_id_from_update_id(1101),
    );
    io.push_updates(vec![
        private_text_update(1101, PRIVATE_CHAT_ID, ALLOWED_UID, "/new"),
        private_text_update(1102, PRIVATE_CHAT_ID, ALLOWED_UID, "重置后的首条消息"),
    ]);
    bounded(fault_hit_rx)
        .await
        .expect("单次台账临时写入故障必须精确命中 /new (1101) 的首次落账尝试");

    let (mut sock1, _) = bounded(harness.listener.accept()).await.unwrap();
    let req1 = read_http_request(&mut sock1).await;
    assert!(
        !req1.contains("建立旧会话绑定"),
        "新消息必须在 /new 重试落账清除绑定之后才执行，请求上下文不得携带旧会话历史"
    );
    write_sse_complete(&mut sock1, None, "新会话回复", None).await;

    let msgs = io.wait_message_count(3).await;
    let texts: Vec<&str> = msgs[1..].iter().map(|(_, t)| t.as_str()).collect();
    assert!(
        texts.contains(&"已清除当前会话绑定，下一条消息将新建会话")
            && texts.contains(&"新会话回复"),
        "应收到 /new 确认与新会话回复: {texts:?}"
    );

    let _ = gw_stop.send(());
    bounded(gw_task).await.unwrap().unwrap();

    let final_snap = Ledger::open_and_prune(
        harness.layout.clone(),
        Utc::now(),
        TELEGRAM_REDELIVERY_WINDOW,
    )
    .await
    .unwrap();
    let new_sid = final_snap
        .bound_session(&chat)
        .await
        .expect("应绑定到新建会话");
    assert_ne!(
        new_sid, old_sid,
        "/new 首次落账失败后必须重试成功并为后续消息新建独立 SessionId"
    );

    harness.shutdown().await;
}

// 场景 15：JoinSet 内的单对话准入子任务持有在途守卫与排队元素时发生 panic，自动清理守卫与队列、排空收发器并向宿主传播 panic，随后重挂立即可用
#[tokio::test]
async fn chat_admission_subtask_panic_cleans_guards_drains_io_propagates_panic_and_allows_remount()
{
    let harness = ImTestHarness::start(&[ALLOWED_UID]).await;
    let io1 = Arc::new(FakeTelegramIo::default());
    let (_gw_stop1, mut gw_task1, probe_rx) = harness.spawn_gateway_with_probe(Arc::clone(&io1));
    let probe = bounded(probe_rx).await.expect("应获取网关内部状态探针");
    let _ = io1.wait_poll_count(1).await;

    crate::gateway::arm_chat_worker_panic_for_test(inbound_id_from_update_id(1200));
    let (io_drain_entered, release_io_drain) = io1.install_drain_gate();

    // 同批送入两条同 chat 消息：1200 被弹出进入准入子任务触发 panic，1201 留在单对话队列且均持有 InFlightGuard
    io1.push_updates(vec![
        private_text_update(1200, PRIVATE_CHAT_ID, ALLOWED_UID, "触发准入子任务 panic"),
        private_text_update(1201, PRIVATE_CHAT_ID, ALLOWED_UID, "同队排队的第二条消息"),
    ]);

    bounded(io_drain_entered)
        .await
        .expect("JoinSet 子任务 panic 后网关应中断轮询并进入 TelegramIo::drain 收尾");
    assert!(
        !gw_task1.is_finished(),
        "TelegramIo::drain 完成前组件任务不得提前结束"
    );
    release_io_drain.send(()).unwrap();

    let join_err = bounded(&mut gw_task1)
        .await
        .expect_err("JoinSet 准入子任务 panic 必须传播给宿主");
    assert!(join_err.is_panic());
    assert_eq!(
        probe.in_flight_len(),
        0,
        "准入子任务 panic 展开后队首与队列中所有元素的 InFlightGuard 必须全部清除"
    );
    assert_eq!(
        probe.chat_queue_entry_count(),
        0,
        "准入子任务 panic 展开后 ChatWorkerGuard 必须清空并移除对话准入队列条目"
    );

    // 模拟宿主捕获故障后重新挂载新实例，重投上述两条消息应完整正常执行
    let io2 = Arc::new(FakeTelegramIo::default());
    let (gw_stop2, gw_task2) = harness.spawn_gateway(Arc::clone(&io2));
    io2.push_updates(vec![
        private_text_update(1200, PRIVATE_CHAT_ID, ALLOWED_UID, "重挂后执行第一条"),
        private_text_update(1201, PRIVATE_CHAT_ID, ALLOWED_UID, "重挂后执行第二条"),
    ]);

    let (mut s1, _) = bounded(harness.listener.accept()).await.unwrap();
    let _ = read_http_request(&mut s1).await;
    write_sse_complete(&mut s1, None, "重挂第一条回复", None).await;
    let msgs = io2.wait_message_count(2).await;
    assert_eq!(msgs.len(), 2, "重挂后首条完成与次条 Busy/执行均应正常响应");

    let _ = gw_stop2.send(());
    bounded(gw_task2).await.unwrap().unwrap();
    harness.shutdown().await;
}

// 场景 16：准入工作子任务在首次 poll 前即随宿主强制 abort 被丢弃时，同步清空单对话队列与在途守卫且无引用成环泄漏
#[tokio::test]
async fn abort_before_chat_worker_first_poll_cleans_queue_and_drops_runtime_without_cycle() {
    let harness = ImTestHarness::start(&[ALLOWED_UID]).await;
    let io1 = Arc::new(FakeTelegramIo::default());
    let (_gw_stop1, gw_task1, probe_rx) = harness.spawn_gateway_with_probe(Arc::clone(&io1));
    let probe = bounded(probe_rx).await.expect("应获取网关内部状态探针");
    let _ = io1.wait_poll_count(1).await;

    // 挂起主监督循环对 spawn_rx 的接收，使入队派生出的准入工作 future 停留在未首次 poll 的状态
    probe.set_hold_spawn_rx(true);
    io1.push_updates(vec![
        private_text_update(
            1300,
            PRIVATE_CHAT_ID,
            ALLOWED_UID,
            "首次 poll 前被取消的首条消息",
        ),
        private_text_update(
            1301,
            PRIVATE_CHAT_ID,
            ALLOWED_UID,
            "同对话队列排队的次条消息",
        ),
    ]);

    bounded(async {
        loop {
            if probe.in_flight_len() == 2 && probe.chat_queue_entry_count() == 1 {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await;
    assert!(
        !probe.was_worker_ever_polled(),
        "挂起接收期间准入工作 future 必须尚未执行首次 poll"
    );

    // 在准入工作 future 从未被 poll 的时刻由宿主强制 abort 整个网关实例
    gw_task1.abort();
    let cancel_err = bounded(gw_task1)
        .await
        .expect_err("强制 abort 网关任务应返回取消错误");
    assert!(cancel_err.is_cancelled());

    assert!(
        !probe.was_worker_ever_polled(),
        "被丢弃前准入工作 future 确认从未被 poll"
    );
    assert_eq!(
        probe.in_flight_len(),
        0,
        "首次 poll 前被取消丢弃时，队列全部元素的 InFlightGuard 必须随 ChatWorkerGuard 一并释放"
    );
    assert_eq!(
        probe.chat_queue_entry_count(),
        0,
        "首次 poll 前被取消丢弃时，ChatWorkerGuard 必须清空并移除对话准入队列条目"
    );

    bounded(async {
        while !probe.is_runtime_dropped() {
            tokio::task::yield_now().await;
        }
    })
    .await;
    assert!(
        probe.is_runtime_dropped(),
        "旧实例强制取消后 GatewayRuntime 强引用必须归零，不得因队列或守卫成环而泄漏凭据与 Service"
    );

    // 重新挂载新实例后，原未落账消息重投可正常完成端到端回复
    let io2 = Arc::new(FakeTelegramIo::default());
    let (gw_stop2, gw_task2) = harness.spawn_gateway(Arc::clone(&io2));
    io2.push_updates(vec![private_text_update(
        1300,
        PRIVATE_CHAT_ID,
        ALLOWED_UID,
        "重挂后重新投递的消息",
    )]);
    let (mut sock2, _) = bounded(harness.listener.accept()).await.unwrap();
    let _ = read_http_request(&mut sock2).await;
    write_sse_complete(&mut sock2, None, "重挂正常回复", None).await;
    let msgs2 = io2.wait_message_count(1).await;
    assert_eq!(msgs2[0].1, "重挂正常回复");

    let _ = gw_stop2.send(());
    bounded(gw_task2).await.unwrap().unwrap();
    harness.shutdown().await;
}

// 场景 17：前序普通消息排队挡住 offset 时，后续已确认的 /help、/cancel 被重投不得重复执行或误取消后来启动的新轮次（ADR §10）
#[tokio::test]
async fn acknowledged_commands_redelivered_behind_blocked_head_are_not_reexecuted() {
    let harness = ImTestHarness::start(&[ALLOWED_UID, OTHER_UID]).await;
    let io = Arc::new(FakeTelegramIo::default());
    let (gw_stop, gw_task) = harness.spawn_gateway(Arc::clone(&io));

    // 1. 先让 chat_a 建立稳定会话绑定 session_a
    io.push_updates(vec![private_text_update(
        1398,
        PRIVATE_CHAT_ID,
        ALLOWED_UID,
        "建立初始会话",
    )]);
    let (mut sock_init, _) = bounded(harness.listener.accept()).await.unwrap();
    let _ = read_http_request(&mut sock_init).await;
    write_sse_complete(&mut sock_init, None, "初始完成", None).await;
    let init_msgs = io.wait_message_count(1).await;
    assert_eq!(init_msgs[0].1, "初始完成");
    io.wait_offset(Some(1399)).await;

    let chat_a = chat_key_from_id(PRIVATE_CHAT_ID);
    let session_a = {
        let snap = Ledger::open_and_prune(
            harness.layout.clone(),
            Utc::now(),
            TELEGRAM_REDELIVERY_WINDOW,
        )
        .await
        .unwrap();
        snap.bound_session(&chat_a)
            .await
            .expect("chat_a 应已绑定 session_a")
    };

    // 2. 在 session_a 上武装 before_accept 闸门，使 1399 停在准入途中（占住 chat_a 的单对话准入队列），
    //    随后到达的普通消息 1400 便真实排在 chat_a 队列中尚未落账确认，从而将 offset 卡在 1400！
    let (entered_1399, release_1399) = harness.service.arm_before_accept_for_test(session_a);
    io.push_updates(vec![private_text_update(
        1399,
        PRIVATE_CHAT_ID,
        ALLOWED_UID,
        "准入途中占住队列的消息",
    )]);
    bounded(entered_1399)
        .await
        .expect("1399 应停在 before_accept 闸门");
    io.wait_offset(Some(1400)).await;

    // 3. 投递 [1400（chat_a 普通消息，排队未确认挡住 offset）, 1401（chat_a /help）, 1402（chat_b /cancel）]
    io.push_updates(vec![
        private_text_update(
            1400,
            PRIVATE_CHAT_ID,
            ALLOWED_UID,
            "排队挡住 offset 的普通消息",
        ),
        private_text_update(1401, PRIVATE_CHAT_ID, ALLOWED_UID, "/help"),
        private_text_update(1402, OTHER_UID, OTHER_UID, "/cancel"),
    ]);

    let after_cmds = io.wait_message_count(3).await;
    assert_eq!(
        after_cmds.iter().filter(|(_, t)| t == HELP_REPLY).count(),
        1,
        "首次 /help 应回复 1 次"
    );
    assert!(
        after_cmds.iter().any(|(_, t)| t == "没有进行中的轮"),
        "chat_b 尚无轮次时首次 /cancel 应回复没有进行中的轮: {after_cmds:?}"
    );
    assert_eq!(
        io.offsets.lock().unwrap().next_offset(),
        Some(1400),
        "1400 仍排队未确认时，offset 必须停在 1400"
    );

    // 4. chat_b 随后启动一轮真正的在途长轮次（1403），保持流式挂起
    io.push_updates(vec![private_text_update(
        1403,
        OTHER_UID,
        OTHER_UID,
        "chat_b 启动新轮次",
    )]);
    let (mut sock_b, _) = bounded(harness.listener.accept()).await.unwrap();
    let _ = read_http_request(&mut sock_b).await;
    sock_b
        .write_all(
            b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n",
        )
        .await
        .unwrap();
    let b_chunk = "data: {\"choices\":[{\"delta\":{\"content\":\"chat_b 流式输出中\"},\"finish_reason\":null}]}\n\n";
    sock_b
        .write_all(format!("{:X}\r\n{b_chunk}\r\n", b_chunk.len()).as_bytes())
        .await
        .unwrap();
    sock_b.flush().await.unwrap();
    let _ = io.wait_draft_count(1).await;

    // 5. 因 1400 仍挡住 offset，Telegram 连续两轮重投 [1400, 1401(/help), 1402(chat_b /cancel), 1403]，并在第二轮末尾附带 1404(chat_b /status)
    io.push_updates(vec![
        private_text_update(
            1400,
            PRIVATE_CHAT_ID,
            ALLOWED_UID,
            "排队挡住 offset 的普通消息",
        ),
        private_text_update(1401, PRIVATE_CHAT_ID, ALLOWED_UID, "/help"),
        private_text_update(1402, OTHER_UID, OTHER_UID, "/cancel"),
        private_text_update(1403, OTHER_UID, OTHER_UID, "chat_b 启动新轮次"),
    ]);
    io.push_updates(vec![
        private_text_update(
            1400,
            PRIVATE_CHAT_ID,
            ALLOWED_UID,
            "排队挡住 offset 的普通消息",
        ),
        private_text_update(1401, PRIVATE_CHAT_ID, ALLOWED_UID, "/help"),
        private_text_update(1402, OTHER_UID, OTHER_UID, "/cancel"),
        private_text_update(1403, OTHER_UID, OTHER_UID, "chat_b 启动新轮次"),
        private_text_update(1404, OTHER_UID, OTHER_UID, "/status"),
    ]);

    // 若 1401 或 1402 被重复执行，在 1404 之前就会多出额外的 /help 或 /cancel 回复，且 chat_b 的新轮次会被误取消！
    let after_redeliveries = io.wait_message_count(4).await;
    assert_eq!(
        after_redeliveries
            .iter()
            .filter(|(_, t)| t == HELP_REPLY)
            .count(),
        1,
        "连续重投两次后，已确认的 /help 仍只应回复 1 次（不得回复 3 次）: {after_redeliveries:?}"
    );
    assert!(
        after_redeliveries[3].1.contains("执行中（轮次 "),
        "旧 /cancel (1402) 重投绝不能取消后来启动的轮次 1403，/status 必须显示仍在执行中: {}",
        after_redeliveries[3].1
    );

    // 6. 放行 1399：1399 进入流式执行，排队的 1400 随即命中 Busy 并确认，offset 一次性推进到 1405
    release_1399.send(()).unwrap();
    let (mut sock_1399, _) = bounded(harness.listener.accept()).await.unwrap();
    let _ = read_http_request(&mut sock_1399).await;
    io.wait_offset(Some(1405)).await;

    write_sse_complete(&mut sock_1399, None, "1399 最终回复", None).await;
    let done_frame =
        "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n";
    sock_b
        .write_all(format!("{:X}\r\n{done_frame}\r\n0\r\n\r\n", done_frame.len()).as_bytes())
        .await
        .unwrap();
    sock_b.flush().await.unwrap();

    let final_msgs = io.wait_message_count(7).await;
    assert_eq!(
        final_msgs.iter().filter(|(_, t)| t == HELP_REPLY).count(),
        1,
        "全流程结束时 /help 总回复数必须严格为 1"
    );
    let final_texts: Vec<&str> = final_msgs.iter().map(|(_, t)| t.as_str()).collect();
    assert!(final_texts.contains(&"正在忙，可发 /cancel"));
    assert!(final_texts.contains(&"1399 最终回复"));
    assert!(final_texts.contains(&"chat_b 流式输出中"));

    let _ = gw_stop.send(());
    bounded(gw_task).await.unwrap().unwrap();
    harness.shutdown().await;
}

// 场景 18：使用真实 Service 模拟 30 秒慢草稿超时（超过 25 秒保活间隔），验证草稿循环绝不陷入紧循环且轮次结束立即发送最终回复（ADR §13）
#[tokio::test(start_paused = true)]
async fn slow_30s_draft_requests_never_livelock_or_block_final_reply_delivery() {
    let harness = ImTestHarness::start(&[ALLOWED_UID]).await;
    let io = Arc::new(FakeTelegramIo::default());
    io.set_slow_draft_behavior(
        Duration::from_secs(30),
        Err(TelegramApiError::Retryable {
            diagnostic: "模拟 30 秒 sendMessageDraft 请求超时".into(),
        }),
    );
    let (gw_stop, gw_task) = harness.spawn_gateway(Arc::clone(&io));

    io.push_updates(vec![private_text_update(
        1500,
        PRIVATE_CHAT_ID,
        ALLOWED_UID,
        "验证 30 秒慢草稿不阻塞最终回复",
    )]);

    let (mut sock1, _) = tokio::time::timeout(Duration::from_secs(120), harness.listener.accept())
        .await
        .unwrap()
        .unwrap();
    let _ = read_http_request(&mut sock1).await;
    sock1
        .write_all(
            b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n",
        )
        .await
        .unwrap();
    let chunk1 = "data: {\"choices\":[{\"delta\":{\"content\":\"慢草稿下的完整回复\"},\"finish_reason\":null}]}\n\n";
    sock1
        .write_all(format!("{:X}\r\n{chunk1}\r\n", chunk1.len()).as_bytes())
        .await
        .unwrap();
    sock1.flush().await.unwrap();

    let sse_ping = ": keepalive\n\n";
    let ping_wire = format!("{:X}\r\n{sse_ping}\r\n", sse_ping.len());

    // 1. 首次草稿请求已发出并将耗时 30 秒（超过 25 秒保活间隔）
    io.wait_draft_entered(1).await;

    // 2. 分段推进虚拟时间 30 秒（每 15 秒向模型流写入 SSE 注释帧防止模型侧空闲超时），让第 1 次草稿超时返回，
    //    再额外推进 5 秒（共 35 秒）：
    //    修复前：因发送前记录时间（now=30s >= 0+25s），返回瞬间立即紧循环发起第 2 次 30s 草稿且永不检查终态；
    //    修复后：以返回时刻为保活基准（下一次保活在 30s+25s=55s），在 35s 时绝不会紧循环重发第 2 次草稿！
    for _ in 0..2 {
        tokio::time::advance(Duration::from_secs(15)).await;
        sock1.write_all(ping_wire.as_bytes()).await.unwrap();
        sock1.flush().await.unwrap();
        for _ in 0..20 {
            tokio::task::yield_now().await;
        }
    }
    tokio::time::advance(Duration::from_secs(5)).await;
    for _ in 0..20 {
        tokio::task::yield_now().await;
    }
    assert_eq!(
        io.draft_entered_count(),
        1,
        "30 秒草稿超时返回后不得立即紧循环重发，应等待返回后满 25 秒保活周期"
    );

    // 3. 再推进 20 秒到达 55 秒保活点，触发第 2 次 30 秒慢草稿请求进入在途状态
    tokio::time::advance(Duration::from_secs(10)).await;
    sock1.write_all(ping_wire.as_bytes()).await.unwrap();
    sock1.flush().await.unwrap();
    tokio::time::advance(Duration::from_secs(10)).await;
    io.wait_draft_entered(2).await;

    // 4. 在第 2 次 30 秒草稿请求刚进行了 2 秒（尚余 28 秒真实在途阻塞）时，恢复真实时钟并完成模型流：
    //    若草稿循环未持续监听终态，此处将被阻塞整整 28 秒真实时间！
    tokio::time::advance(Duration::from_secs(2)).await;
    for _ in 0..10 {
        tokio::task::yield_now().await;
    }
    tokio::time::resume();
    let wall_finish_start = std::time::Instant::now();
    let done_frame =
        "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n";
    sock1
        .write_all(format!("{:X}\r\n{done_frame}\r\n0\r\n\r\n", done_frame.len()).as_bytes())
        .await
        .unwrap();
    sock1.flush().await.unwrap();

    let msgs = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let notified = io.message_notify.notified();
            {
                let guard = io.sent_messages.lock().unwrap();
                if !guard.is_empty() {
                    return guard.clone();
                }
            }
            notified.await;
        }
    })
    .await
    .expect("轮次结束后最终回复必须立即送达，不得被慢草稿请求阻塞");

    let wall_elapsed = wall_finish_start.elapsed();
    assert!(
        wall_elapsed < Duration::from_secs(2),
        "轮次结束后应立即中断尚余 28 秒的在途慢草稿并发送最终回复，实际真实耗时: {wall_elapsed:?}"
    );
    assert_eq!(msgs[0].1, "慢草稿下的完整回复");
    assert_eq!(
        io.draft_entered_count(),
        2,
        "轮次进入终态后不得再发起任何新的草稿请求"
    );

    let _ = gw_stop.send(());
    bounded(gw_task).await.unwrap().unwrap();
    harness.shutdown().await;
}

// 场景 19：模型输出半截正文后断流重试时，必须向 Telegram 发送空草稿清除失败尝试的旧内容，随后更新为第二次尝试正文并送达最终回复
#[tokio::test]
async fn clears_failed_attempt_draft_with_empty_snapshot_on_model_retry() {
    let harness = ImTestHarness::start(&[ALLOWED_UID]).await;
    let io = Arc::new(FakeTelegramIo::default());
    let (gw_stop, gw_task) = harness.spawn_gateway(Arc::clone(&io));

    io.push_updates(vec![private_text_update(
        1600,
        PRIVATE_CHAT_ID,
        ALLOWED_UID,
        "测试模型断流重试清除失败草稿",
    )]);

    // 1. 第一次模型尝试输出半截正文后未发 [DONE] 即直接断开连接，触发后台自动重试
    let (mut sock_attempt1, _) = bounded(harness.listener.accept()).await.unwrap();
    let _ = read_http_request(&mut sock_attempt1).await;
    sock_attempt1
        .write_all(
            b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n",
        )
        .await
        .unwrap();
    let partial_chunk = "data: {\"choices\":[{\"delta\":{\"content\":\"半截失败尝试草稿\"},\"finish_reason\":null}]}\n\n";
    sock_attempt1
        .write_all(format!("{:X}\r\n{partial_chunk}\r\n", partial_chunk.len()).as_bytes())
        .await
        .unwrap();
    sock_attempt1.flush().await.unwrap();

    let drafts_first = io.wait_draft_count(1).await;
    let active_draft_id = drafts_first[0].1;
    assert_eq!(drafts_first[0].2, "半截失败尝试草稿");
    assert!(drafts_first[0].3);

    // 突然关闭第一次尝试的 TCP 流以模拟中途断流
    drop(sock_attempt1);

    // 2. 接受第二次模型尝试连接并暂不返回正文：等待第二次正文期间，网关必须已向 Telegram 发送空字符串草稿清空旧内容！
    let (mut sock_attempt2, _) = bounded(harness.listener.accept()).await.unwrap();
    let _ = read_http_request(&mut sock_attempt2).await;

    let drafts_cleared = io.wait_draft_count(2).await;
    assert_eq!(drafts_cleared[1].1, active_draft_id);
    assert_eq!(
        drafts_cleared[1].2, "",
        "模型断流进入重试等待期间，必须发送空草稿清除上一轮失败尝试的半截内容"
    );
    assert!(drafts_cleared[1].3);

    // 3. 第二次模型尝试流式输出新正文并完成，草稿更新为新正文，最终回复送达新正文
    sock_attempt2
        .write_all(
            b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n",
        )
        .await
        .unwrap();
    let retry_chunk = "data: {\"choices\":[{\"delta\":{\"content\":\"第二次尝试完整回复\"},\"finish_reason\":null}]}\n\n";
    sock_attempt2
        .write_all(format!("{:X}\r\n{retry_chunk}\r\n", retry_chunk.len()).as_bytes())
        .await
        .unwrap();
    sock_attempt2.flush().await.unwrap();

    let drafts_retried = io.wait_draft_count(3).await;
    assert_eq!(drafts_retried[2].1, active_draft_id);
    assert_eq!(drafts_retried[2].2, "第二次尝试完整回复");

    let done_frame =
        "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n";
    sock_attempt2
        .write_all(format!("{:X}\r\n{done_frame}\r\n0\r\n\r\n", done_frame.len()).as_bytes())
        .await
        .unwrap();
    sock_attempt2.flush().await.unwrap();

    let final_msgs = io.wait_message_count(1).await;
    assert_eq!(final_msgs[0].1, "第二次尝试完整回复");

    let _ = gw_stop.send(());
    bounded(gw_task).await.unwrap().unwrap();
    harness.shutdown().await;
}

// 场景 20：成功慢草稿请求（每次耗时 6 秒）按发起时刻计算 25 秒保活，连续投递间隔严格保持 25 秒（不超过 Telegram 30 秒有效期，ADR §13）
#[tokio::test(start_paused = true)]
async fn slow_6s_successful_draft_requests_keep_25s_interval_within_30s_ttl() {
    let harness = ImTestHarness::start(&[ALLOWED_UID]).await;
    let io = Arc::new(FakeTelegramIo::default());
    io.set_slow_draft_behavior(Duration::from_secs(6), Ok(()));
    let (gw_stop, gw_task) = harness.spawn_gateway(Arc::clone(&io));

    io.push_updates(vec![private_text_update(
        1700,
        PRIVATE_CHAT_ID,
        ALLOWED_UID,
        "测试 6 秒成功慢请求保活间隔不超过 30 秒",
    )]);

    let (mut sock1, _) = tokio::time::timeout(Duration::from_secs(120), harness.listener.accept())
        .await
        .unwrap()
        .unwrap();
    let _ = read_http_request(&mut sock1).await;
    sock1
        .write_all(
            b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n",
        )
        .await
        .unwrap();
    let chunk1 = "data: {\"choices\":[{\"delta\":{\"content\":\"长思考期间的草稿预览\"},\"finish_reason\":null}]}\n\n";
    sock1
        .write_all(format!("{:X}\r\n{chunk1}\r\n", chunk1.len()).as_bytes())
        .await
        .unwrap();
    sock1.flush().await.unwrap();

    let sse_ping = ": keepalive\n\n";
    let ping_wire = format!("{:X}\r\n{sse_ping}\r\n", sse_ping.len());

    // 1. 第 1 次草稿请求在 t=0 发起，耗时 6 秒在 t=6s 成功完成
    io.wait_draft_entered(1).await;
    tokio::time::advance(Duration::from_secs(6)).await;
    let _ = io.wait_draft_count(1).await;
    let t1 = io.draft_timestamps.lock().unwrap()[0];

    // 2. 从 t=6s 再推进 19 秒到达 t=25s（距第 1 次发起满 25 秒），第 2 次保活请求必须准时发起（而非等到 t=31s），并在 t=31s 完成
    tokio::time::advance(Duration::from_secs(10)).await;
    sock1.write_all(ping_wire.as_bytes()).await.unwrap();
    sock1.flush().await.unwrap();
    tokio::time::advance(Duration::from_secs(9)).await;
    io.wait_draft_entered(2).await;

    tokio::time::advance(Duration::from_secs(6)).await;
    sock1.write_all(ping_wire.as_bytes()).await.unwrap();
    sock1.flush().await.unwrap();
    let _ = io.wait_draft_count(2).await;
    let t2 = io.draft_timestamps.lock().unwrap()[1];
    let interval_1_2 = t2.duration_since(t1);
    assert_eq!(
        interval_1_2,
        Duration::from_secs(25),
        "每次成功请求耗时 6 秒时，两次草稿投递间隔必须为 25 秒（小于 30 秒有效期），不得叠加为 31 秒"
    );

    // 3. 再推进 19 秒到达 t=50s，第 3 次保活请求准时发起并在 t=56s 完成，投递间隔同样为 25 秒
    tokio::time::advance(Duration::from_secs(10)).await;
    sock1.write_all(ping_wire.as_bytes()).await.unwrap();
    sock1.flush().await.unwrap();
    tokio::time::advance(Duration::from_secs(9)).await;
    io.wait_draft_entered(3).await;

    tokio::time::advance(Duration::from_secs(6)).await;
    let _ = io.wait_draft_count(3).await;
    let t3 = io.draft_timestamps.lock().unwrap()[2];
    assert_eq!(t3.duration_since(t2), Duration::from_secs(25));

    tokio::time::resume();
    let done_frame =
        "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n";
    sock1
        .write_all(format!("{:X}\r\n{done_frame}\r\n0\r\n\r\n", done_frame.len()).as_bytes())
        .await
        .unwrap();
    sock1.flush().await.unwrap();

    let final_msgs = io.wait_message_count(1).await;
    assert_eq!(final_msgs[0].1, "长思考期间的草稿预览");

    let _ = gw_stop.send(());
    bounded(gw_task).await.unwrap().unwrap();
    harness.shutdown().await;
}
