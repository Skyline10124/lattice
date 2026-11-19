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
    ConfigureRequest, ConfigureSession, ConversationEntry, ImageRef, LocalErrorKind, LocalUpdate,
    MAX_IMAGE_BYTES, MessageSemantic, Node, PublicTurnOutcome, SubmissionResult, SubmissionUpdate,
    TurnRequest, UserInputPart,
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
    album::LATE_ALBUM_FRAGMENT_REPLY,
    command::{HELP_REPLY, UNSUPPORTED_MEDIA_REPLY, cancel_turn_and_format},
    identity::{ChatKey, InboundId, new_session_id, new_turn_id},
    ledger::{InboundRecord, Ledger},
    prepare_im_gateway,
    telegram::{
        OffsetTracker, RawTelegramUpdate, TELEGRAM_REDELIVERY_WINDOW, TelegramApiError,
        TelegramDownloadError, TelegramIo, TelegramUpdate, chat_key_from_id,
        inbound_id_from_update_id,
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

/// 可编程的假 `TelegramIo`：记录每次 `getUpdates` 传入的 `offset`、全部草稿、全部消息与文件下载，并支持注入阻塞或报错。
type MatchingSendGate = (String, oneshot::Sender<()>, oneshot::Receiver<()>);
type RecordedDownloadCall = (tokio::time::Instant, String, usize);
type ScriptedDownloadMap =
    std::collections::BTreeMap<String, VecDeque<Result<Vec<u8>, TelegramDownloadError>>>;
type BeforeAckHook = Box<dyn FnMut(&[InboundId]) + Send>;

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
    download_files: StdMutex<std::collections::BTreeMap<String, Vec<u8>>>,
    download_overrides: StdMutex<ScriptedDownloadMap>,
    download_calls: StdMutex<Vec<RecordedDownloadCall>>,
    download_notify: Notify,
    download_gate: StdMutex<Option<(oneshot::Sender<()>, oneshot::Receiver<()>)>>,
    poll_return_gate: StdMutex<Option<(oneshot::Sender<()>, oneshot::Receiver<()>)>>,
    before_ack_hook: StdMutex<Option<BeforeAckHook>>,
    bypass_is_acked_once: StdMutex<std::collections::BTreeSet<InboundId>>,
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
            download_files: StdMutex::new(std::collections::BTreeMap::new()),
            download_overrides: StdMutex::new(std::collections::BTreeMap::new()),
            download_calls: StdMutex::new(Vec::new()),
            download_notify: Notify::new(),
            download_gate: StdMutex::new(None),
            poll_return_gate: StdMutex::new(None),
            before_ack_hook: StdMutex::new(None),
            bypass_is_acked_once: StdMutex::new(std::collections::BTreeSet::new()),
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

    fn set_download_bytes(&self, file_id: &str, bytes: Vec<u8>) {
        self.download_files
            .lock()
            .unwrap()
            .insert(file_id.to_owned(), bytes);
    }

    fn push_download_outcome(
        &self,
        file_id: &str,
        outcome: Result<Vec<u8>, TelegramDownloadError>,
    ) {
        self.download_overrides
            .lock()
            .unwrap()
            .entry(file_id.to_owned())
            .or_default()
            .push_back(outcome);
    }

    fn install_download_gate(&self) -> (oneshot::Receiver<()>, oneshot::Sender<()>) {
        let (entered_tx, entered_rx) = oneshot::channel();
        let (release_tx, release_rx) = oneshot::channel();
        *self.download_gate.lock().unwrap() = Some((entered_tx, release_rx));
        (entered_rx, release_tx)
    }

    fn install_poll_return_gate(&self) -> (oneshot::Receiver<()>, oneshot::Sender<()>) {
        let (entered_tx, entered_rx) = oneshot::channel();
        let (release_tx, release_rx) = oneshot::channel();
        *self.poll_return_gate.lock().unwrap() = Some((entered_tx, release_rx));
        (entered_rx, release_tx)
    }

    fn set_before_ack_hook<F>(&self, hook: F)
    where
        F: FnMut(&[InboundId]) + Send + 'static,
    {
        *self.before_ack_hook.lock().unwrap() = Some(Box::new(hook));
    }

    fn arm_bypass_is_acked_once(&self, inbound_id: InboundId) {
        self.bypass_is_acked_once.lock().unwrap().insert(inbound_id);
    }

    fn download_calls(&self) -> Vec<RecordedDownloadCall> {
        self.download_calls.lock().unwrap().clone()
    }

    async fn wait_download_count(&self, count: usize) -> Vec<RecordedDownloadCall> {
        bounded(async {
            loop {
                let notified = self.download_notify.notified();
                {
                    let guard = self.download_calls.lock().unwrap();
                    if guard.len() >= count {
                        return guard.clone();
                    }
                }
                notified.await;
            }
        })
        .await
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
            let maybe_batch = self.batches.lock().unwrap().pop_front();
            if let Some(batch) = maybe_batch {
                let mapped = batch.map(|updates| {
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
                let maybe_gate = self.poll_return_gate.lock().unwrap().take();
                if let Some((entered_tx, release_rx)) = maybe_gate {
                    let _ = entered_tx.send(());
                    let _ = release_rx.await;
                }
                return mapped;
            }
            notified.await;
        }
    }

    fn acknowledge(&self, inbound_ids: &[InboundId]) {
        if let Some(hook) = self.before_ack_hook.lock().unwrap().as_mut() {
            hook(inbound_ids);
        }
        {
            let mut tracker = self.offsets.lock().unwrap();
            for inbound_id in inbound_ids {
                tracker.ack_inbound(inbound_id);
            }
        }
        self.offset_notify.notify_waiters();
    }

    fn is_acknowledged(&self, inbound_id: &InboundId) -> bool {
        if self.bypass_is_acked_once.lock().unwrap().remove(inbound_id) {
            return false;
        }
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

    async fn download_file(
        &self,
        file_id: &str,
        max_bytes: usize,
    ) -> Result<Vec<u8>, TelegramDownloadError> {
        self.download_calls.lock().unwrap().push((
            tokio::time::Instant::now(),
            file_id.to_owned(),
            max_bytes,
        ));
        self.download_notify.notify_waiters();

        let maybe_gate = self.download_gate.lock().unwrap().take();
        if let Some((entered_tx, release_rx)) = maybe_gate {
            let _ = entered_tx.send(());
            let _ = release_rx.await;
        }

        if let Some(scripted) = self
            .download_overrides
            .lock()
            .unwrap()
            .get_mut(file_id)
            .and_then(VecDeque::pop_front)
        {
            return scripted.and_then(|bytes| {
                if bytes.len() > max_bytes {
                    Err(TelegramDownloadError::TooLarge)
                } else {
                    Ok(bytes)
                }
            });
        }

        let files = self.download_files.lock().unwrap();
        let Some(bytes) = files.get(file_id) else {
            return Err(TelegramDownloadError::Api(
                TelegramApiError::UnauthorizedOrInvalid {
                    status: 404,
                    diagnostic: "假平台未配置该 file_id".into(),
                },
            ));
        };
        if bytes.len() > max_bytes {
            return Err(TelegramDownloadError::TooLarge);
        }
        Ok(bytes.clone())
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

    fn set_model_input_modalities(&self, modalities: &[&str]) {
        let endpoint = format!("http://{}/v1", self.listener.local_addr().unwrap());
        let mods = modalities
            .iter()
            .map(|m| format!("{m:?}"))
            .collect::<Vec<_>>()
            .join(", ");
        let cfg_priv = PrivateDirectory::open_or_create(&self.config_dir).unwrap();
        cfg_priv
            .replace(
                OsStr::new("providers.toml"),
                format!(
                    "[providers.local]\nprotocol='chat-completions'\nbase_url='{endpoint}'\nauth='none'\n\n[providers.local.models.\"local-test\"]\ninput = [{mods}]\n"
                )
                .as_bytes(),
            )
            .unwrap();
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
// - 不受支持媒体（贴纸）、/help、/start（含参数）、未授权与群组消息不建会话不落账，
//   且在 sendMessage 被阻塞尚未发出时 offset 照常立即推进。
#[tokio::test]
async fn deduplicates_inbound_id_preserves_single_session_and_handles_new_and_non_ledger_events() {
    let harness = ImTestHarness::start(&[ALLOWED_UID]).await;
    let io = Arc::new(FakeTelegramIo::default());
    let (gw_stop, gw_task) = harness.spawn_gateway(Arc::clone(&io));
    let chat_key = chat_key_from_id(PRIVATE_CHAT_ID);

    // 1. 验证未授权、群组消息、贴纸消息、/help、/start payload 在回复阻塞时仍立即确认推进 offset
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
    let sticker_update: RawTelegramUpdate = serde_json::from_value(serde_json::json!({
        "update_id": 202,
        "message": {
            "message_id": 2,
            "from": {"id": ALLOWED_UID},
            "chat": {"id": PRIVATE_CHAT_ID, "type": "private"},
            "sticker": {
                "file_id": "stk1",
                "file_unique_id": "u1",
                "type": "regular",
                "width": 512,
                "height": 512
            }
        }
    }))
    .unwrap();
    let help_update = private_text_update(203, PRIVATE_CHAT_ID, ALLOWED_UID, "/help");
    let start_update =
        private_text_update(204, PRIVATE_CHAT_ID, ALLOWED_UID, "/start invite_token_xyz");

    io.push_updates(vec![
        group_update.into_domain(),
        stranger_update,
        sticker_update.into_domain(),
        help_update,
        start_update,
    ]);

    // 即使 sendMessage 被闸门挡住尚未返回，全部 5 条不写台账事件的确认也必须使下一次轮询 offset 推进到 205
    io.wait_send_entered(1).await;
    io.wait_offset(Some(205)).await;
    let _ = unblock_send.send(());

    let initial_replies = io.wait_message_count(3).await;
    let reply_texts: Vec<&str> = initial_replies.iter().map(|(_, t)| t.as_str()).collect();
    assert!(reply_texts.contains(&UNSUPPORTED_MEDIA_REPLY));
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
                Vec::new(),
                chat_key.clone(),
                preset_session_id,
                dummy_turn,
                true,
                Utc::now(),
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
        parts: vec![UserInputPart::Text {
            text: "遗留已接受但台账停在 Registered".into(),
        }],
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
        parts: vec![UserInputPart::Text {
            text: "遗留 Accepted 待补发".into(),
        }],
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
            parts: vec![UserInputPart::Text {
                text: "挂起中的长轮次".into(),
            }],
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
                parts: vec![UserInputPart::Text {
                    text: "准入途中的遗留 Registered 轮次".into(),
                }],
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
                Vec::new(),
                chat_c.clone(),
                session_c,
                turn_c_missing,
                true,
                Utc::now(),
            )
            .await
            .unwrap();
        ledger
            .register_turn(
                id_c_inflight.clone(),
                Vec::new(),
                chat_c.clone(),
                session_c,
                turn_c_inflight,
                false,
                Utc::now(),
            )
            .await
            .unwrap();

        ledger
            .register_turn(
                id_reg_accepted.clone(),
                Vec::new(),
                chat_b.clone(),
                session_a,
                turn_reg_accepted,
                false,
                Utc::now(),
            )
            .await
            .unwrap();
        ledger
            .register_turn(
                id_accepted_done.clone(),
                Vec::new(),
                chat_b.clone(),
                session_b,
                turn_accepted,
                true,
                Utc::now(),
            )
            .await
            .unwrap();
        ledger.mark_accepted(&id_accepted_done).await.unwrap();

        ledger
            .register_turn(
                id_accepted_hanging.clone(),
                Vec::new(),
                chat_a.clone(),
                session_a,
                turn_hanging,
                true,
                Utc::now(),
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
            .register_turn(
                reg_id.clone(),
                Vec::new(),
                chat_key,
                sid,
                tid,
                true,
                Utc::now(),
            )
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
        parts: vec![UserInputPart::Text {
            text: "来自 CLI/TUI 的并发请求".into(),
        }],
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
                Vec::new(),
                chat_key.clone(),
                preset_sid,
                new_turn_id(),
                true,
                Utc::now(),
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
    // 在首条轮次尚未返回前，排队中的次条消息必然收到忙碌提示，随后再完成首条轮次回复，避免高负载下调度竞态。
    let _ = io2.wait_message_count(1).await;
    write_sse_complete(&mut s1, None, "重挂第一条回复", None).await;
    let msgs = io2.wait_message_count(2).await;
    assert_eq!(
        msgs.len(),
        2,
        "重挂后次条忙碌提示与首条完成回复均应正常响应"
    );

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

/// 组装由 `serve_for_test_with_components` 真实托管的 `"im"` 组件注册项（注入可切换的假 `TelegramIo`）。
fn hosted_im_registration(
    config_dir: PathBuf,
    layout: StateLayout,
    io_slot: Arc<StdMutex<Arc<FakeTelegramIo>>>,
) -> lattice_server::ComponentRegistration {
    lattice_server::ComponentRegistration::new("im", move |service| {
        match prepare_im_gateway(&config_dir, layout.clone(), service) {
            ImBuildOutcome::Prepared(gw) => {
                let io = io_slot.lock().unwrap().clone();
                lattice_server::ComponentBuildOutcome::Component(
                    lattice_server::PreparedComponent::new(move |stop| async move {
                        gw.run_with_io(io, stop).await.map_err(|e| e.to_string())
                    }),
                )
            }
            ImBuildOutcome::Unavailable { reason } => {
                lattice_server::ComponentBuildOutcome::Unavailable { reason }
            }
            ImBuildOutcome::InvalidConfiguration { reason } => {
                lattice_server::ComponentBuildOutcome::InvalidConfiguration { reason }
            }
            ImBuildOutcome::Disabled => lattice_server::ComponentBuildOutcome::Disabled,
        }
    })
}

/// 初始化真实托管 IM 组件测试目录与本地假模型监听器。
async fn setup_hosted_im_env(
    allowed_senders: &[i64],
) -> (tempfile::TempDir, PathBuf, StateLayout, TcpListener) {
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
    (dir, config_dir, layout, listener)
}

async fn wait_client_ready(client: &LocalClient) -> lattice_ipc::DaemonStatus {
    bounded(async {
        loop {
            if let Ok(status) = client.status().await
                && status.components.iter().any(|c| {
                    c.name == "im" && matches!(c.state, lattice_ipc::ComponentState::Mounted { .. })
                })
            {
                return status;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
}

#[tokio::test]
async fn reload_mid_turn_preserves_epoch_and_new_gateway_delivers_final_reply() {
    let (dir, config_dir, layout, listener) = setup_hosted_im_env(&[ALLOWED_UID]).await;
    let io1 = Arc::new(FakeTelegramIo::default());
    let io2 = Arc::new(FakeTelegramIo::default());
    let io_slot = Arc::new(StdMutex::new(Arc::clone(&io1)));

    let (stop_tx, stop_rx) = mpsc::channel(4);
    let reg = hosted_im_registration(config_dir, layout.clone(), Arc::clone(&io_slot));
    let serve_layout = layout.clone();
    let home = Some(dir.path().to_path_buf());
    let daemon_task = tokio::spawn(async move {
        lattice_server::serve_for_test_with_components(
            serve_layout,
            home,
            Duration::from_secs(30),
            stop_rx,
            vec![reg],
        )
        .await
    });

    let client = LocalClient::new(layout.clone());
    let before = wait_client_ready(&client).await;
    assert_eq!(
        before.components,
        vec![lattice_ipc::ComponentStatus {
            name: "im".into(),
            state: lattice_ipc::ComponentState::Mounted {
                configuration_error: None,
            },
        }]
    );

    io1.push_updates(vec![private_text_update(
        1801,
        PRIVATE_CHAT_ID,
        ALLOWED_UID,
        "长轮次执行期间热重载网关",
    )]);

    let (mut sock, _) = bounded(listener.accept()).await.unwrap();
    let _ = read_http_request(&mut sock).await;
    sock.write_all(
        b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n",
    )
    .await
    .unwrap();
    let part1 = "data: {\"choices\":[{\"delta\":{\"content\":\"热重载前半段\"},\"finish_reason\":null}]}\n\n";
    sock.write_all(format!("{:X}\r\n{part1}\r\n", part1.len()).as_bytes())
        .await
        .unwrap();
    sock.flush().await.unwrap();

    // 确认该轮已被后台接受并产出草稿。
    let _ = io1.wait_draft_count(1).await;

    // 切换到新网关收发替身并经 IPC 执行 im_reload。
    *io_slot.lock().unwrap() = Arc::clone(&io2);
    let mount = bounded(client.im_reload()).await.unwrap();
    assert_eq!(mount, lattice_ipc::ImMountResult::Mounted);

    let during = bounded(client.status()).await.unwrap();
    assert_eq!(during.epoch, before.epoch, "热重载不得改变后台 epoch");
    assert_eq!(during.owners, 1, "正在执行的轮次不得因组件热重载中断");

    // 完成该轮模型流式输出：新网关实例接手台账后发出完整最终回复。
    let part2 = "data: {\"choices\":[{\"delta\":{\"content\":\"与后半段\"},\"finish_reason\":null}]}\n\ndata: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n";
    sock.write_all(format!("{:X}\r\n{part2}\r\n0\r\n\r\n", part2.len()).as_bytes())
        .await
        .unwrap();
    sock.flush().await.unwrap();

    let final_msgs = io2.wait_message_count(1).await;
    assert_eq!(final_msgs[0].1, "热重载前半段与后半段");
    assert!(
        io1.sent_messages.lock().unwrap().is_empty(),
        "旧网关实例退出后不应发出最终回复"
    );

    stop_tx.send(()).await.unwrap();
    let exit = bounded(daemon_task).await.unwrap().unwrap();
    assert!(exit.failure.is_none());
}

#[tokio::test]
async fn step3_10s_timeout_abandons_hung_gateway_and_restart_delivers_final_reply() {
    let (dir, config_dir, layout, listener) = setup_hosted_im_env(&[ALLOWED_UID]).await;
    let io1 = Arc::new(FakeTelegramIo::default());
    let _hold_send_tx = io1.install_send_gate();
    let (drain_entered_rx, _hold_drain_tx) = io1.install_drain_gate();

    let io_slot1 = Arc::new(StdMutex::new(Arc::clone(&io1)));
    let (stop_tx1, stop_rx1) = mpsc::channel(4);
    let reg1 = hosted_im_registration(config_dir.clone(), layout.clone(), io_slot1);
    let serve_layout1 = layout.clone();
    let home1 = Some(dir.path().to_path_buf());
    let mut daemon_task1 = tokio::spawn(async move {
        lattice_server::serve_for_test_with_components(
            serve_layout1,
            home1,
            Duration::from_secs(30),
            stop_rx1,
            vec![reg1],
        )
        .await
    });

    let client = LocalClient::new(layout.clone());
    let _ = wait_client_ready(&client).await;

    io1.push_updates(vec![private_text_update(
        1802,
        PRIVATE_CHAT_ID,
        ALLOWED_UID,
        "停机超 10 秒放弃收尾后重启补发",
    )]);

    let (mut sock, _) = bounded(listener.accept()).await.unwrap();
    let _ = read_http_request(&mut sock).await;
    write_sse_complete(&mut sock, None, "重启后台后补发的最终回复", None).await;

    // 等待模型轮次结束且网关进入被卡住的最终回复 send_message。
    bounded(async {
        loop {
            let notified = io1.send_entered_notify.notified();
            if io1
                .send_entered_count
                .load(std::sync::atomic::Ordering::SeqCst)
                >= 1
            {
                break;
            }
            notified.await;
        }
    })
    .await;

    // 暂停模拟时钟并触发后台停机：在途 send_message 被取消（保留未发出的台账记录），网关收尾卡在 drain 满 10 秒后被第 3 步强制中止。
    tokio::time::pause();
    stop_tx1.send(()).await.unwrap();
    drain_entered_rx.await.unwrap();
    tokio::time::advance(Duration::from_secs(9)).await;
    tokio::task::yield_now().await;
    assert!(
        !daemon_task1.is_finished(),
        "未满 10 秒时后台仍在第 3 步等待 IM 收尾"
    );

    tokio::time::advance(Duration::from_secs(1)).await;
    for _ in 0..8 {
        tokio::task::yield_now().await;
    }
    tokio::time::resume();

    let exit1 = bounded(&mut daemon_task1).await.unwrap().unwrap();
    assert!(exit1.failure.is_none());
    drop(exit1);
    assert!(
        io1.sent_messages.lock().unwrap().is_empty(),
        "卡住的旧网关在 10 秒超时被中止，未发出最终回复"
    );

    // 在同一状态根重启新后台并再次挂载 IM：启动期恢复台账补发该轮最终回复。
    let io2 = Arc::new(FakeTelegramIo::default());
    let io_slot2 = Arc::new(StdMutex::new(Arc::clone(&io2)));
    let (stop_tx2, stop_rx2) = mpsc::channel(4);
    let reg2 = hosted_im_registration(config_dir, layout.clone(), io_slot2);
    let serve_layout2 = layout.clone();
    let home2 = Some(dir.path().to_path_buf());
    let daemon_task2 = tokio::spawn(async move {
        lattice_server::serve_for_test_with_components(
            serve_layout2,
            home2,
            Duration::from_secs(30),
            stop_rx2,
            vec![reg2],
        )
        .await
    });

    let _ = wait_client_ready(&client).await;
    let recovered_msgs = io2.wait_message_count(1).await;
    assert_eq!(recovered_msgs[0].1, "重启后台后补发的最终回复");

    stop_tx2.send(()).await.unwrap();
    let exit2 = bounded(daemon_task2).await.unwrap().unwrap();
    assert!(exit2.failure.is_none());
}

#[tokio::test]
async fn gateway_panic_is_isolated_in_daemon_status_and_recovers_on_im_reload() {
    let (dir, config_dir, layout, listener) = setup_hosted_im_env(&[ALLOWED_UID]).await;
    let io1 = Arc::new(FakeTelegramIo::default());
    let io2 = Arc::new(FakeTelegramIo::default());
    let io_slot = Arc::new(StdMutex::new(Arc::clone(&io1)));

    let (stop_tx, stop_rx) = mpsc::channel(4);
    let reg = hosted_im_registration(config_dir, layout.clone(), Arc::clone(&io_slot));
    let serve_layout = layout.clone();
    let home = Some(dir.path().to_path_buf());
    let daemon_task = tokio::spawn(async move {
        lattice_server::serve_for_test_with_components(
            serve_layout,
            home,
            Duration::from_secs(30),
            stop_rx,
            vec![reg],
        )
        .await
    });

    let client = LocalClient::new(layout);
    let _ = wait_client_ready(&client).await;

    // 触发假 TelegramIo 轮询任务 panic，验证 PreparedImGateway 将 panic 传播给后台组件监督器且不拖垮后台。
    io1.panic_on_next_poll
        .store(true, std::sync::atomic::Ordering::SeqCst);
    io1.batch_notify.notify_waiters();

    let reason = bounded(async {
        loop {
            let status = client.status().await.unwrap();
            if let Some(lattice_ipc::ComponentStatus {
                state: lattice_ipc::ComponentState::Unmounted { reason },
                ..
            }) = status.components.first()
            {
                return reason.clone();
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    assert!(reason.contains("panic"), "未挂载原因应指明 panic：{reason}");
    assert!(
        !reason.contains("注入轮询任务"),
        "不得暴露原始 panic 消息载荷：{reason}"
    );

    // 切换到正常收发替身后通过 client.im_reload() 恢复挂载并正常处理消息。
    *io_slot.lock().unwrap() = Arc::clone(&io2);
    assert_eq!(
        bounded(client.im_reload()).await.unwrap(),
        lattice_ipc::ImMountResult::Mounted
    );

    io2.push_updates(vec![private_text_update(
        1803,
        PRIVATE_CHAT_ID,
        ALLOWED_UID,
        "panic 后恢复服务",
    )]);
    let (mut sock, _) = bounded(listener.accept()).await.unwrap();
    let _ = read_http_request(&mut sock).await;
    write_sse_complete(&mut sock, None, "恢复成功", None).await;

    let msgs = io2.wait_message_count(1).await;
    assert_eq!(msgs[0].1, "恢复成功");

    stop_tx.send(()).await.unwrap();
    let exit = bounded(daemon_task).await.unwrap().unwrap();
    assert!(exit.failure.is_none());
}

#[tokio::test]
async fn hosted_gateway_subtask_panic_never_leaks_payload_to_stderr() {
    const SUBPROC_ENV: &str = "LATTICE_TEST_GATEWAY_PANIC_SUBPROCESS";

    if std::env::var(SUBPROC_ENV).as_deref() == Ok("1") {
        let filter = tracing_subscriber::EnvFilter::try_from_env("LATTICE_LOG")
            .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("trace"));
        let _ = tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_writer(std::io::stderr)
            .with_ansi(false)
            .compact()
            .try_init();
        lattice_server::install_component_panic_hook();

        let (dir, config_dir, layout, _listener) = setup_hosted_im_env(&[ALLOWED_UID]).await;
        let io1 = Arc::new(FakeTelegramIo::default());
        let io2 = Arc::new(FakeTelegramIo::default());
        let io_slot = Arc::new(StdMutex::new(Arc::clone(&io1)));

        let (stop_tx, stop_rx) = mpsc::channel(4);
        let reg = hosted_im_registration(config_dir, layout.clone(), Arc::clone(&io_slot));
        let serve_layout = layout.clone();
        let home = Some(dir.path().to_path_buf());
        let daemon_task = tokio::spawn(async move {
            lattice_server::serve_for_test_with_components(
                serve_layout,
                home,
                Duration::from_secs(30),
                stop_rx,
                vec![reg],
            )
            .await
        });

        let client = LocalClient::new(layout);
        let _ = wait_client_ready(&client).await;

        // 1. 触发轮询子任务 panic。
        io1.arm_panic_on_next_poll();
        bounded(async {
            loop {
                let status = client.status().await.unwrap();
                if matches!(
                    status.components.first(),
                    Some(lattice_ipc::ComponentStatus {
                        state: lattice_ipc::ComponentState::Unmounted { reason },
                        ..
                    }) if reason.contains("panic")
                ) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await;

        // 2. 重新挂载后触发单对话准入子任务 panic。
        *io_slot.lock().unwrap() = Arc::clone(&io2);
        assert_eq!(
            bounded(client.im_reload()).await.unwrap(),
            lattice_ipc::ImMountResult::Mounted
        );
        crate::gateway::arm_chat_worker_panic_for_test(inbound_id_from_update_id(1899));
        io2.push_updates(vec![private_text_update(
            1899,
            PRIVATE_CHAT_ID,
            ALLOWED_UID,
            "触发准入工作协程 panic",
        )]);
        bounded(async {
            loop {
                let status = client.status().await.unwrap();
                if matches!(
                    status.components.first(),
                    Some(lattice_ipc::ComponentStatus {
                        state: lattice_ipc::ComponentState::Unmounted { reason },
                        ..
                    }) if reason.contains("panic")
                ) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await;

        stop_tx.send(()).await.unwrap();
        let exit = bounded(daemon_task).await.unwrap().unwrap();
        assert!(exit.failure.is_none());
        return;
    }

    let exe = std::env::current_exe().unwrap();
    let out = std::process::Command::new(&exe)
        .args([
            "--exact",
            "gateway_tests::hosted_gateway_subtask_panic_never_leaks_payload_to_stderr",
            "--nocapture",
        ])
        .env(SUBPROC_ENV, "1")
        .env("LATTICE_LOG", "trace")
        .env("NO_COLOR", "1")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "网关子任务 panic 隔离子进程应正常退出，stdout={stdout}\nstderr={stderr}"
    );
    for raw_payload in ["注入轮询任务", "注入单对话准入子任务"] {
        assert!(
            !stdout.contains(raw_payload) && !stderr.contains(raw_payload),
            "网关子任务 panic 原始载荷不得泄露到 stderr/stdout（命中 {raw_payload}）：\nstdout={stdout}\nstderr={stderr}"
        );
    }
    assert!(
        stderr.contains("受托管组件执行期间发生 panic（已抑制原始载荷）"),
        "stderr 应记录脱敏后的组件 panic 位置告警：{stderr}"
    );
}

fn test_base64_encode(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0];
        let b1 = chunk.get(1).copied().unwrap_or(0);
        let b2 = chunk.get(2).copied().unwrap_or(0);
        let n = (u32::from(b0) << 16) | (u32::from(b1) << 8) | u32::from(b2);
        out.push(TABLE[((n >> 18) & 0x3f) as usize] as char);
        out.push(TABLE[((n >> 12) & 0x3f) as usize] as char);
        if chunk.len() > 1 {
            out.push(TABLE[((n >> 6) & 0x3f) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(TABLE[(n & 0x3f) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}

fn private_photo_update(
    update_id: i64,
    chat_id: i64,
    sender_id: i64,
    photos: &[(&str, Option<u64>)],
    caption: Option<&str>,
) -> TelegramUpdate {
    let photo_arr: Vec<serde_json::Value> = photos
        .iter()
        .enumerate()
        .map(|(idx, (file_id, file_size))| {
            let mut obj = serde_json::json!({
                "file_id": file_id,
                "file_unique_id": format!("u_{update_id}_{idx}"),
                "width": (idx + 1) * 100,
                "height": (idx + 1) * 100,
            });
            if let Some(size) = file_size {
                obj["file_size"] = serde_json::json!(size);
            }
            obj
        })
        .collect();
    let mut msg = serde_json::json!({
        "message_id": update_id,
        "from": {"id": sender_id},
        "chat": {"id": chat_id, "type": "private"},
        "photo": photo_arr,
    });
    if let Some(cap) = caption {
        msg["caption"] = serde_json::json!(cap);
    }
    let raw: RawTelegramUpdate = serde_json::from_value(serde_json::json!({
        "update_id": update_id,
        "message": msg,
    }))
    .expect("构造私聊压缩图片更新应成功");
    raw.into_domain()
}

fn private_document_update(
    update_id: i64,
    chat_id: i64,
    sender_id: i64,
    file_id: &str,
    file_size: Option<u64>,
    mime_type: Option<&str>,
    caption: Option<&str>,
) -> TelegramUpdate {
    let mut doc = serde_json::json!({
        "file_id": file_id,
        "file_unique_id": format!("doc_u_{update_id}"),
    });
    if let Some(size) = file_size {
        doc["file_size"] = serde_json::json!(size);
    }
    if let Some(mime) = mime_type {
        doc["mime_type"] = serde_json::json!(mime);
    }
    let mut msg = serde_json::json!({
        "message_id": update_id,
        "from": {"id": sender_id},
        "chat": {"id": chat_id, "type": "private"},
        "document": doc,
    });
    if let Some(cap) = caption {
        msg["caption"] = serde_json::json!(cap);
    }
    let raw: RawTelegramUpdate = serde_json::from_value(serde_json::json!({
        "update_id": update_id,
        "message": msg,
    }))
    .expect("构造私聊文档更新应成功");
    raw.into_domain()
}

// 场景 13：授权私聊单张压缩 photo（无说明、带说明、说明为 /new 不触发会话重置）
// 与未压缩 PNG/WebP/GIF document 端到端提交，核对假模型收到的图片字节、MIME、[Image] / [Image, Text] 顺序与会话库 BLOBS 历史重放持久化。
#[tokio::test]
async fn submits_single_photo_and_uncompressed_image_documents_preserving_order_and_persisting_blobs()
 {
    let harness = ImTestHarness::start(&[ALLOWED_UID]).await;
    let io = Arc::new(FakeTelegramIo::default());
    let chat_key = chat_key_from_id(PRIVATE_CHAT_ID);

    let jpeg_1 = b"\xff\xd8\xff\xe0\x00\x10JFIF_PHOTO_ROUND_1_PAYLOAD".to_vec();
    let jpeg_2 = b"\xff\xd8\xff\xe0\x00\x10JFIF_PHOTO_ROUND_2_PAYLOAD".to_vec();
    let jpeg_3 = b"\xff\xd8\xff\xe0\x00\x10JFIF_PHOTO_SLASH_NEW_PAYLOAD".to_vec();
    let png_doc = b"\x89PNG\r\n\x1a\nUNCOMPRESSED_PNG_DOC_PAYLOAD".to_vec();
    let webp_doc = b"RIFF\x24\x00\x00\x00WEBPUNCOMPRESSED_WEBP_PAYLOAD".to_vec();
    let gif_doc = b"GIF89aUNCOMPRESSED_GIF_DOC_PAYLOAD".to_vec();

    io.set_download_bytes("photo_hi_1", jpeg_1.clone());
    io.set_download_bytes("photo_hi_2", jpeg_2.clone());
    io.set_download_bytes("photo_hi_3", jpeg_3.clone());
    io.set_download_bytes("doc_png_4", png_doc.clone());
    io.set_download_bytes("doc_webp_5", webp_doc.clone());
    io.set_download_bytes("doc_gif_6", gif_doc.clone());

    let (gw_stop, gw_task) = harness.spawn_gateway(Arc::clone(&io));

    // 1. 纯图片 photo（多尺寸取最后一项最高分辨率，无 caption -> [Image]）
    io.push_updates(vec![private_photo_update(
        2101,
        PRIVATE_CHAT_ID,
        ALLOWED_UID,
        &[
            ("photo_thumb_unused", Some(32)),
            ("photo_hi_1", Some(jpeg_1.len() as u64)),
        ],
        None,
    )]);

    let (mut s1, _) = bounded(harness.listener.accept()).await.unwrap();
    let req1: serde_json::Value = serde_json::from_str(&read_http_request(&mut s1).await).unwrap();
    let user_msg1 = req1["messages"].as_array().unwrap().last().unwrap();
    assert_eq!(user_msg1["role"], "user");
    assert_eq!(
        user_msg1["content"],
        serde_json::json!([{
            "type": "image_url",
            "image_url": {
                "url": format!("data:image/jpeg;base64,{}", test_base64_encode(&jpeg_1))
            }
        }]),
        "无 caption 的单张 photo 必须以单一 [Image] 内容块发往模型"
    );
    write_sse_complete(&mut s1, None, "第1轮纯图回复", None).await;
    let msgs1 = io.wait_message_count(1).await;
    assert_eq!(msgs1[0].1, "第1轮纯图回复");
    assert_eq!(
        io.download_calls()
            .iter()
            .map(|(_, id, max)| (id.as_str(), *max))
            .collect::<Vec<_>>(),
        vec![("photo_hi_1", MAX_IMAGE_BYTES)],
        "必须只下载最高分辨率 photo_hi_1 且传入 MAX_IMAGE_BYTES"
    );

    // 2. 带说明文字的 photo -> 同一会话内按 [Image, Text] 顺序提交，且历史重放第 1 轮 BLOBS 图片
    io.push_updates(vec![private_photo_update(
        2102,
        PRIVATE_CHAT_ID,
        ALLOWED_UID,
        &[("photo_hi_2", Some(jpeg_2.len() as u64))],
        Some("请核对这张截图"),
    )]);

    let (mut s2, _) = bounded(harness.listener.accept()).await.unwrap();
    let req2: serde_json::Value = serde_json::from_str(&read_http_request(&mut s2).await).unwrap();
    let msgs_arr2 = req2["messages"].as_array().unwrap();
    assert!(
        msgs_arr2.iter().any(|m| {
            m["content"].as_array().is_some_and(|arr| {
                arr.iter().any(|part| {
                    part["image_url"]["url"]
                        == format!("data:image/jpeg;base64,{}", test_base64_encode(&jpeg_1))
                })
            })
        }),
        "多轮会话第 2 轮请求必须从会话库 BLOBS 重放第 1 轮已持久化的图片"
    );
    let user_msg2 = msgs_arr2.last().unwrap();
    assert_eq!(
        user_msg2["content"],
        serde_json::json!([
            {
                "type": "image_url",
                "image_url": {
                    "url": format!("data:image/jpeg;base64,{}", test_base64_encode(&jpeg_2))
                }
            },
            {
                "type": "text",
                "text": "请核对这张截图"
            }
        ]),
        "带说明文字的图片必须严格按 [Image, Text] 顺序发送"
    );
    write_sse_complete(&mut s2, None, "第2轮图文回复", None).await;
    let msgs2 = io.wait_message_count(2).await;
    assert_eq!(msgs2[1].1, "第2轮图文回复");

    // 3. caption 为 "/new" 的图片消息：绝不触发 /new 会话重置，仍在原会话提交 [Image, Text("/new")]
    io.push_updates(vec![private_photo_update(
        2103,
        PRIVATE_CHAT_ID,
        ALLOWED_UID,
        &[("photo_hi_3", Some(jpeg_3.len() as u64))],
        Some("/new"),
    )]);

    let (mut s3, _) = bounded(harness.listener.accept()).await.unwrap();
    let req3: serde_json::Value = serde_json::from_str(&read_http_request(&mut s3).await).unwrap();
    let msgs_arr3 = req3["messages"].as_array().unwrap();
    assert!(
        msgs_arr3.iter().any(|m| m["content"] == "第2轮图文回复"),
        "图片 caption 为 /new 时不得重置会话，仍须携带既有会话历史"
    );
    assert_eq!(
        msgs_arr3.last().unwrap()["content"],
        serde_json::json!([
            {
                "type": "image_url",
                "image_url": {
                    "url": format!("data:image/jpeg;base64,{}", test_base64_encode(&jpeg_3))
                }
            },
            {
                "type": "text",
                "text": "/new"
            }
        ])
    );
    write_sse_complete(&mut s3, None, "第3轮斜杠说明回复", None).await;
    let msgs3 = io.wait_message_count(3).await;
    assert_eq!(msgs3[2].1, "第3轮斜杠说明回复");

    // 4. 未压缩 PNG、WebP、GIF document 依次提交，核对各自魔数识别出的 MIME 与原始字节
    for (idx, uid, fid, raw_mime, expected_mime, bytes, reply_label) in [
        (
            4usize,
            2104i64,
            "doc_png_4",
            "  Image/PNG ; name=\"ui.png\" ",
            "image/png",
            &png_doc,
            "第4轮PNG回复",
        ),
        (
            5usize,
            2105i64,
            "doc_webp_5",
            "image/webp",
            "image/webp",
            &webp_doc,
            "第5轮WebP回复",
        ),
        (
            6usize,
            2106i64,
            "doc_gif_6",
            "IMAGE/GIF",
            "image/gif",
            &gif_doc,
            "第6轮GIF回复",
        ),
    ] {
        io.push_updates(vec![private_document_update(
            uid,
            PRIVATE_CHAT_ID,
            ALLOWED_UID,
            fid,
            Some(bytes.len() as u64),
            Some(raw_mime),
            None,
        )]);
        let (mut sock, _) = bounded(harness.listener.accept()).await.unwrap();
        let req_json: serde_json::Value =
            serde_json::from_str(&read_http_request(&mut sock).await).unwrap();
        let last_user = req_json["messages"].as_array().unwrap().last().unwrap();
        assert_eq!(
            last_user["content"],
            serde_json::json!([{
                "type": "image_url",
                "image_url": {
                    "url": format!("data:{expected_mime};base64,{}", test_base64_encode(bytes))
                }
            }]),
            "未压缩文档 {fid} 应按魔数识别为 {expected_mime} 并编码完整字节"
        );
        write_sse_complete(&mut sock, None, reply_label, None).await;
        let all_msgs = io.wait_message_count(idx).await;
        assert_eq!(all_msgs[idx - 1].1, reply_label);
    }

    // 5. 再向同一对话发送第 7 轮纯文本消息，行为级验证假模型收到的请求体完整重放了前 6 轮从会话库 BLOBS 读取的全部原始图片载荷
    io.push_updates(vec![private_text_update(
        2107,
        PRIVATE_CHAT_ID,
        ALLOWED_UID,
        "总结以上全部图片",
    )]);
    let (mut s7, _) = bounded(harness.listener.accept()).await.unwrap();
    let req7: serde_json::Value = serde_json::from_str(&read_http_request(&mut s7).await).unwrap();
    let msgs_arr7 = req7["messages"].as_array().unwrap();
    let replayed_data_urls: Vec<String> = msgs_arr7
        .iter()
        .filter_map(|m| m["content"].as_array())
        .flatten()
        .filter_map(|part| part["image_url"]["url"].as_str().map(str::to_owned))
        .collect();
    assert_eq!(
        replayed_data_urls,
        vec![
            format!("data:image/jpeg;base64,{}", test_base64_encode(&jpeg_1)),
            format!("data:image/jpeg;base64,{}", test_base64_encode(&jpeg_2)),
            format!("data:image/jpeg;base64,{}", test_base64_encode(&jpeg_3)),
            format!("data:image/png;base64,{}", test_base64_encode(&png_doc)),
            format!("data:image/webp;base64,{}", test_base64_encode(&webp_doc)),
            format!("data:image/gif;base64,{}", test_base64_encode(&gif_doc)),
        ],
        "第 7 轮纯文本请求必须按顺序精确重放从会话库 BLOBS 读取的全部 6 张原始图片 base64 data URL"
    );
    let last_user7 = msgs_arr7.last().unwrap();
    assert_eq!(last_user7["role"], "user");
    assert!(
        last_user7["content"] == "总结以上全部图片"
            || last_user7["content"]
                == serde_json::json!([{"type": "text", "text": "总结以上全部图片"}]),
        "第 7 轮末尾用户消息正文必须为“总结以上全部图片”，实际: {}",
        last_user7["content"]
    );
    write_sse_complete(&mut s7, None, "第7轮总结回复", None).await;
    let msgs7 = io.wait_message_count(7).await;
    assert_eq!(msgs7[6].1, "第7轮总结回复");

    io.wait_offset(Some(2108)).await;
    let _ = gw_stop.send(());
    bounded(gw_task).await.unwrap().unwrap();

    // 6. 核对台账唯一绑定 SessionId 与会话历史节点 ImageRef
    let ledger = Ledger::open_and_prune(
        harness.layout.clone(),
        Utc::now(),
        TELEGRAM_REDELIVERY_WINDOW,
    )
    .await
    .unwrap();
    let bound_sid = ledger
        .bound_session(&chat_key)
        .await
        .expect("7 轮图文与总结消息应绑定同一个会话");

    let client = LocalClient::new(harness.layout.clone());
    let mut observed = client.observe(bound_sid);
    let user_messages = bounded(async {
        let mut msgs = Vec::new();
        while msgs.len() < 7 {
            match observed.next().await {
                Some(LocalUpdate::Entry { entry }) => {
                    if let ConversationEntry::UserMessage {
                        content: Node::UserMessage { message, .. },
                        ..
                    } = *entry
                    {
                        msgs.push(message);
                    }
                }
                Some(_) => {}
                None => panic!("会话观察流在收齐 7 条用户消息前意外结束"),
            }
        }
        msgs
    })
    .await;
    drop(observed);
    assert_eq!(
        user_messages.len(),
        7,
        "同一会话内必须持久保留全部 7 条用户消息"
    );
    assert_eq!(
        user_messages[0].parts[0].semantic,
        MessageSemantic::Image {
            image: ImageRef::from_bytes(&jpeg_1).unwrap(),
        }
    );
    assert_eq!(user_messages[0].parts.len(), 1);
    assert_eq!(user_messages[1].parts.len(), 2);
    assert_eq!(
        user_messages[1].parts[0].semantic,
        MessageSemantic::Image {
            image: ImageRef::from_bytes(&jpeg_2).unwrap(),
        }
    );
    assert_eq!(
        user_messages[1].parts[1].semantic,
        MessageSemantic::Text {
            text: "请核对这张截图".into(),
        }
    );
    assert_eq!(
        user_messages[2].parts[1].semantic,
        MessageSemantic::Text {
            text: "/new".into(),
        }
    );
    assert_eq!(
        user_messages[3].parts[0].semantic,
        MessageSemantic::Image {
            image: ImageRef::from_bytes(&png_doc).unwrap(),
        }
    );
    assert_eq!(
        user_messages[4].parts[0].semantic,
        MessageSemantic::Image {
            image: ImageRef::from_bytes(&webp_doc).unwrap(),
        }
    );
    assert_eq!(
        user_messages[5].parts[0].semantic,
        MessageSemantic::Image {
            image: ImageRef::from_bytes(&gif_doc).unwrap(),
        }
    );
    assert_eq!(
        user_messages[6].parts[0].semantic,
        MessageSemantic::Text {
            text: "总结以上全部图片".into(),
        }
    );

    harness.shutdown().await;
}

// 场景 14：未授权发送者或群组聊天发来图片消息时立即静默确认，零下载、零台账、零草稿会话。
#[tokio::test]
async fn ignores_unauthorized_or_group_image_messages_silently_with_zero_downloads() {
    let harness = ImTestHarness::start(&[ALLOWED_UID]).await;
    let io = Arc::new(FakeTelegramIo::default());
    io.set_download_bytes("should_never_download", b"\x89PNG\r\n\x1a\nDATA".to_vec());

    let (gw_stop, gw_task) = harness.spawn_gateway(Arc::clone(&io));

    let group_photo: RawTelegramUpdate = serde_json::from_value(serde_json::json!({
        "update_id": 2201,
        "message": {
            "message_id": 1,
            "from": {"id": ALLOWED_UID},
            "chat": {"id": -100999, "type": "supergroup"},
            "photo": [{"file_id": "should_never_download", "file_unique_id": "u1", "width": 100, "height": 100}]
        }
    }))
    .unwrap();

    io.push_updates(vec![
        group_photo.into_domain(),
        private_photo_update(
            2202,
            OTHER_UID,
            OTHER_UID,
            &[("should_never_download", Some(16))],
            Some("陌生人私聊发图"),
        ),
        private_document_update(
            2203,
            OTHER_UID,
            OTHER_UID,
            "should_never_download",
            Some(16),
            Some("image/png"),
            None,
        ),
        private_text_update(2204, PRIVATE_CHAT_ID, ALLOWED_UID, "/help"),
    ]);

    let replies = io.wait_message_count(1).await;
    assert_eq!(replies[0].1, HELP_REPLY);
    io.wait_offset(Some(2205)).await;
    assert!(
        io.download_calls().is_empty(),
        "未授权或非私聊图片消息绝不能触发任何 download_file 调用"
    );

    let _ = gw_stop.send(());
    bounded(gw_task).await.unwrap().unwrap();

    let ledger = Ledger::open_and_prune(
        harness.layout.clone(),
        Utc::now(),
        TELEGRAM_REDELIVERY_WINDOW,
    )
    .await
    .unwrap();
    assert_eq!(
        ledger.bound_session(&chat_key_from_id(OTHER_UID)).await,
        None
    );
    assert!(
        !harness.layout.lattice_root().join("lattice.redb").exists(),
        "未授权图片与 /help 不得初始化或写入会话库"
    );

    harness.shutdown().await;
}

// 场景 15：伪装 image/png 的非图片字节、空文件、MAX_IMAGE_BYTES + 1 超限与非法 file_path 均不写台账、不建草稿会话、确认更新并回复清晰中文原因；
// 恰好 MAX_IMAGE_BYTES 的合法图片正常通过并持久化。
#[tokio::test]
async fn rejects_disguised_empty_oversized_and_invalid_path_images_without_ledger_or_draft_session_and_accepts_exact_max_bytes()
 {
    let harness = ImTestHarness::start(&[ALLOWED_UID]).await;
    let io = Arc::new(FakeTelegramIo::default());
    let chat_key = chat_key_from_id(PRIVATE_CHAT_ID);

    io.set_download_bytes("fake_png_pdf", b"%PDF-1.7 disguised as image/png".to_vec());
    io.set_download_bytes("empty_body", Vec::new());
    io.push_download_outcome("stream_over_max", Err(TelegramDownloadError::TooLarge));
    io.push_download_outcome(
        "invalid_rel_path",
        Err(TelegramDownloadError::InvalidFilePath),
    );

    let mut exact_max_png = vec![0xABu8; MAX_IMAGE_BYTES];
    exact_max_png[..8].copy_from_slice(b"\x89PNG\r\n\x1a\n");
    io.set_download_bytes("exact_max_png", exact_max_png.clone());

    let (gw_stop, gw_task) = harness.spawn_gateway(Arc::clone(&io));

    // 投递 6 条非法图片消息（覆盖静态快筛拦截与下载后魔数/空文件/超限/路径错误拦截）
    io.push_updates(vec![
        private_document_update(
            2301,
            PRIVATE_CHAT_ID,
            ALLOWED_UID,
            "fake_png_pdf",
            Some(31),
            Some("image/png"),
            Some("伪装成 PNG 的 PDF"),
        ),
        private_document_update(
            2302,
            PRIVATE_CHAT_ID,
            ALLOWED_UID,
            "static_zero_size",
            Some(0),
            Some("image/png"),
            None,
        ),
        private_document_update(
            2303,
            PRIVATE_CHAT_ID,
            ALLOWED_UID,
            "empty_body",
            None,
            Some("image/png"),
            None,
        ),
        private_document_update(
            2304,
            PRIVATE_CHAT_ID,
            ALLOWED_UID,
            "static_over_max",
            Some(MAX_IMAGE_BYTES as u64 + 1),
            Some("image/png"),
            None,
        ),
        private_photo_update(
            2305,
            PRIVATE_CHAT_ID,
            ALLOWED_UID,
            &[("stream_over_max", None)],
            None,
        ),
        private_photo_update(
            2306,
            PRIVATE_CHAT_ID,
            ALLOWED_UID,
            &[("invalid_rel_path", None)],
            None,
        ),
    ]);

    let reject_msgs = io.wait_message_count(6).await;
    io.wait_offset(Some(2307)).await;

    assert!(
        reject_msgs[0].1.contains("不支持的图片格式"),
        "伪装魔数应返回清晰中文格式错误: {}",
        reject_msgs[0].1
    );
    assert!(
        reject_msgs[1].1.contains("0 字节") && reject_msgs[2].1.contains("0 字节"),
        "静态与下载后空文件均应提示 0 字节: {:?}",
        &reject_msgs[1..=2]
    );
    assert!(
        reject_msgs[3].1.contains("3.75 MiB") && reject_msgs[4].1.contains("3.75 MiB"),
        "静态与流式超单图上限均应提示 3.75 MiB: {:?}",
        &reject_msgs[3..=4]
    );
    assert!(
        reject_msgs[5].1.contains("文件路径无效"),
        "非法 file_path 应提示路径无效: {}",
        reject_msgs[5].1
    );

    // 静态快筛命中的 2302 与 2304 不得触发下载
    let called_ids: Vec<String> = io
        .download_calls()
        .into_iter()
        .map(|(_, id, _)| id)
        .collect();
    assert_eq!(
        called_ids,
        vec![
            "fake_png_pdf".to_string(),
            "empty_body".to_string(),
            "stream_over_max".to_string(),
            "invalid_rel_path".to_string(),
        ],
        "静态快筛拦截的空文件与超限文件绝不能发起 download_file"
    );

    // 验证全部 6 条拒绝消息均未写入台账、未绑定会话，且未调用 configure(New) 创建任何草稿会话
    {
        let ledger = Ledger::open_and_prune(
            harness.layout.clone(),
            Utc::now(),
            TELEGRAM_REDELIVERY_WINDOW,
        )
        .await
        .unwrap();
        assert_eq!(
            ledger.bound_session(&chat_key).await,
            None,
            "图片核验拒绝时不得在台账中建立会话绑定"
        );
        for uid in 2301..=2306 {
            assert_eq!(
                ledger.inbound_record(&inbound_id_from_update_id(uid)).await,
                None,
                "被拒绝的图片更新 {uid} 不得写入台账"
            );
        }
        assert!(
            !harness.layout.lattice_root().join("lattice.redb").exists(),
            "图片核验拒绝时绝不得调用 configure(New) 初始化会话库或遗留空草稿会话"
        );
    }

    // 投递恰好 MAX_IMAGE_BYTES（3,932,160 B）的合法 PNG：必须通过并成功提交到假模型
    io.push_updates(vec![private_document_update(
        2307,
        PRIVATE_CHAT_ID,
        ALLOWED_UID,
        "exact_max_png",
        Some(MAX_IMAGE_BYTES as u64),
        Some("image/png"),
        None,
    )]);

    let (mut sock, _) = bounded(harness.listener.accept()).await.unwrap();
    let req_body = read_http_request(&mut sock).await;
    let req_val: serde_json::Value = serde_json::from_str(&req_body).unwrap();
    let data_url =
        req_val["messages"].as_array().unwrap().last().unwrap()["content"][0]["image_url"]["url"]
            .as_str()
            .expect("应包含 base64 图片 URL");
    let b64_part = data_url
        .strip_prefix("data:image/png;base64,")
        .expect("应使用 image/png 前缀");
    assert_eq!(
        b64_part.len(),
        5 * 1024 * 1024,
        "恰好 3.75 MiB 原始 PNG 的 base64 长度必须精确等于 5 MiB（5,242,880 字符）"
    );
    write_sse_complete(&mut sock, None, "边界上限图片提交成功", None).await;

    let final_msgs = io.wait_message_count(7).await;
    assert_eq!(final_msgs[6].1, "边界上限图片提交成功");
    io.wait_offset(Some(2308)).await;

    let _ = gw_stop.send(());
    bounded(gw_task).await.unwrap().unwrap();
    harness.shutdown().await;
}

// 场景 16：下载重试矩阵与停机中断（start_paused = true）：
// - 429（遵守 retry_after）与 5xx 重试后成功；
// - 重试 3 次耗尽（共 4 次调用）后确认更新、不落账并回复下载失败；
// - 400 与 404 只尝试 1 次不重试；
// - 下载进行中或退避等待期间收到停机信号立即退出且不推进 offset。
#[tokio::test(start_paused = true)]
async fn retries_429_and_5xx_downloads_stops_on_400_404_exhaustion_and_interrupts_without_advancing_offset_on_shutdown()
 {
    let harness = ImTestHarness::start(&[ALLOWED_UID, OTHER_UID]).await;
    let io = Arc::new(FakeTelegramIo::default());
    let valid_png = b"\x89PNG\r\n\x1a\nRETRY_SUCCESS_PNG".to_vec();

    // 1. retry_ok：第 1 次 429（retry_after = 45s）-> 第 2 次 502 -> 第 3 次成功返回图片字节
    io.push_download_outcome(
        "retry_ok",
        Err(TelegramDownloadError::Api(TelegramApiError::RateLimited {
            retry_after: Some(Duration::from_secs(45)),
            diagnostic: "Telegram 返回 HTTP 429".into(),
        })),
    );
    io.push_download_outcome(
        "retry_ok",
        Err(TelegramDownloadError::Api(TelegramApiError::Retryable {
            diagnostic: "Telegram 返回 HTTP 502".into(),
        })),
    );
    io.push_download_outcome("retry_ok", Ok(valid_png.clone()));

    // 2. retry_exhaust：首次 + 3 次重试共 4 次全部返回 5xx 可重试失败
    for _ in 0..5 {
        io.push_download_outcome(
            "retry_exhaust",
            Err(TelegramDownloadError::Api(TelegramApiError::Retryable {
                diagnostic: "Telegram 返回 HTTP 503".into(),
            })),
        );
    }

    // 3. err_400 与 err_404：不可重试错误，必须只尝试 1 次
    io.push_download_outcome(
        "err_400",
        Err(TelegramDownloadError::Api(TelegramApiError::Rejected {
            status: 400,
            diagnostic: "Telegram 返回 HTTP 400：[REDACTED]".into(),
        })),
    );
    io.push_download_outcome(
        "err_404",
        Err(TelegramDownloadError::Api(
            TelegramApiError::UnauthorizedOrInvalid {
                status: 404,
                diagnostic: "Telegram 返回 HTTP 404：[REDACTED]".into(),
            },
        )),
    );

    let (gw_stop1, gw_task1) = harness.spawn_gateway(Arc::clone(&io));

    // 子用例 A：验证 429 retry_after=45s 严格等待后再重试 5xx 并最终成功提交
    io.push_updates(vec![private_photo_update(
        2401,
        PRIVATE_CHAT_ID,
        ALLOWED_UID,
        &[("retry_ok", Some(valid_png.len() as u64))],
        Some("限流与 5xx 重试后成功"),
    )]);

    let calls_1 = io.wait_download_count(1).await;
    let t_attempt_1 = calls_1[0].0;

    tokio::time::advance(Duration::from_secs(44)).await;
    for _ in 0..10 {
        tokio::task::yield_now().await;
    }
    assert_eq!(
        io.download_calls().len(),
        1,
        "429 retry_after=45s 未届满前不得发起第 2 次下载尝试"
    );

    // 补满 45s 触发第 2 次尝试（返回 502），再推进指数退避触发第 3 次成功尝试
    tokio::time::advance(Duration::from_secs(1)).await;
    let calls_2 = io.wait_download_count(2).await;
    assert_eq!(
        calls_2[1].0.duration_since(t_attempt_1),
        Duration::from_secs(45),
        "第 2 次下载尝试必须精确遵守服务端 45 秒 retry_after"
    );

    tokio::time::advance(Duration::from_secs(1)).await;
    let calls_3 = io.wait_download_count(3).await;
    assert_eq!(calls_3.len(), 3);

    let (mut sock, _) = bounded(harness.listener.accept()).await.unwrap();
    let _ = read_http_request(&mut sock).await;
    write_sse_complete(&mut sock, None, "重试下载后模型回复成功", None).await;
    let msgs_a = io.wait_message_count(1).await;
    assert_eq!(msgs_a[0].1, "重试下载后模型回复成功");
    io.wait_offset(Some(2402)).await;

    // 子用例 B：在另一私聊 OTHER_UID 上验证重试 3 次耗尽（总计 4 次尝试）以及 400 / 404 不重试
    io.push_updates(vec![
        private_photo_update(2402, OTHER_UID, OTHER_UID, &[("retry_exhaust", None)], None),
        private_photo_update(2403, OTHER_UID, OTHER_UID, &[("err_400", None)], None),
        private_photo_update(2404, OTHER_UID, OTHER_UID, &[("err_404", None)], None),
    ]);

    let _ = io.wait_download_count(4).await;
    for step_secs in [1u64, 1, 2] {
        tokio::time::advance(Duration::from_secs(step_secs)).await;
        for _ in 0..10 {
            tokio::task::yield_now().await;
        }
    }

    let msgs_b = io.wait_message_count(4).await;
    io.wait_offset(Some(2405)).await;
    assert!(
        msgs_b[1].1.contains("下载图片失败") && !msgs_b[1].1.contains("INTEGRATION_BOT_SECRET"),
        "重试耗尽应回复脱敏中文失败说明: {}",
        msgs_b[1].1
    );
    assert!(
        msgs_b[2].1.contains("HTTP 400") && msgs_b[3].1.contains("HTTP 404"),
        "400 与 404 应回复包含对应状态码的中文失败说明: {:?}",
        &msgs_b[2..=3]
    );

    let all_calls = io.download_calls();
    let count_for = |target: &str| all_calls.iter().filter(|(_, id, _)| id == target).count();
    assert_eq!(
        count_for("retry_exhaust"),
        4,
        "可重试错误必须执行首次尝试加 3 次重试（共 4 次）后停止"
    );
    assert_eq!(count_for("err_400"), 1, "HTTP 400 必须只尝试 1 次不重试");
    assert_eq!(count_for("err_404"), 1, "HTTP 404 必须只尝试 1 次不重试");

    // 子用例 C1：下载请求在途时收到停机信号 -> 立即退出且不确认更新、不推进 offset
    let (dl_entered_rx, _dl_release_tx) = io.install_download_gate();
    io.push_updates(vec![private_photo_update(
        2405,
        OTHER_UID,
        OTHER_UID,
        &[("in_flight_stop", None)],
        None,
    )]);
    bounded(dl_entered_rx)
        .await
        .expect("应进入在途 download_file 闸门");
    let _ = gw_stop1.send(());
    bounded(gw_task1).await.unwrap().unwrap();
    assert!(
        !io.is_acknowledged(&inbound_id_from_update_id(2405)),
        "下载在途被停机中断时绝不能确认该入站更新"
    );
    assert_eq!(
        io.offsets.lock().unwrap().next_offset(),
        Some(2405),
        "下载在途停机中断后 offset 必须停留在 2405 而不越过未完成图片"
    );

    // 子用例 C2：429 长退避等待期间收到停机信号 -> 不等待退避届满立即退出且不推进 offset
    let io2 = Arc::new(FakeTelegramIo::default());
    io2.push_download_outcome(
        "backoff_stop",
        Err(TelegramDownloadError::Api(TelegramApiError::RateLimited {
            retry_after: Some(Duration::from_secs(120)),
            diagnostic: "Telegram 返回 HTTP 429".into(),
        })),
    );
    let (gw_stop2, gw_task2) = harness.spawn_gateway(Arc::clone(&io2));
    io2.push_updates(vec![private_photo_update(
        2410,
        OTHER_UID,
        OTHER_UID,
        &[("backoff_stop", None)],
        None,
    )]);
    let _ = io2.wait_download_count(1).await;
    for _ in 0..10 {
        tokio::task::yield_now().await;
    }
    let stop_at = tokio::time::Instant::now();
    let _ = gw_stop2.send(());
    bounded(gw_task2).await.unwrap().unwrap();
    assert!(
        tokio::time::Instant::now().duration_since(stop_at) < Duration::from_secs(5),
        "退避等待期间收到停机信号必须立即中断睡眠退出，不得等待 120s retry_after"
    );
    assert_eq!(io2.download_calls().len(), 1);
    assert!(
        !io2.is_acknowledged(&inbound_id_from_update_id(2410)),
        "退避等待期间停机中断绝不能确认该入站更新"
    );
    assert_eq!(io2.offsets.lock().unwrap().next_offset(), None);

    // 核对 OTHER_UID 上的失败与中断更新均未污染台账
    let ledger = Ledger::open_and_prune(
        harness.layout.clone(),
        Utc::now(),
        TELEGRAM_REDELIVERY_WINDOW,
    )
    .await
    .unwrap();
    assert_eq!(
        ledger.bound_session(&chat_key_from_id(OTHER_UID)).await,
        None,
        "全部下载失败或中断的对话不得在台账中留下会话绑定"
    );
    for uid in [2402, 2403, 2404, 2405, 2410] {
        assert_eq!(
            ledger.inbound_record(&inbound_id_from_update_id(uid)).await,
            None,
            "下载失败或中断的更新 {uid} 不得写入台账"
        );
    }

    harness.shutdown().await;
}

fn private_album_photo_update(
    update_id: i64,
    chat_id: i64,
    sender_id: i64,
    media_group_id: &str,
    photos: &[(&str, Option<u64>)],
    caption: Option<&str>,
) -> TelegramUpdate {
    let photo_arr: Vec<serde_json::Value> = photos
        .iter()
        .enumerate()
        .map(|(idx, (file_id, file_size))| {
            let mut obj = serde_json::json!({
                "file_id": file_id,
                "file_unique_id": format!("u_alb_{update_id}_{idx}"),
                "width": (idx + 1) * 100,
                "height": (idx + 1) * 100,
            });
            if let Some(size) = file_size {
                obj["file_size"] = serde_json::json!(size);
            }
            obj
        })
        .collect();
    let mut msg = serde_json::json!({
        "message_id": update_id,
        "from": {"id": sender_id},
        "chat": {"id": chat_id, "type": "private"},
        "media_group_id": media_group_id,
        "photo": photo_arr,
    });
    if let Some(cap) = caption {
        msg["caption"] = serde_json::json!(cap);
    }
    let raw: RawTelegramUpdate = serde_json::from_value(serde_json::json!({
        "update_id": update_id,
        "message": msg,
    }))
    .expect("构造私聊相册图片更新应成功");
    raw.into_domain()
}

fn private_album_unsupported_update(
    update_id: i64,
    chat_id: i64,
    sender_id: i64,
    media_group_id: &str,
    kind_field: &str,
) -> TelegramUpdate {
    let mut msg = serde_json::json!({
        "message_id": update_id,
        "from": {"id": sender_id},
        "chat": {"id": chat_id, "type": "private"},
        "media_group_id": media_group_id,
    });
    msg[kind_field] = serde_json::json!({
        "file_id": format!("unsup_{kind_field}_{update_id}"),
        "width": 320,
        "height": 240,
    });
    let raw: RawTelegramUpdate = serde_json::from_value(serde_json::json!({
        "update_id": update_id,
        "message": msg,
    }))
    .expect("构造相册混入不支持媒体更新应成功");
    raw.into_domain()
}

// 场景 17（相册测试 1）：两批乱序到达（100 后 99，均带不同说明文字与图片）在 600ms 静默到期合并为一轮提交，
// 假模型收到 [Image(99), Image(100), Text("cap99\ncap100")]；
// 结合 arm_replace_gate_for_test 验证台账落盘前 99 与 100 均未确认，落盘后一次性推进 offset 到 101 且磁盘写入 tg:99 -> Registered、tg:100 -> Merged。
#[tokio::test(start_paused = true)]
async fn merges_out_of_order_album_batches_after_600ms_and_acks_atomically_after_ledger_replace() {
    let harness = ImTestHarness::start(&[ALLOWED_UID]).await;
    let io = Arc::new(FakeTelegramIo::default());

    let png_99 = b"\x89PNG\r\n\x1a\nALBUM_IMAGE_99_BYTES".to_vec();
    let jpeg_100 = b"\xff\xd8\xff\xe0\x00\x10ALBUM_IMAGE_100_BYTES".to_vec();
    io.set_download_bytes("img_99", png_99.clone());
    io.set_download_bytes("img_100", jpeg_100.clone());

    let (reg_entered_rx, reg_release_tx, reg_finished_rx) =
        crate::ledger::arm_replace_gate_for_test(harness.layout.lattice_root().to_path_buf());

    let (gw_stop, gw_task) = harness.spawn_gateway(Arc::clone(&io));
    let _ = io.wait_poll_count(1).await;

    // 第 1 批先到达 update_id = 100
    io.push_updates(vec![private_album_photo_update(
        100,
        PRIVATE_CHAT_ID,
        ALLOWED_UID,
        "album-out-of-order",
        &[("img_100", Some(jpeg_100.len() as u64))],
        Some("cap100"),
    )]);
    let _ = io.wait_poll_count(2).await;

    // 间隔 200ms 后第 2 批到达数值更小的 update_id = 99（重置 600ms 静默截止）
    tokio::time::advance(Duration::from_millis(200)).await;
    io.push_updates(vec![private_album_photo_update(
        99,
        PRIVATE_CHAT_ID,
        ALLOWED_UID,
        "album-out-of-order",
        &[("img_99", Some(png_99.len() as u64))],
        Some("cap99"),
    )]);
    let _ = io.wait_poll_count(3).await;

    // 第 2 批到达后 599ms 仍未满 600ms 静默窗口，不得提前下载或确认
    tokio::time::advance(Duration::from_millis(599)).await;
    for _ in 0..10 {
        tokio::task::yield_now().await;
    }
    assert!(
        io.download_calls().is_empty(),
        "静默窗口满 600ms 前不得提前闭合相册发起下载"
    );
    assert!(!io.is_acknowledged(&inbound_id_from_update_id(99)));
    assert!(!io.is_acknowledged(&inbound_id_from_update_id(100)));

    // 补满第 600ms：触发唤醒闭合、按数值升序下载 [img_99, img_100]，并停在 register_turn 落盘替换前闸门
    tokio::time::advance(Duration::from_millis(1)).await;
    bounded(reg_entered_rx)
        .await
        .expect("相册闭合完成下载核验后应进入 register_turn 台账落盘闸门");

    let dl_ids: Vec<String> = io
        .download_calls()
        .into_iter()
        .map(|(_, id, _)| id)
        .collect();
    assert_eq!(
        dl_ids,
        vec!["img_99".to_string(), "img_100".to_string()],
        "乱序到达的相册分片必须按数值 update_id 升序依次下载"
    );

    // 验证台账原子替换落盘前：99 与 100 均未确认，offset 仍为 None
    assert!(
        !io.is_acknowledged(&inbound_id_from_update_id(99))
            && !io.is_acknowledged(&inbound_id_from_update_id(100)),
        "台账原子落盘完成前绝不能确认任何相册分片"
    );
    assert_eq!(io.offsets.lock().unwrap().next_offset(), None);

    // 紧接着武装下一次台账写入闸门（拦截 mark_accepted），以便直接检查 register_turn 落盘后的磁盘状态
    let (acc_entered_rx, acc_release_tx, _) =
        crate::ledger::arm_replace_gate_for_test(harness.layout.lattice_root().to_path_buf());
    reg_release_tx.send(()).unwrap();
    bounded(reg_finished_rx)
        .await
        .expect("register_turn 原子替换应完成");
    bounded(acc_entered_rx)
        .await
        .expect("submit 接受后应停在 mark_accepted 替换前闸门");

    // 此时 register_turn 已落盘：99 与 100 已一次性确认，连续前缀 offset 推进到 101，且磁盘保存 Registered + Merged
    assert!(io.is_acknowledged(&inbound_id_from_update_id(99)));
    assert!(io.is_acknowledged(&inbound_id_from_update_id(100)));
    assert_eq!(io.offsets.lock().unwrap().next_offset(), Some(101));
    io.wait_offset(Some(101)).await;

    {
        let on_disk = Ledger::open_and_prune(
            harness.layout.clone(),
            Utc::now(),
            TELEGRAM_REDELIVERY_WINDOW,
        )
        .await
        .unwrap();
        let rec_99 = on_disk
            .inbound_record(&inbound_id_from_update_id(99))
            .await
            .expect("磁盘台账必须包含首条 tg:99");
        let rec_100 = on_disk
            .inbound_record(&inbound_id_from_update_id(100))
            .await
            .expect("磁盘台账必须包含并入分片 tg:100");
        match (rec_99, rec_100) {
            (
                InboundRecord::Registered {
                    chat: c1,
                    session: s1,
                    turn: t1,
                },
                InboundRecord::Merged {
                    chat: c2,
                    session: s2,
                    turn: t2,
                    ..
                },
            ) => {
                assert_eq!(c1, chat_key_from_id(PRIVATE_CHAT_ID));
                assert_eq!(c1, c2);
                assert_eq!(s1, s2, "Registered 与 Merged 必须关联同一个 SessionId");
                assert_eq!(t1, t2, "Registered 与 Merged 必须关联同一个 TurnId");
            }
            other => panic!("register_turn 落盘后应为 (Registered, Merged)，实际: {other:?}"),
        }
    }

    acc_release_tx.send(()).unwrap();

    // 核对假模型收到的单轮消息严格为 [Image(99), Image(100), Text("cap99\ncap100")]
    let (mut sock, _) = bounded(harness.listener.accept()).await.unwrap();
    let req_json: serde_json::Value =
        serde_json::from_str(&read_http_request(&mut sock).await).unwrap();
    let last_user = req_json["messages"].as_array().unwrap().last().unwrap();
    assert_eq!(
        last_user["content"],
        serde_json::json!([
            {
                "type": "image_url",
                "image_url": {
                    "url": format!("data:image/png;base64,{}", test_base64_encode(&png_99))
                }
            },
            {
                "type": "image_url",
                "image_url": {
                    "url": format!("data:image/jpeg;base64,{}", test_base64_encode(&jpeg_100))
                }
            },
            {
                "type": "text",
                "text": "cap99\ncap100"
            }
        ]),
        "相册闭合后必须按数值 update_id 升序发送全部图片与换行拼接的说明文字"
    );
    write_sse_complete(&mut sock, None, "乱序相册合并回复", None).await;

    let replies = io.wait_message_count(1).await;
    assert_eq!(replies[0].1, "乱序相册合并回复");

    let _ = gw_stop.send(());
    bounded(gw_task).await.unwrap().unwrap();

    let final_ledger = Ledger::open_and_prune(
        harness.layout.clone(),
        Utc::now(),
        TELEGRAM_REDELIVERY_WINDOW,
    )
    .await
    .unwrap();
    assert!(matches!(
        final_ledger
            .inbound_record(&inbound_id_from_update_id(99))
            .await,
        Some(InboundRecord::DeliveredTurn { .. })
    ));
    assert!(matches!(
        final_ledger
            .inbound_record(&inbound_id_from_update_id(100))
            .await,
        Some(InboundRecord::Merged { .. })
    ));

    harness.shutdown().await;
}

// 场景 18（相册测试 2）：重投已缓冲分片不重置 600ms 截止，过滤后零新更新的空轮询按 100ms 节流（核对 poll_timestamps 间隔），
// 且轮询错误退避（如 429 retry_after）期间到期的开放相册照常按时闭合。
#[tokio::test(start_paused = true)]
async fn redelivered_buffered_fragment_never_resets_deadline_and_throttles_idle_polls_at_100ms() {
    let harness = ImTestHarness::start(&[ALLOWED_UID]).await;
    let io = Arc::new(FakeTelegramIo::default());
    let png = b"\x89PNG\r\n\x1a\nTHROTTLE_ALBUM_PNG".to_vec();
    io.set_download_bytes("f_throttle", png.clone());
    io.set_download_bytes("f_backoff_wake", png);

    let (gw_stop, gw_task) = harness.spawn_gateway(Arc::clone(&io));
    let _ = io.wait_poll_count(1).await;
    let t0 = io.poll_timestamps.lock().unwrap()[0];

    // t0 收到首个相册分片 3101（截止时刻设为 t0 + 600ms）；有新更新不节流，立即进入第 2 次轮询
    let frag_3101 = private_album_photo_update(
        3101,
        PRIVATE_CHAT_ID,
        ALLOWED_UID,
        "g-throttle",
        &[("f_throttle", Some(26))],
        Some("节流验证"),
    );
    io.push_updates(vec![frag_3101.clone()]);
    let _ = io.wait_poll_count(2).await;
    assert_eq!(
        io.poll_timestamps.lock().unwrap()[1].duration_since(t0),
        Duration::ZERO,
        "包含新分片的轮询批次不得触发 100ms 节流"
    );

    // t0 + 250ms 平台重投未确认的已缓冲分片 3101：过滤后无新更新，必须按 100ms 节流且不重置截止时刻
    tokio::time::advance(Duration::from_millis(250)).await;
    io.push_updates(vec![frag_3101.clone()]);
    for _ in 0..15 {
        tokio::task::yield_now().await;
    }
    assert_eq!(
        io.poll_timestamps.lock().unwrap().len(),
        2,
        "过滤后零新更新时不得无等待紧循环发起下一轮 getUpdates"
    );

    tokio::time::advance(Duration::from_millis(99)).await;
    for _ in 0..10 {
        tokio::task::yield_now().await;
    }
    assert_eq!(
        io.poll_timestamps.lock().unwrap().len(),
        2,
        "100ms 节流未届满（99ms）前不得提前发起第 3 次轮询"
    );

    tokio::time::advance(Duration::from_millis(1)).await;
    let _ = io.wait_poll_count(3).await;
    assert_eq!(
        io.poll_timestamps.lock().unwrap()[2].duration_since(t0),
        Duration::from_millis(350),
        "第 3 次轮询时间戳必须精确落在重投响应后 100ms（t0 + 350ms）"
    );

    // t0 + 350ms 返回空更新列表：同样触发 100ms 节流，第 4 次轮询落在 t0 + 450ms
    io.push_updates(Vec::new());
    for _ in 0..15 {
        tokio::task::yield_now().await;
    }
    tokio::time::advance(Duration::from_millis(99)).await;
    for _ in 0..10 {
        tokio::task::yield_now().await;
    }
    assert_eq!(io.poll_timestamps.lock().unwrap().len(), 3);
    tokio::time::advance(Duration::from_millis(1)).await;
    let _ = io.wait_poll_count(4).await;
    {
        let timestamps = io.poll_timestamps.lock().unwrap();
        assert_eq!(
            timestamps[3].duration_since(timestamps[2]),
            Duration::from_millis(100),
            "开放相册期间空批次轮询间隔必须精确为 100ms"
        );
    }

    // t0 + 540ms（距原定 t0 + 600ms 仅剩 60ms）再次重投 3101：节流收敛为 min(100ms, 60ms) = 60ms，
    // 且相册在原定 t0 + 600ms 准时闭合（证明两次重投均未重置 600ms 窗口）！
    tokio::time::advance(Duration::from_millis(90)).await;
    io.push_updates(vec![frag_3101]);
    for _ in 0..15 {
        tokio::task::yield_now().await;
    }
    tokio::time::advance(Duration::from_millis(59)).await;
    for _ in 0..10 {
        tokio::task::yield_now().await;
    }
    assert!(
        io.download_calls().is_empty(),
        "t0 + 599ms 尚未到达原定 600ms 截止"
    );

    tokio::time::advance(Duration::from_millis(1)).await;
    let dl_calls = io.wait_download_count(1).await;
    assert_eq!(
        dl_calls[0].0.duration_since(t0),
        Duration::from_millis(600),
        "重投已缓冲分片不得推迟原定 t0 + 600ms 相册闭合时刻"
    );

    let (mut sock1, _) = bounded(harness.listener.accept()).await.unwrap();
    let _ = read_http_request(&mut sock1).await;
    write_sse_complete(&mut sock1, None, "节流相册回复", None).await;
    let msgs1 = io.wait_message_count(1).await;
    assert_eq!(msgs1[0].1, "节流相册回复");
    io.wait_offset(Some(3102)).await;

    // 附加核对：存在开放相册时下一轮 getUpdates 返回 429 retry_after=30s，600ms 到期照常唤醒闭合相册，且总退避仍守满 30s
    let poll_idx_before_err = io.poll_timestamps.lock().unwrap().len();
    let t_err_start = tokio::time::Instant::now();
    io.push_updates(vec![private_album_photo_update(
        3102,
        PRIVATE_CHAT_ID,
        ALLOWED_UID,
        "g-backoff-wake",
        &[("f_backoff_wake", Some(26))],
        None,
    )]);
    io.push_poll_error(TelegramApiError::RateLimited {
        retry_after: Some(Duration::from_secs(30)),
        diagnostic: "Telegram 返回 HTTP 429".into(),
    });
    let _ = io.wait_poll_count(poll_idx_before_err + 1).await;
    for _ in 0..15 {
        tokio::task::yield_now().await;
    }

    tokio::time::advance(Duration::from_millis(600)).await;
    let dl_calls_2 = io.wait_download_count(2).await;
    assert_eq!(
        dl_calls_2[1].0.duration_since(t_err_start),
        Duration::from_millis(600),
        "getUpdates 处于 429 退避期间也必须在 600ms 到期时准时闭合开放相册"
    );
    let (mut sock2, _) = bounded(harness.listener.accept()).await.unwrap();
    let _ = read_http_request(&mut sock2).await;
    write_sse_complete(&mut sock2, None, "退避期间闭合回复", None).await;
    let msgs2 = io.wait_message_count(2).await;
    assert_eq!(msgs2[1].1, "退避期间闭合回复");
    assert_eq!(
        io.poll_timestamps.lock().unwrap().len(),
        poll_idx_before_err + 1,
        "30s retry_after 未满前不得提前发起下一轮 getUpdates"
    );

    let _ = gw_stop.send(());
    bounded(gw_task).await.unwrap().unwrap();
    harness.shutdown().await;
}

// 场景 19（相册测试 3）：提前闭合保序：
// - 开放相册后紧跟同对话普通文本或 /new 立即提前闭合相册，且单对话 FIFO 先提交相册再执行后续文本或 /new；
// - 跨对话开放相册互不干扰；
// - stopped_message_generation 不提前闭合开放相册。
#[tokio::test(start_paused = true)]
async fn early_closes_album_preserving_fifo_before_text_or_new_isolates_chats_and_ignores_stop_draft()
 {
    let harness = ImTestHarness::start(&[ALLOWED_UID, OTHER_UID]).await;
    let io = Arc::new(FakeTelegramIo::default());
    let chat_a = chat_key_from_id(PRIVATE_CHAT_ID);

    let img_a1 = b"\x89PNG\r\n\x1a\nEARLY_CLOSE_A1".to_vec();
    let img_a2 = b"\x89PNG\r\n\x1a\nEARLY_CLOSE_A2".to_vec();
    let img_b1 = b"\x89PNG\r\n\x1a\nOTHER_CHAT_B1".to_vec();
    io.set_download_bytes("a_img1", img_a1.clone());
    io.set_download_bytes("a_img2", img_a2.clone());
    io.set_download_bytes("b_img1", img_b1);

    let (gw_stop, gw_task) = harness.spawn_gateway(Arc::clone(&io));
    let _ = io.wait_poll_count(1).await;
    let t0 = tokio::time::Instant::now();

    // 1. chat_a 与 chat_b 各自打开一个相册，随后 chat_a 收到 stopped_message_generation：两者均不得提前闭合
    io.push_updates(vec![
        private_album_photo_update(
            3201,
            PRIVATE_CHAT_ID,
            ALLOWED_UID,
            "ga-1",
            &[("a_img1", Some(img_a1.len() as u64))],
            Some("相册A1说明"),
        ),
        private_album_photo_update(
            3202,
            OTHER_UID,
            OTHER_UID,
            "gb-1",
            &[("b_img1", Some(21))],
            Some("跨对话B1说明"),
        ),
        stop_draft_update(3203, PRIVATE_CHAT_ID, 98765),
    ]);
    let _ = io.wait_poll_count(2).await;

    tokio::time::advance(Duration::from_millis(100)).await;
    for _ in 0..10 {
        tokio::task::yield_now().await;
    }
    assert!(
        io.download_calls().is_empty(),
        "stopped_message_generation 绝不能提前闭合当前开放相册"
    );

    // 2. 在 t0 + 100ms（未满 600ms）向 chat_a 发送普通文本消息 3204：
    //    必须立即闭合 ga-1 并按 FIFO 先提交 ga-1 创建会话，紧随其后的 3204 在 ga-1 执行期间撞上同一会话 Busy；
    //    同时 chat_b 的 gb-1 仍保持开放不受影响！
    io.push_updates(vec![private_text_update(
        3204,
        PRIVATE_CHAT_ID,
        ALLOWED_UID,
        "紧随相册的同对话文本",
    )]);

    let dl_a1 = io.wait_download_count(1).await;
    assert_eq!(dl_a1[0].1, "a_img1");
    assert_eq!(
        dl_a1[0].0.duration_since(t0),
        Duration::from_millis(100),
        "同对话普通文本到达时必须立即提前闭合开放相册，不等待 600ms"
    );

    let (mut sock_a1, _) = bounded(harness.listener.accept()).await.unwrap();
    let req_a1: serde_json::Value =
        serde_json::from_str(&read_http_request(&mut sock_a1).await).unwrap();
    let user_a1 = req_a1["messages"].as_array().unwrap().last().unwrap();
    assert_eq!(
        user_a1["content"][0]["image_url"]["url"],
        format!("data:image/png;base64,{}", test_base64_encode(&img_a1)),
        "FIFO 队首必须先提交刚闭合的相册 ga-1，而非后续文本消息"
    );

    // ga-1 尚未结束时，排在其后的文本 3204 复用同一会话提交并收到 Busy
    let mid_msgs = io.wait_message_count(1).await;
    assert_eq!(mid_msgs[0].0, chat_a);
    assert_eq!(mid_msgs[0].1, "正在忙，可发 /cancel");
    assert_eq!(
        io.download_calls().len(),
        1,
        "chat_a 提前闭合不得干扰跨对话 chat_b 的开放相册 gb-1"
    );

    write_sse_complete(&mut sock_a1, None, "相册A1完成回复", None).await;
    let msgs_after_a1 = io.wait_message_count(2).await;
    assert_eq!(msgs_after_a1[1].1, "相册A1完成回复");

    // 推进剩余 500ms 至 t0 + 600ms：chat_b 的 gb-1 按自身静默窗口到期闭合提交
    tokio::time::advance(Duration::from_millis(500)).await;
    let dl_b1 = io.wait_download_count(2).await;
    assert_eq!(dl_b1[1].1, "b_img1");
    let (mut sock_b1, _) = bounded(harness.listener.accept()).await.unwrap();
    let _ = read_http_request(&mut sock_b1).await;
    write_sse_complete(&mut sock_b1, None, "跨对话相册B1回复", None).await;
    let msgs_after_b1 = io.wait_message_count(3).await;
    assert_eq!(msgs_after_b1[2].1, "跨对话相册B1回复");

    let sid_before_new = Ledger::open_and_prune(
        harness.layout.clone(),
        Utc::now(),
        TELEGRAM_REDELIVERY_WINDOW,
    )
    .await
    .unwrap()
    .bound_session(&chat_a)
    .await
    .expect("chat_a 应持有 ga-1 创建的会话绑定");

    // 3. 开放相册 ga-2 后同批紧跟 /new：必须先将 ga-2 提交到原绑定会话 sid_before_new，再执行 /new 清空绑定！
    io.push_updates(vec![
        private_album_photo_update(
            3210,
            PRIVATE_CHAT_ID,
            ALLOWED_UID,
            "ga-2",
            &[("a_img2", Some(img_a2.len() as u64))],
            Some("重置前相册A2"),
        ),
        private_text_update(3211, PRIVATE_CHAT_ID, ALLOWED_UID, "/new"),
    ]);

    let (mut sock_a2, _) = bounded(harness.listener.accept()).await.unwrap();
    let req_a2: serde_json::Value =
        serde_json::from_str(&read_http_request(&mut sock_a2).await).unwrap();
    let msgs_arr_a2 = req_a2["messages"].as_array().unwrap();
    assert!(
        msgs_arr_a2.iter().any(|m| m["content"] == "相册A1完成回复"),
        "紧跟 /new 前闭合的相册 ga-2 必须仍在旧会话 sid_before_new 中提交并携带旧历史"
    );
    write_sse_complete(&mut sock_a2, None, "相册A2完成回复", None).await;

    let final_msgs = io.wait_message_count(5).await;
    let tail_texts: Vec<&str> = final_msgs[3..].iter().map(|(_, t)| t.as_str()).collect();
    assert!(tail_texts.contains(&"已清除当前会话绑定，下一条消息将新建会话"));
    assert!(tail_texts.contains(&"相册A2完成回复"));
    io.wait_offset(Some(3212)).await;

    let _ = gw_stop.send(());
    bounded(gw_task).await.unwrap().unwrap();

    let final_ledger = Ledger::open_and_prune(
        harness.layout.clone(),
        Utc::now(),
        TELEGRAM_REDELIVERY_WINDOW,
    )
    .await
    .unwrap();
    assert_eq!(
        final_ledger.bound_session(&chat_a).await,
        None,
        "相册 ga-2 准入后排在后面的 /new 必须已清除 chat_a 的会话绑定"
    );
    match final_ledger
        .inbound_record(&inbound_id_from_update_id(3210))
        .await
    {
        Some(InboundRecord::DeliveredTurn { session, .. }) => {
            assert_eq!(session, sid_before_new);
        }
        other => panic!("3210 应记录在原会话上并完成投递，实际: {other:?}"),
    }

    harness.shutdown().await;
}

// 场景 20（相册测试 4）：整组拒绝：
// - 9 张相册（静默到期闭合）与 10 张相册（第 10 张立即闭合）均按超过 8 张整组拒绝、零下载、只回 1 条提示；
// - 混有视频/贴纸的相册整组拒绝 1 次、零下载、回复 UNSUPPORTED_MEDIA_REPLY。
#[tokio::test(start_paused = true)]
async fn rejects_9_and_10_image_albums_and_mixed_unsupported_albums_once_with_zero_downloads() {
    let harness = ImTestHarness::start(&[ALLOWED_UID]).await;
    let io = Arc::new(FakeTelegramIo::default());
    io.set_download_bytes("never_dl", b"\x89PNG\r\n\x1a\nNEVER".to_vec());

    let (gw_stop, gw_task) = harness.spawn_gateway(Arc::clone(&io));
    let _ = io.wait_poll_count(1).await;

    // 1. 9 张相册：不在第 8 张截断，等待 600ms 静默到期后整组拒绝 1 次
    let nine_updates: Vec<_> = (0..9)
        .map(|i| {
            private_album_photo_update(
                3301 + i,
                PRIVATE_CHAT_ID,
                ALLOWED_UID,
                "album-nine",
                &[("never_dl", Some(64))],
                None,
            )
        })
        .collect();
    io.push_updates(nine_updates);
    let _ = io.wait_poll_count(2).await;

    tokio::time::advance(Duration::from_millis(599)).await;
    for _ in 0..10 {
        tokio::task::yield_now().await;
    }
    assert!(
        io.sent_messages.lock().unwrap().is_empty(),
        "9 张相册在 599ms 时仍处于收集中，不得在第 8 张处提前截断"
    );

    tokio::time::advance(Duration::from_millis(1)).await;
    let msgs_after_nine = io.wait_message_count(1).await;
    assert_eq!(msgs_after_nine.len(), 1, "9 张超限相册必须只回复 1 条提示");
    assert!(
        msgs_after_nine[0].1.contains("最多支持 8 张") && msgs_after_nine[0].1.contains("9 张"),
        "9 张超限提示应说明上限与当前张数: {}",
        msgs_after_nine[0].1
    );
    io.wait_offset(Some(3310)).await;

    // 2. 10 张相册：收满第 10 张立即闭合（无需等待 600ms），整组拒绝 1 次
    let t_before_ten = tokio::time::Instant::now();
    let ten_updates: Vec<_> = (0..10)
        .map(|i| {
            private_album_photo_update(
                3311 + i,
                PRIVATE_CHAT_ID,
                ALLOWED_UID,
                "album-ten",
                &[("never_dl", Some(64))],
                None,
            )
        })
        .collect();
    io.push_updates(ten_updates);
    let msgs_after_ten = io.wait_message_count(2).await;
    assert_eq!(
        tokio::time::Instant::now().duration_since(t_before_ten),
        Duration::ZERO,
        "10 张相册必须在收到第 10 张时立即闭合拒绝，无需等待 600ms"
    );
    assert!(
        msgs_after_ten[1].1.contains("最多支持 8 张") && msgs_after_ten[1].1.contains("10 张"),
        "10 张超限提示应说明上限与当前张数: {}",
        msgs_after_ten[1].1
    );
    io.wait_offset(Some(3321)).await;

    // 3. 混有图片 + 视频 + 贴纸的相册：收集期内不逐条报错，600ms 闭合后整组拒绝 1 次并回复 UNSUPPORTED_MEDIA_REPLY
    io.push_updates(vec![
        private_album_photo_update(
            3331,
            PRIVATE_CHAT_ID,
            ALLOWED_UID,
            "album-mixed",
            &[("never_dl", Some(64))],
            Some("混排说明"),
        ),
        private_album_unsupported_update(
            3332,
            PRIVATE_CHAT_ID,
            ALLOWED_UID,
            "album-mixed",
            "video",
        ),
        private_album_unsupported_update(
            3333,
            PRIVATE_CHAT_ID,
            ALLOWED_UID,
            "album-mixed",
            "sticker",
        ),
    ]);
    let poll_target = io.observed_offsets.lock().unwrap().len() + 1;
    let _ = io.wait_poll_count(poll_target).await;
    tokio::time::advance(Duration::from_millis(599)).await;
    for _ in 0..10 {
        tokio::task::yield_now().await;
    }
    assert_eq!(
        io.sent_messages.lock().unwrap().len(),
        2,
        "携带 media_group_id 的不支持媒体分片在相册闭合前不得单独回复提示"
    );

    tokio::time::advance(Duration::from_millis(1)).await;
    let msgs_after_mixed = io.wait_message_count(3).await;
    assert_eq!(msgs_after_mixed.len(), 3);
    assert_eq!(msgs_after_mixed[2].1, UNSUPPORTED_MEDIA_REPLY);
    io.wait_offset(Some(3334)).await;

    assert!(
        io.download_calls().is_empty(),
        "9 张、10 张超限与混入视频/贴纸的相册均须在静态快筛拦截，零文件下载"
    );

    let _ = gw_stop.send(());
    bounded(gw_task).await.unwrap().unwrap();

    let ledger = Ledger::open_and_prune(
        harness.layout.clone(),
        Utc::now(),
        TELEGRAM_REDELIVERY_WINDOW,
    )
    .await
    .unwrap();
    assert_eq!(
        ledger
            .bound_session(&chat_key_from_id(PRIVATE_CHAT_ID))
            .await,
        None,
        "整组拒绝的相册不得写入台账或创建草稿会话"
    );
    for uid in (3301..=3309).chain(3311..=3320).chain(3331..=3333) {
        assert_eq!(
            ledger.inbound_record(&inbound_id_from_update_id(uid)).await,
            None
        );
    }
    assert!(!harness.layout.lattice_root().join("lattice.redb").exists());

    harness.shutdown().await;
}

// 场景 21（相册测试 5）：迟到尾图 vs Merged 重投：
// - 相册闭合落账后，同 media_group_id 的新 update_id 回复 LATE_ALBUM_FRAGMENT_REPLY，不下载、不落账、不开启新轮次；
// - 重投已落账的 Merged update_id（即使同 media_group_id 也在近期已闭合集合中）静默确认、零回复、不开启新轮次。
#[tokio::test(start_paused = true)]
async fn replies_late_fragment_notice_for_new_update_id_and_silently_acks_redelivered_merged_id() {
    let harness = ImTestHarness::start(&[ALLOWED_UID]).await;
    let io1 = Arc::new(FakeTelegramIo::default());
    let png_1 = b"\x89PNG\r\n\x1a\nLATE_TEST_1".to_vec();
    let png_2 = b"\x89PNG\r\n\x1a\nLATE_TEST_2".to_vec();
    io1.set_download_bytes("late_f1", png_1);
    io1.set_download_bytes("late_f2", png_2);
    io1.set_download_bytes("late_tail_f3", b"\x89PNG\r\n\x1a\nNEVER_DL_LATE".to_vec());

    let (gw_stop1, gw_task1) = harness.spawn_gateway(Arc::clone(&io1));
    let _ = io1.wait_poll_count(1).await;

    io1.push_updates(vec![
        private_album_photo_update(
            3401,
            PRIVATE_CHAT_ID,
            ALLOWED_UID,
            "album-late-check",
            &[("late_f1", Some(19))],
            Some("正常到达"),
        ),
        private_album_photo_update(
            3402,
            PRIVATE_CHAT_ID,
            ALLOWED_UID,
            "album-late-check",
            &[("late_f2", Some(19))],
            None,
        ),
    ]);
    let _ = io1.wait_poll_count(2).await;
    tokio::time::advance(Duration::from_millis(600)).await;

    let (mut sock, _) = bounded(harness.listener.accept()).await.unwrap();
    let _ = read_http_request(&mut sock).await;
    write_sse_complete(&mut sock, None, "首轮正常相册回复", None).await;
    let msgs1 = io1.wait_message_count(1).await;
    assert_eq!(msgs1[0].1, "首轮正常相册回复");
    io1.wait_offset(Some(3403)).await;
    assert_eq!(io1.download_calls().len(), 2);

    // 1. 同一实例内到达同 media_group_id 的迟到新分片 3403：立即确认并回复迟到提示，零下载、不触发模型新轮次
    io1.push_updates(vec![private_album_photo_update(
        3403,
        PRIVATE_CHAT_ID,
        ALLOWED_UID,
        "album-late-check",
        &[("late_tail_f3", Some(21))],
        Some("迟到的第3张"),
    )]);
    let msgs_after_late = io1.wait_message_count(2).await;
    assert_eq!(msgs_after_late[1].1, LATE_ALBUM_FRAGMENT_REPLY);
    io1.wait_offset(Some(3404)).await;
    assert_eq!(
        io1.download_calls().len(),
        2,
        "迟到分片不得发起 download_file"
    );

    let _ = gw_stop1.send(());
    bounded(gw_task1).await.unwrap().unwrap();

    // 核对 3402 为 Merged，而迟到分片 3403 未写入台账
    {
        let snap = Ledger::open_and_prune(
            harness.layout.clone(),
            Utc::now(),
            TELEGRAM_REDELIVERY_WINDOW,
        )
        .await
        .unwrap();
        assert!(matches!(
            snap.inbound_record(&inbound_id_from_update_id(3402)).await,
            Some(InboundRecord::Merged { .. })
        ));
        assert_eq!(
            snap.inbound_record(&inbound_id_from_update_id(3403)).await,
            None,
            "迟到分片不得写入台账"
        );
    }

    // 2. 启动新实例（全新 OffsetTracker）：重投已落账的 [3401 (DeliveredTurn), 3402 (Merged)] -> 静默确认推进 offset 至 3403，零回复、零下载
    let io2 = Arc::new(FakeTelegramIo::default());
    io2.set_download_bytes("reopen_f", b"\x89PNG\r\n\x1a\nREOPEN".to_vec());
    let (gw_stop2, gw_task2) = harness.spawn_gateway(Arc::clone(&io2));
    let _ = io2.wait_poll_count(1).await;

    io2.push_updates(vec![
        private_album_photo_update(
            3401,
            PRIVATE_CHAT_ID,
            ALLOWED_UID,
            "album-late-check",
            &[("reopen_f", Some(14))],
            None,
        ),
        private_album_photo_update(
            3402,
            PRIVATE_CHAT_ID,
            ALLOWED_UID,
            "album-late-check",
            &[("reopen_f", Some(14))],
            None,
        ),
    ]);
    io2.wait_offset(Some(3403)).await;
    for _ in 0..15 {
        tokio::task::yield_now().await;
    }
    assert!(
        io2.sent_messages.lock().unwrap().is_empty(),
        "重投已落账的 DeliveredTurn 与 Merged 分片必须静默确认，严禁发送迟到提示或开启新轮次"
    );
    assert!(io2.download_calls().is_empty());

    let _ = gw_stop2.send(());
    bounded(gw_task2).await.unwrap().unwrap();
    harness.shutdown().await;
}

// 场景 22（相册测试 6）：停机与恢复：
// - 600ms 窗口内停机不确认未闭合分片、不推进 offset，新实例启动后接收完整重投并成功合并提交一次；
// - 已原子登记 Registered + Merged 后重挂恢复，仅首条 Registered 驱动恢复一次并发送一条最终回复（同时吸收平台重投）。
#[tokio::test(start_paused = true)]
async fn preserves_unclosed_album_offset_across_shutdown_and_recovers_registered_plus_merged_once()
{
    let harness = ImTestHarness::start(&[ALLOWED_UID, OTHER_UID]).await;

    // 第一部分：600ms 收集窗口内停机 -> 零确认、零下载、零落账 -> 新实例接收重投后完整提交一次
    let img_1 = b"\x89PNG\r\n\x1a\nSTOP_WINDOW_IMG_1".to_vec();
    let img_2 = b"\x89PNG\r\n\x1a\nSTOP_WINDOW_IMG_2".to_vec();

    let io1 = Arc::new(FakeTelegramIo::default());
    io1.set_download_bytes("sw_1", img_1.clone());
    io1.set_download_bytes("sw_2", img_2.clone());
    let (gw_stop1, gw_task1) = harness.spawn_gateway(Arc::clone(&io1));
    let _ = io1.wait_poll_count(1).await;

    let batch_stop = vec![
        private_album_photo_update(
            3501,
            PRIVATE_CHAT_ID,
            ALLOWED_UID,
            "g-stop-window",
            &[("sw_1", Some(img_1.len() as u64))],
            Some("窗口内首图"),
        ),
        private_album_photo_update(
            3502,
            PRIVATE_CHAT_ID,
            ALLOWED_UID,
            "g-stop-window",
            &[("sw_2", Some(img_2.len() as u64))],
            Some("窗口内次图"),
        ),
    ];
    io1.push_updates(batch_stop.clone());
    let _ = io1.wait_poll_count(2).await;

    // 仅推进 300ms（仍在 600ms 静默窗口内）即触发停机
    tokio::time::advance(Duration::from_millis(300)).await;
    let _ = gw_stop1.send(());
    bounded(gw_task1).await.unwrap().unwrap();

    assert!(!io1.is_acknowledged(&inbound_id_from_update_id(3501)));
    assert!(!io1.is_acknowledged(&inbound_id_from_update_id(3502)));
    assert_eq!(
        io1.offsets.lock().unwrap().next_offset(),
        None,
        "600ms 窗口内停机绝不能确认未闭合分片或推进 offset"
    );
    assert!(io1.download_calls().is_empty());

    // 新实例启动并收到 Telegram 完整重投 [3501, 3502] -> 600ms 后合并提交一次
    let io2 = Arc::new(FakeTelegramIo::default());
    io2.set_download_bytes("sw_1", img_1.clone());
    io2.set_download_bytes("sw_2", img_2.clone());
    let (gw_stop2, gw_task2) = harness.spawn_gateway(Arc::clone(&io2));
    let _ = io2.wait_poll_count(1).await;
    io2.push_updates(batch_stop);
    let _ = io2.wait_poll_count(2).await;
    tokio::time::advance(Duration::from_millis(600)).await;

    let (mut sock_sw, _) = bounded(harness.listener.accept()).await.unwrap();
    let req_sw: serde_json::Value =
        serde_json::from_str(&read_http_request(&mut sock_sw).await).unwrap();
    let content_sw = req_sw["messages"].as_array().unwrap().last().unwrap()["content"]
        .as_array()
        .unwrap();
    assert_eq!(content_sw.len(), 3);
    assert_eq!(content_sw[2]["text"], "窗口内首图\n窗口内次图");
    write_sse_complete(&mut sock_sw, None, "停机重投相册回复", None).await;

    let msgs_sw = io2.wait_message_count(1).await;
    assert_eq!(msgs_sw[0].1, "停机重投相册回复");
    io2.wait_offset(Some(3503)).await;
    let _ = gw_stop2.send(());
    bounded(gw_task2).await.unwrap().unwrap();

    // 第二部分：在 OTHER_UID 上真实执行相册 register_turn (3510 -> Registered, 3511 -> Merged) + submit，
    // 在 mark_accepted 落盘前强拆旧网关实例，使磁盘保留 Registered + Merged 且后台轮次仍在途运行；
    // 新实例启动恢复并同时接收 [3510, 3511] 重投 -> 仅恢复一次、发送一条最终回复！
    let rec_img_1 = b"\x89PNG\r\n\x1a\nRECOVER_ALBUM_1".to_vec();
    let rec_img_2 = b"\x89PNG\r\n\x1a\nRECOVER_ALBUM_2".to_vec();
    let io3 = Arc::new(FakeTelegramIo::default());
    io3.set_download_bytes("rec_1", rec_img_1.clone());
    io3.set_download_bytes("rec_2", rec_img_2.clone());
    let (_gw_stop3, gw_task3) = harness.spawn_gateway(Arc::clone(&io3));
    let _ = io3.wait_poll_count(1).await;

    let rec_batch = vec![
        private_album_photo_update(
            3510,
            OTHER_UID,
            OTHER_UID,
            "g-recover-group",
            &[("rec_1", Some(rec_img_1.len() as u64))],
            Some("恢复首图"),
        ),
        private_album_photo_update(
            3511,
            OTHER_UID,
            OTHER_UID,
            "g-recover-group",
            &[("rec_2", Some(rec_img_2.len() as u64))],
            Some("恢复次图"),
        ),
    ];
    io3.push_updates(rec_batch.clone());
    let _ = io3.wait_poll_count(2).await;

    // 放过第一次台账写入（register_turn），在第二次台账写入（mark_accepted）前暂停并强拆旧实例
    let (reg3_entered, reg3_release, reg3_finished) =
        crate::ledger::arm_replace_gate_for_test(harness.layout.lattice_root().to_path_buf());
    tokio::time::advance(Duration::from_millis(600)).await;
    bounded(reg3_entered).await.unwrap();
    let (acc3_entered, acc3_release, acc3_finished) =
        crate::ledger::arm_replace_gate_for_test(harness.layout.lattice_root().to_path_buf());
    reg3_release.send(()).unwrap();
    bounded(reg3_finished).await.unwrap();
    bounded(acc3_entered).await.unwrap();

    // 此时后台已接受该相册轮次并向假模型发起请求（保持流式连接打开），而磁盘台账仍停在 Registered + Merged
    let (mut sock_rec, _) = bounded(harness.listener.accept()).await.unwrap();
    let _ = read_http_request(&mut sock_rec).await;

    gw_task3.abort();
    let _ = bounded(gw_task3).await;
    acc3_release.send(()).unwrap();
    bounded(acc3_finished).await.unwrap();

    {
        let disk_before_recover = Ledger::open_and_prune(
            harness.layout.clone(),
            Utc::now(),
            TELEGRAM_REDELIVERY_WINDOW,
        )
        .await
        .unwrap();
        assert!(matches!(
            disk_before_recover
                .inbound_record(&inbound_id_from_update_id(3510))
                .await,
            Some(InboundRecord::Registered { .. })
        ));
        assert!(matches!(
            disk_before_recover
                .inbound_record(&inbound_id_from_update_id(3511))
                .await,
            Some(InboundRecord::Merged { .. })
        ));
    }

    // 启动新实例 io4：启动恢复仅针对首条 3510（忽略 Merged 3511），同时重投 [3510, 3511] 也被台账静默确认
    let io4 = Arc::new(FakeTelegramIo::default());
    let (gw_stop4, gw_task4) = harness.spawn_gateway(Arc::clone(&io4));
    let _ = io4.wait_poll_count(1).await;
    io4.push_updates(rec_batch);
    io4.wait_offset(Some(3512)).await;

    write_sse_complete(&mut sock_rec, None, "相册唯一恢复最终回复", None).await;
    let recovered_msgs = io4.wait_message_count(1).await;
    for _ in 0..20 {
        tokio::task::yield_now().await;
    }
    assert_eq!(
        recovered_msgs.len(),
        1,
        "Registered + Merged 恢复与重投只能发送唯一 1 条最终回复"
    );
    assert_eq!(recovered_msgs[0].0, chat_key_from_id(OTHER_UID));
    assert_eq!(recovered_msgs[0].1, "相册唯一恢复最终回复");
    assert!(
        io4.download_calls().is_empty(),
        "恢复已提交的相册轮次不得重新下载任何图片"
    );

    let _ = gw_stop4.send(());
    bounded(gw_task4).await.unwrap().unwrap();

    let final_snap = Ledger::open_and_prune(
        harness.layout.clone(),
        Utc::now(),
        TELEGRAM_REDELIVERY_WINDOW,
    )
    .await
    .unwrap();
    assert!(matches!(
        final_snap
            .inbound_record(&inbound_id_from_update_id(3510))
            .await,
        Some(InboundRecord::DeliveredTurn { .. })
    ));
    assert!(matches!(
        final_snap
            .inbound_record(&inbound_id_from_update_id(3511))
            .await,
        Some(InboundRecord::Merged { .. })
    ));

    harness.shutdown().await;
}

// 场景 23：拒绝后的重投竞态防护（start_paused = true）：
// - 核验确认调用发生时在途守卫仍存活（先 ack 后清守卫，消除“既不在途又未确认”窗口）；
// - 同一 update_id 图片因不可重试 404 被拒后紧随触发重投，或因重试耗尽被拒且重投已先越过外层检查进入工作队列时，
//   均不会再次发起下载、不会重复回复、也不会错误产生后续轮次。
#[tokio::test(start_paused = true)]
async fn rejected_image_redelivery_race_never_redownloads_replies_twice_or_creates_turn() {
    let harness = ImTestHarness::start(&[ALLOWED_UID]).await;
    let io = Arc::new(FakeTelegramIo::default());
    let valid_png = b"\x89PNG\r\n\x1a\nUNEXPECTED_SECOND_PASS".to_vec();

    // 1. race_404：首次返回不可重试 404，第二次预置成功图片（若发生二次下载则会提交给假模型）
    io.push_download_outcome(
        "race_404",
        Err(TelegramDownloadError::Api(
            TelegramApiError::UnauthorizedOrInvalid {
                status: 404,
                diagnostic: "Telegram 返回 HTTP 404".into(),
            },
        )),
    );
    io.push_download_outcome("race_404", Ok(valid_png.clone()));

    // 2. race_exhaust：前 4 次（首次 + 3 次重试）返回 503 耗尽重试，第 5 次预置成功图片
    for _ in 0..4 {
        io.push_download_outcome(
            "race_exhaust",
            Err(TelegramDownloadError::Api(TelegramApiError::Retryable {
                diagnostic: "Telegram 返回 HTTP 503".into(),
            })),
        );
    }
    io.push_download_outcome("race_exhaust", Ok(valid_png.clone()));

    let (gw_stop, gw_task, probe_rx) = harness.spawn_gateway_with_probe(Arc::clone(&io));
    let probe = Arc::new(bounded(probe_rx).await.expect("应获取网关调试探针"));
    let _ = io.wait_poll_count(1).await;

    let ack_in_flight_observations = Arc::new(StdMutex::new(Vec::<(InboundId, bool)>::new()));
    {
        let probe_for_ack = Arc::clone(&probe);
        let obs_for_ack = Arc::clone(&ack_in_flight_observations);
        io.set_before_ack_hook(move |ids| {
            let mut rec = obs_for_ack.lock().unwrap();
            for id in ids {
                if id == &inbound_id_from_update_id(2451) || id == &inbound_id_from_update_id(2452)
                {
                    rec.push((id.clone(), probe_for_ack.contains_in_flight(id)));
                }
            }
        });
    }

    // 子用例 A（不可重试 404 被拒 + 重投紧随触发）：
    // 在首次下载 2451 在途未确认时，让下一轮 getUpdates 先取出重投的 [2451] 并停在返回前闸门；
    // 待 2451 完成拒绝（ack + 清守卫）后立即放行重投批次进入 dispatch_update。
    let u_2451 = private_photo_update(
        2451,
        PRIVATE_CHAT_ID,
        ALLOWED_UID,
        &[("race_404", Some(valid_png.len() as u64))],
        Some("不可重试被拒图片"),
    );
    let (dl_entered, dl_release) = io.install_download_gate();
    io.push_updates(vec![u_2451.clone()]);
    for _ in 0..10 {
        tokio::task::yield_now().await;
    }
    bounded(dl_entered).await.unwrap();

    let (poll_entered_a, poll_release_a) = io.install_poll_return_gate();
    io.push_updates(vec![u_2451]);
    for _ in 0..10 {
        tokio::task::yield_now().await;
    }
    bounded(poll_entered_a).await.unwrap();

    let _ = dl_release.send(());
    let msgs_after_a = io.wait_message_count(1).await;
    assert!(
        msgs_after_a[0].1.contains("HTTP 404"),
        "2451 首次被拒应回复包含 HTTP 404 的说明: {}",
        msgs_after_a[0].1
    );

    let _ = poll_release_a.send(());
    for _ in 0..20 {
        tokio::task::yield_now().await;
    }
    assert_eq!(
        io.download_calls().len(),
        1,
        "紧随拒绝触发的 2451 重投必须被已确认状态拦截，不得发起第 2 次下载"
    );
    assert_eq!(
        io.sent_messages.lock().unwrap().len(),
        1,
        "2451 重投不得重复发送拒绝回复"
    );

    // 子用例 B（重试耗尽被拒 + 重投更新已先越过外层检查进入单对话队列）：
    // 在 2452 重试退避期间（未确认），先让 getUpdates 取出重投的 [2452] 停在返回前闸门；
    // 待 4 次下载耗尽完成拒绝后，单次旁路外层 is_acknowledged 检查并放行重投进入 run_chat_worker，
    // 验证出队后的 is_acknowledged 复查直接确认并跳过，绝不触发第 5 次下载或产生新轮次。
    let u_2452 = private_photo_update(
        2452,
        PRIVATE_CHAT_ID,
        ALLOWED_UID,
        &[("race_exhaust", Some(valid_png.len() as u64))],
        Some("重试耗尽被拒图片"),
    );
    io.push_updates(vec![u_2452.clone()]);
    for _ in 0..10 {
        tokio::task::yield_now().await;
    }
    let _ = io.wait_download_count(2).await;

    let (poll_entered_b, poll_release_b) = io.install_poll_return_gate();
    io.push_updates(vec![u_2452]);
    for _ in 0..10 {
        tokio::task::yield_now().await;
    }
    bounded(poll_entered_b).await.unwrap();

    for step_secs in [1u64, 1, 2] {
        tokio::time::advance(Duration::from_secs(step_secs)).await;
        for _ in 0..10 {
            tokio::task::yield_now().await;
        }
    }
    let msgs_after_b = io.wait_message_count(2).await;
    assert!(
        msgs_after_b[1].1.contains("下载图片失败"),
        "2452 重试耗尽应回复下载失败提示: {}",
        msgs_after_b[1].1
    );
    assert_eq!(
        io.download_calls().len(),
        5,
        "2451（1 次）与 2452（4 次）在重投放行前合计应恰好 5 次下载调用"
    );

    io.arm_bypass_is_acked_once(inbound_id_from_update_id(2452));
    let _ = poll_release_b.send(());
    for _ in 0..25 {
        tokio::task::yield_now().await;
    }

    // 追加 /status 确认队列畅通且从未建立会话或产生多余轮次
    io.push_updates(vec![private_text_update(
        2453,
        PRIVATE_CHAT_ID,
        ALLOWED_UID,
        "/status",
    )]);
    let final_msgs = io.wait_message_count(3).await;
    assert_eq!(
        final_msgs[2].1, "当前没有绑定的会话",
        "被拒图片重投绝不得创建草稿会话或产生后续轮次"
    );
    io.wait_offset(Some(2454)).await;

    assert_eq!(
        io.download_calls().len(),
        5,
        "越过外层检查进入工作队列的 2452 重投必须被出队确认复查拦截，绝不再次下载"
    );

    let ack_obs = ack_in_flight_observations.lock().unwrap().clone();
    assert!(
        ack_obs.len() >= 2 && ack_obs.iter().all(|(_, was_in_flight)| *was_in_flight),
        "每次调用 acknowledge 时对应更新必须仍处于 in_flight 守卫保护中（先确认再清守卫），实际观察: {ack_obs:?}"
    );
    assert!(
        !harness.layout.lattice_root().join("lattice.redb").exists(),
        "被拒图片及重投全过程不得初始化会话库"
    );

    let _ = gw_stop.send(());
    bounded(gw_task).await.unwrap().unwrap();
    harness.shutdown().await;
}

// 场景 24：单图下载 429 退避期间的空轮询节流与停机立即唤醒（start_paused = true）：
// - 无开放相册但单图正在 45s 429 退避中时，持续重投同一在途 update_id 按 100ms 节流而不紧循环；
// - 正常空批次长轮询返回（无开放相册且 updates 为空）保持零节流；
// - 节流休眠期间收到停机信号立即打断退出，且不确认未完成的在途图片更新。
#[tokio::test(start_paused = true)]
async fn throttles_idle_polls_at_100ms_during_single_image_429_backoff_and_wakes_immediately_on_stop()
 {
    let harness = ImTestHarness::start(&[ALLOWED_UID]).await;
    let io = Arc::new(FakeTelegramIo::default());
    let png = b"\x89PNG\r\n\x1a\nSINGLE_429_THROTTLE".to_vec();

    io.push_download_outcome(
        "single_429_img",
        Err(TelegramDownloadError::Api(TelegramApiError::RateLimited {
            retry_after: Some(Duration::from_secs(45)),
            diagnostic: "Telegram 返回 HTTP 429".into(),
        })),
    );
    io.push_download_outcome("single_429_img", Ok(png.clone()));

    let (gw_stop, gw_task) = harness.spawn_gateway(Arc::clone(&io));
    let _ = io.wait_poll_count(1).await;
    let t0 = io.poll_timestamps.lock().unwrap()[0];

    // 1. t0 推送单张私聊图片 2481（无 media_group_id）：触发第 1 次下载返回 429 进入 45s 退避；首轮含新更新不节流
    let u_single = private_photo_update(
        2481,
        PRIVATE_CHAT_ID,
        ALLOWED_UID,
        &[("single_429_img", Some(png.len() as u64))],
        Some("单图退避节流"),
    );
    io.push_updates(vec![u_single.clone()]);
    for _ in 0..15 {
        tokio::task::yield_now().await;
    }
    let _ = io.wait_download_count(1).await;
    let _ = io.wait_poll_count(2).await;
    {
        let stamps = io.poll_timestamps.lock().unwrap().clone();
        assert_eq!(
            stamps[1].duration_since(t0),
            Duration::ZERO,
            "包含新单图更新的轮询批次不得节流"
        );
    }

    // 2. t0 + 200ms 平台重投在途未确认的 2481：整批被 in_flight 过滤后无新更新，必须进入 100ms 节流休眠
    tokio::time::advance(Duration::from_millis(200)).await;
    io.push_updates(vec![u_single.clone()]);
    for _ in 0..15 {
        tokio::task::yield_now().await;
    }
    assert_eq!(
        io.poll_timestamps.lock().unwrap().len(),
        2,
        "单图退避期间重投被过滤后不得紧循环立即发起第 3 次轮询"
    );

    tokio::time::advance(Duration::from_millis(99)).await;
    for _ in 0..10 {
        tokio::task::yield_now().await;
    }
    assert_eq!(
        io.poll_timestamps.lock().unwrap().len(),
        2,
        "99ms 时 100ms 节流尚未届满，不得提前发起第 3 次轮询"
    );

    tokio::time::advance(Duration::from_millis(1)).await;
    let _ = io.wait_poll_count(3).await;
    {
        let stamps = io.poll_timestamps.lock().unwrap().clone();
        assert_eq!(
            stamps[2].duration_since(t0),
            Duration::from_millis(300),
            "第 3 次轮询必须精确发生在重投后 100ms（t0 + 300ms）"
        );
    }

    // 3. 紧接再次重投 2481：再经 100ms 节流才进入第 4 次轮询（t0 + 400ms）
    io.push_updates(vec![u_single.clone()]);
    for _ in 0..15 {
        tokio::task::yield_now().await;
    }
    tokio::time::advance(Duration::from_millis(99)).await;
    for _ in 0..10 {
        tokio::task::yield_now().await;
    }
    assert_eq!(io.poll_timestamps.lock().unwrap().len(), 3);
    tokio::time::advance(Duration::from_millis(1)).await;
    let _ = io.wait_poll_count(4).await;
    {
        let stamps = io.poll_timestamps.lock().unwrap().clone();
        assert_eq!(
            stamps[3].duration_since(stamps[2]),
            Duration::from_millis(100),
            "连续重投在途单图期间轮询间隔必须精确保持 100ms"
        );
    }

    // 4. 无开放相册时正常长轮询超时返回空列表 Vec::new()：不得触发节流
    io.push_updates(Vec::new());
    for _ in 0..15 {
        tokio::task::yield_now().await;
    }
    let _ = io.wait_poll_count(5).await;
    {
        let stamps = io.poll_timestamps.lock().unwrap().clone();
        assert_eq!(
            stamps[4].duration_since(stamps[3]),
            Duration::ZERO,
            "无开放相册且长轮询返回空列表时不得额外节流"
        );
    }

    // 5. 再次重投 2481 使轮询进入 100ms 节流休眠，推进 30ms 后发出停机信号 -> 必须立即唤醒退出且不确认 2481
    io.push_updates(vec![u_single]);
    for _ in 0..15 {
        tokio::task::yield_now().await;
    }
    tokio::time::advance(Duration::from_millis(30)).await;
    for _ in 0..10 {
        tokio::task::yield_now().await;
    }
    assert_eq!(io.poll_timestamps.lock().unwrap().len(), 5);

    let t_stop = tokio::time::Instant::now();
    let _ = gw_stop.send(());
    bounded(gw_task).await.unwrap().unwrap();
    assert_eq!(
        tokio::time::Instant::now().duration_since(t_stop),
        Duration::ZERO,
        "停机信号必须立即打断 100ms 空轮询节流休眠与 45s 下载退避"
    );
    assert!(
        !io.is_acknowledged(&inbound_id_from_update_id(2481)),
        "退避中断退出时未完成的单图更新不得被确认"
    );
    assert_eq!(io.download_calls().len(), 1);

    harness.shutdown().await;
}

/// 分块排空假 provider 收到的 HTTP 请求头与正文并返回 `Content-Length`，避免在大预算测试中额外分配整包字符串副本。
async fn drain_http_request_len(socket: &mut tokio::net::TcpStream) -> usize {
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
    let mut remaining = content_len;
    let mut buf = vec![0_u8; 64 * 1024];
    while remaining > 0 {
        let to_read = remaining.min(buf.len());
        socket
            .read_exact(&mut buf[..to_read])
            .await
            .expect("排空 HTTP 请求体字节应成功");
        remaining -= to_read;
    }
    content_len
}

/// 向假 provider 连接写入 `HTTP/1.1 413 Payload Too Large` JSON 错误响应。
async fn write_http_413_error(socket: &mut tokio::net::TcpStream, body_json: &str) {
    let header = format!(
        "HTTP/1.1 413 Payload Too Large\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body_json.len()
    );
    socket.write_all(header.as_bytes()).await.unwrap();
    socket.write_all(body_json.as_bytes()).await.unwrap();
    socket.flush().await.unwrap();
}

// 场景 25：目标模型不支持图片输入（input = ["text"]）：
// 私聊发送合法图片时在准入预检阶段被拒绝，假模型零 HTTP 连接，IM 回复含脱敏中文原因与减少图片、/new、lattice session rewind 引导，
// 台账推进为 DeliveredTurn，且确认 offset 正常推进。
#[tokio::test]
async fn rejects_image_for_text_only_model_with_im_guidance_and_marks_delivered_turn() {
    let harness = ImTestHarness::start(&[ALLOWED_UID]).await;
    harness.set_model_input_modalities(&["text"]);

    let io = Arc::new(FakeTelegramIo::default());
    let png = b"\x89PNG\r\n\x1a\nTEXT_ONLY_MODEL_CHECK".to_vec();
    io.set_download_bytes("text_only_f1", png.clone());

    let (gw_stop, gw_task) = harness.spawn_gateway(Arc::clone(&io));

    io.push_updates(vec![private_photo_update(
        3601,
        PRIVATE_CHAT_ID,
        ALLOWED_UID,
        &[("text_only_f1", Some(png.len() as u64))],
        Some("请帮我分析这张图"),
    )]);

    let msgs = io.wait_message_count(1).await;
    io.wait_offset(Some(3602)).await;

    let reply = &msgs[0].1;
    assert!(
        reply.contains("local/local-test") && reply.contains("不支持图片输入"),
        "拒绝回复应包含脱敏中文原因与模型名：{reply}"
    );
    assert!(
        reply.contains("减少图片")
            && reply.contains("/new")
            && reply.contains("lattice session rewind"),
        "拒绝回复必须同时引导减少图片、发送 /new 与执行 lattice session rewind：{reply}"
    );
    assert!(
        !reply.contains("INTEGRATION_BOT_SECRET"),
        "拒绝回复严禁泄露机器人凭据：{reply}"
    );
    assert_eq!(
        reply.matches("减少图片").count(),
        1,
        "拒绝回复不得重复拼接冗余引导：{reply}"
    );

    // 等待异步拒绝回复任务将首条台账标记为 DeliveredTurn
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
                snap.inbound_record(&inbound_id_from_update_id(3601)).await,
                Some(InboundRecord::DeliveredTurn { .. })
            ) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;

    let _ = gw_stop.send(());
    bounded(gw_task).await.unwrap().unwrap();

    // 网关与所有子任务已收尾，确认假模型监听器从未收到任何 TCP 连接
    let listener_pending = std::future::poll_fn(|cx| {
        std::task::Poll::Ready(harness.listener.poll_accept(cx).is_pending())
    })
    .await;
    assert!(
        listener_pending,
        "纯文本模型准入预检拒绝图片时，假模型必须保持零 HTTP 连接"
    );

    harness.shutdown().await;
}

// 场景 26：累计历史超过 32 MiB 本地请求体预算：
// 前 3 轮每轮提交 2 张 3.6 MiB 合法 PNG（每轮约 9.6 MiB base64，3 轮累计约 28.8 MiB 均成功），
// 第 4 轮再发 1 张 3.6 MiB PNG 使累计超过 32 MiB 触发准入预检拒绝：第 4 轮假模型零新连接，
// IM 回复含 32 MiB / 本地请求体预算原因及减少图片、/new、lattice session rewind 引导，台账记为 DeliveredTurn。
#[tokio::test(start_paused = true)]
async fn rejects_fourth_round_exceeding_32mib_local_request_budget_with_im_guidance() {
    let harness = ImTestHarness::start(&[ALLOWED_UID]).await;
    harness.set_model_input_modalities(&["text", "image"]);

    let io = Arc::new(FakeTelegramIo::default());
    // 3_774_873 字节（3.60 MiB <= 3.75 MiB），单张 base64 恰好 5_033_164 字节（4.80 MiB）
    let mut big_png = vec![0u8; 3_774_873];
    big_png[..8].copy_from_slice(b"\x89PNG\r\n\x1a\n");
    io.set_download_bytes("budget_big_png", big_png.clone());

    let (gw_stop, gw_task) = harness.spawn_gateway(Arc::clone(&io));
    let _ = io.wait_poll_count(1).await;

    for round in 1..=3_i64 {
        let base_uid = 3700 + round * 10;
        let group_id = format!("budget-album-{round}");
        io.push_updates(vec![
            private_album_photo_update(
                base_uid + 1,
                PRIVATE_CHAT_ID,
                ALLOWED_UID,
                &group_id,
                &[("budget_big_png", Some(big_png.len() as u64))],
                Some(&format!("第{round}轮大图组")),
            ),
            private_album_photo_update(
                base_uid + 2,
                PRIVATE_CHAT_ID,
                ALLOWED_UID,
                &group_id,
                &[("budget_big_png", Some(big_png.len() as u64))],
                None,
            ),
        ]);
        let _ = io.wait_poll_count((round as usize) + 1).await;
        tokio::time::advance(Duration::from_millis(600)).await;

        let (mut sock, _) = bounded(harness.listener.accept()).await.unwrap();
        let req_len = drain_http_request_len(&mut sock).await;
        assert!(
            req_len > (round as usize) * 10_000_000 && req_len <= 32 * 1024 * 1024,
            "第 {round} 轮请求体长度应随历史图片递增且在 32 MiB 预算内，实际: {req_len}"
        );
        write_sse_complete(&mut sock, None, &format!("第{round}轮完成"), None).await;

        let msgs = io.wait_message_count(round as usize).await;
        assert_eq!(msgs[(round as usize) - 1].1, format!("第{round}轮完成"));
        io.wait_offset(Some(base_uid + 3)).await;
    }

    // 第 4 轮发送 1 张单图（累计 7 张约 33.6 MiB base64 > 32 MiB），在后台准入预检阶段被拒绝
    io.push_updates(vec![private_photo_update(
        3741,
        PRIVATE_CHAT_ID,
        ALLOWED_UID,
        &[("budget_big_png", Some(big_png.len() as u64))],
        Some("第4轮越过本地请求体预算"),
    )]);

    let all_msgs = io.wait_message_count(4).await;
    io.wait_offset(Some(3742)).await;

    let fourth_reply = &all_msgs[3].1;
    assert!(
        fourth_reply.contains("本地请求体预算") && fourth_reply.contains("32 MiB"),
        "第 4 轮回复应包含 32 MiB 本地请求体预算原因：{fourth_reply}"
    );
    assert!(
        fourth_reply.contains("减少图片")
            && fourth_reply.contains("/new")
            && fourth_reply.contains("lattice session rewind"),
        "第 4 轮回复必须包含减少图片、/new 与 lattice session rewind 引导：{fourth_reply}"
    );
    assert!(!fourth_reply.contains("INTEGRATION_BOT_SECRET"));

    let _ = gw_stop.send(());
    bounded(gw_task).await.unwrap().unwrap();

    // 验证第 4 轮未向假模型发起任何新 HTTP 连接
    let no_fourth_conn = std::future::poll_fn(|cx| {
        std::task::Poll::Ready(harness.listener.poll_accept(cx).is_pending())
    })
    .await;
    assert!(
        no_fourth_conn,
        "第 4 轮触发 32 MiB 本地预算预检拒绝时，假模型必须零新连接"
    );

    let ledger = Ledger::open_and_prune(
        harness.layout.clone(),
        Utc::now(),
        TELEGRAM_REDELIVERY_WINDOW,
    )
    .await
    .unwrap();
    assert!(matches!(
        ledger
            .inbound_record(&inbound_id_from_update_id(3741))
            .await,
        Some(InboundRecord::DeliveredTurn { .. })
    ));

    harness.shutdown().await;
}

// 场景 27：远端返回 HTTP 413（覆盖实时投递与停机重启后从持久 summary.notice 恢复补发两条路径）：
// - 实时路径：假模型返回 HTTP/1.1 413 Payload Too Large，IM 回复含 HTTP 413、脱敏原因及减少图片、/new、lattice session rewind，台账落为 DeliveredTurn；
// - 恢复路径：第 2 轮图片在发送最终回复前被停机中断（台账停在 Accepted），核实持久库中 outcome.diagnostic == None 且 summary.notice 含 HTTP 413，
//   重启新网关实例后补发的最终回复同样包含完整 HTTP 413 原因与 IM 引导，并将台账推进为 DeliveredTurn。
#[tokio::test]
async fn delivers_http_413_guidance_both_live_and_via_recovered_summary_notice() {
    let harness = ImTestHarness::start(&[ALLOWED_UID]).await;
    harness.set_model_input_modalities(&["text", "image"]);

    let png_1 = b"\x89PNG\r\n\x1a\nHTTP_413_LIVE_ROUND".to_vec();
    let png_2 = b"\x89PNG\r\n\x1a\nHTTP_413_RECOVER_ROUND".to_vec();

    let io1 = Arc::new(FakeTelegramIo::default());
    io1.set_download_bytes("f413_live", png_1.clone());
    io1.set_download_bytes("f413_recover", png_2.clone());

    let (gw_stop1, gw_task1) = harness.spawn_gateway(Arc::clone(&io1));

    // 1. 实时路径：第 1 轮假模型返回 HTTP 413 JSON 错误体
    io1.push_updates(vec![private_photo_update(
        3801,
        PRIVATE_CHAT_ID,
        ALLOWED_UID,
        &[("f413_live", Some(png_1.len() as u64))],
        Some("实时 413 测试"),
    )]);

    let (mut sock1, _) = bounded(harness.listener.accept()).await.unwrap();
    let _ = read_http_request(&mut sock1).await;
    write_http_413_error(
        &mut sock1,
        r#"{"error":{"message":"Request payload size exceeds upstream 8 MiB limit","type":"request_too_large"}}"#,
    )
    .await;

    let live_msgs = io1.wait_message_count(1).await;
    io1.wait_offset(Some(3802)).await;

    let live_reply = &live_msgs[0].1;
    assert!(
        live_reply.contains("［本轮执行失败：")
            && live_reply.contains("HTTP 413")
            && live_reply.contains("Request payload size exceeds upstream 8 MiB limit")
            && live_reply.contains("目标服务拒绝了本次请求大小"),
        "实时 413 回复应包含脱敏远端摘要与中文原因：{live_reply}"
    );
    assert!(
        live_reply.contains("减少图片")
            && live_reply.contains("/new")
            && live_reply.contains("lattice session rewind"),
        "实时 413 回复必须包含减少图片、/new 与 lattice session rewind：{live_reply}"
    );
    assert!(!live_reply.contains("INTEGRATION_BOT_SECRET"));

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
                snap.inbound_record(&inbound_id_from_update_id(3801)).await,
                Some(InboundRecord::DeliveredTurn { .. })
            ) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;

    // 2. 恢复路径：拦截第 2 轮最终回复发送，在后台轮次已以 HTTP 413 失败落盘但 IM 尚未发出回复时停机
    let _hold_round2_send = io1.install_send_gate();
    io1.push_updates(vec![private_photo_update(
        3802,
        PRIVATE_CHAT_ID,
        ALLOWED_UID,
        &[("f413_recover", Some(png_2.len() as u64))],
        Some("重启恢复 413 测试"),
    )]);

    let (mut sock2, _) = bounded(harness.listener.accept()).await.unwrap();
    let _ = read_http_request(&mut sock2).await;
    write_http_413_error(
        &mut sock2,
        r#"{"error":{"message":"Second turn payload exceeds gateway cap","type":"request_too_large"}}"#,
    )
    .await;

    // 第 1 轮已进入过 1 次 send_message，等待第 2 轮进入 send_message 闸门后立即停机旧网关实例
    io1.wait_send_entered(2).await;
    io1.wait_offset(Some(3803)).await;
    let _ = gw_stop1.send(());
    bounded(gw_task1).await.unwrap().unwrap();
    assert_eq!(
        io1.sent_messages.lock().unwrap().len(),
        1,
        "停机中断前第 2 轮回复尚未发出"
    );

    // 核实持久状态：台账停在 Accepted，且后台 observe 读出的 outcome.diagnostic 为 None、summary.notice 含 HTTP 413
    let (session_id, round2_turn_id) = {
        let snap = Ledger::open_and_prune(
            harness.layout.clone(),
            Utc::now(),
            TELEGRAM_REDELIVERY_WINDOW,
        )
        .await
        .unwrap();
        match snap.inbound_record(&inbound_id_from_update_id(3802)).await {
            Some(InboundRecord::Accepted { session, turn, .. }) => (session, turn),
            other => panic!("停机中断发送后 3802 应保留在 Accepted 相位，实际: {other:?}"),
        }
    };
    {
        let mut observed = harness
            .service
            .observe(session_id, round2_turn_id)
            .await
            .expect("观察已落盘的失败轮次应成功");
        let raw_outcome = bounded(observed.outcome()).await.unwrap();
        assert_eq!(
            raw_outcome.diagnostic, None,
            "恢复路径必须依赖持久 summary.notice 而非内存 diagnostic"
        );
        let persisted_notice = raw_outcome
            .detail
            .summary
            .notice
            .as_ref()
            .expect("持久轮次摘要必须记录 HTTP 413 notice");
        assert!(persisted_notice.text.contains("HTTP 413"));
    }

    // 启动新网关实例 io2，自动从持久库恢复 Accepted 条目并补发最终回复
    let io2 = Arc::new(FakeTelegramIo::default());
    let (gw_stop2, gw_task2) = harness.spawn_gateway(Arc::clone(&io2));

    let recovered_msgs = io2.wait_message_count(1).await;
    let recovered_reply = &recovered_msgs[0].1;
    assert!(
        recovered_reply.contains("［本轮执行失败：")
            && recovered_reply.contains("HTTP 413")
            && recovered_reply.contains("Second turn payload exceeds gateway cap")
            && recovered_reply.contains("目标服务拒绝了本次请求大小"),
        "恢复补发的 413 回复应包含持久 summary.notice 中的原因：{recovered_reply}"
    );
    assert!(
        recovered_reply.contains("减少图片")
            && recovered_reply.contains("/new")
            && recovered_reply.contains("lattice session rewind"),
        "恢复补发的 413 回复必须包含减少图片、/new 与 lattice session rewind：{recovered_reply}"
    );
    assert!(!recovered_reply.contains("INTEGRATION_BOT_SECRET"));

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
                snap.inbound_record(&inbound_id_from_update_id(3802)).await,
                Some(InboundRecord::DeliveredTurn { .. })
            ) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;

    let _ = gw_stop2.send(());
    bounded(gw_task2).await.unwrap().unwrap();
    harness.shutdown().await;
}

// 场景 28：跨前端 BLOBS 回放测试（本地客户端 LocalClient 在 IM 含图会话上续发纯文本消息）：
// 先通过 IM 私聊提交图片完成首轮并落盘，再由连接同一后台的真实 LocalClient 向同一绑定 SessionId 提交纯文本续发，
// 验证假模型收到的第 2 轮 HTTP 请求体精确重放了 IM 持久化在 BLOBS 中的原始图片（base64 data URL 逐字节一致），且 LocalClient 收到完整模型回复。
#[tokio::test]
async fn replays_im_persisted_blobs_when_local_client_continues_same_session() {
    let harness = ImTestHarness::start(&[ALLOWED_UID]).await;
    harness.set_model_input_modalities(&["text", "image"]);

    let io = Arc::new(FakeTelegramIo::default());
    let chat_key = chat_key_from_id(PRIVATE_CHAT_ID);
    let im_png = b"\x89PNG\r\n\x1a\nCROSS_FRONTEND_BLOBS_REPLAY_IMAGE_PAYLOAD".to_vec();
    let expected_data_url = format!("data:image/png;base64,{}", test_base64_encode(&im_png));
    io.set_download_bytes("cross_frontend_png", im_png.clone());

    let (gw_stop, gw_task) = harness.spawn_gateway(Arc::clone(&io));

    // 1. 先经 IM 网关向私聊发送含图消息完成第 1 轮并落盘至会话库 BLOBS
    io.push_updates(vec![private_document_update(
        3901,
        PRIVATE_CHAT_ID,
        ALLOWED_UID,
        "cross_frontend_png",
        Some(im_png.len() as u64),
        Some("image/png"),
        Some("请记住这张来自 Telegram 私聊的架构图"),
    )]);

    let (mut sock1, _) = bounded(harness.listener.accept()).await.unwrap();
    let req1: serde_json::Value =
        serde_json::from_str(&read_http_request(&mut sock1).await).unwrap();
    assert_eq!(
        req1["messages"].as_array().unwrap().last().unwrap()["content"],
        serde_json::json!([
            {
                "type": "image_url",
                "image_url": { "url": expected_data_url }
            },
            {
                "type": "text",
                "text": "请记住这张来自 Telegram 私聊的架构图"
            }
        ])
    );
    write_sse_complete(&mut sock1, None, "IM 首轮图片已记录", Some((30, 10))).await;

    let im_msgs = io.wait_message_count(1).await;
    assert_eq!(im_msgs[0].1, "IM 首轮图片已记录");
    io.wait_offset(Some(3902)).await;

    let bound_sid = Ledger::open_and_prune(
        harness.layout.clone(),
        Utc::now(),
        TELEGRAM_REDELIVERY_WINDOW,
    )
    .await
    .unwrap()
    .bound_session(&chat_key)
    .await
    .expect("IM 首轮完成后台账应绑定 SessionId");

    // 2. 经真实 LocalClient（连接同一后台 IPC socket，等价于 `lattice session send --session <id>`）在同一会话上提交纯文本续发
    let ipc_client = LocalClient::new(harness.layout.clone());
    let cli_turn_id = new_turn_id();
    let mut local_turn = ipc_client.start_turn(TurnRequest {
        turn_id: cli_turn_id,
        session: bound_sid,
        workspace: harness.workspace.clone(),
        parts: vec![UserInputPart::Text {
            text: "终端 CLI 纯文本续发：请结合上一轮图片回答".into(),
        }],
        expected_settings: None,
    });

    let model_round2 = async {
        let (mut sock2, _) = bounded(harness.listener.accept()).await.unwrap();
        let req2: serde_json::Value =
            serde_json::from_str(&read_http_request(&mut sock2).await).unwrap();
        let msgs_arr = req2["messages"].as_array().unwrap();
        let replayed_urls: Vec<String> = msgs_arr
            .iter()
            .filter_map(|m| m["content"].as_array())
            .flatten()
            .filter_map(|part| part["image_url"]["url"].as_str().map(str::to_owned))
            .collect();
        assert_eq!(
            replayed_urls,
            vec![expected_data_url.clone()],
            "LocalClient 纯文本续发请求必须从会话库 BLOBS 精确重放 IM 提交的同一张原始图片"
        );
        assert!(
            msgs_arr.iter().any(|m| m["content"] == "IM 首轮图片已记录"),
            "续发请求应携带 IM 首轮助手回复上下文"
        );
        let last_user = msgs_arr.last().unwrap();
        assert_eq!(last_user["role"], "user");
        assert!(
            last_user["content"] == "终端 CLI 纯文本续发：请结合上一轮图片回答"
                || last_user["content"]
                    == serde_json::json!([{"type": "text", "text": "终端 CLI 纯文本续发：请结合上一轮图片回答"}]),
            "末尾用户消息应为 LocalClient 提交的纯文本续发内容，实际: {}",
            last_user["content"]
        );
        write_sse_complete(
            &mut sock2,
            None,
            "CLI 续发已读取历史图片并完成回答",
            Some((55, 18)),
        )
        .await;
    };

    let client_collect = async {
        let mut step_replies = Vec::new();
        while let Some(upd) = bounded(local_turn.next()).await {
            if let LocalUpdate::Entry { entry } = upd
                && let ConversationEntry::Step {
                    step,
                    content:
                        Node::Step {
                            reply: Some(message),
                            ..
                        },
                    ..
                } = *entry
                && step.key.turn_id == cli_turn_id
            {
                let text: String = message
                    .parts
                    .iter()
                    .filter_map(|part| match &part.semantic {
                        MessageSemantic::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect();
                step_replies.push(text);
            }
        }
        let report = bounded(local_turn.detach()).await;
        (step_replies, report)
    };

    let ((), (step_replies, report)) = tokio::join!(model_round2, client_collect);
    assert_eq!(
        report.summary.map(|s| s.outcome),
        Some(PublicTurnOutcome::Completed),
        "LocalClient 续发轮次应正常完成"
    );
    assert_eq!(
        step_replies,
        vec!["CLI 续发已读取历史图片并完成回答".to_string()],
        "LocalClient 必须完整收到模型基于重放历史图片生成的回复"
    );
    assert_eq!(
        io.sent_messages.lock().unwrap().len(),
        1,
        "外部 LocalClient 发起的轮次不应向 Telegram 重复发送额外消息"
    );

    let _ = gw_stop.send(());
    bounded(gw_task).await.unwrap().unwrap();
    harness.shutdown().await;
}
