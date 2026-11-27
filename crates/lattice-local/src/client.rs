// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 本地 IPC 客户端句柄与观察流帧读取。

use std::{path::PathBuf, time::Duration};

use lattice_ipc::{
    Acceptance, AuthKey, AuthRemoval, AuthStatus, ConfigureReply, ConfigureRequest, ContextOpReply,
    ContextOpRequest, DaemonStatus, FeedReply, FeedRequest, HISTORY_TURNS, ImAuthRemoveReply,
    ImAuthSetReply, ImAuthStatus, ImMountResult, InspectReply, LocalError, LocalErrorKind,
    LocalHistory, ModelCandidateQuery, ModelCandidates, NodeId, RecoveryCause, Request, Response,
    Result, RevocationStatus, SessionHead, SessionId, SessionListRow, SessionLog, SessionRecord,
    SessionUsage, TriggerRequest, TurnDetail, TurnId, TurnLookup, TurnRequest, ViewUpdate, ipc,
    require_chatgpt,
};
use lattice_store::StateLayout;
use tokio::net::UnixStream;

use crate::{endpoint, login::LocalAuthLogin, operation::ConfigureOutcome};

/// stop 回执后等待旧后台释放实例锁的上限；覆盖回执后收尾连接时的请求读取与写出超时。
const STOP_RELEASE_TIMEOUT: Duration = Duration::from_secs(30);
/// 跨版本信号停机时等待旧后台排空并释放状态根的上限；与默认 `TimeoutStopSec`（60s + 30s）对齐。
const SIGNAL_STOP_RELEASE_TIMEOUT: Duration = Duration::from_secs(90);

/// 停止本地后台的结果：同版本返回完整状态，更旧版本返回控制回执或信号停机结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StopOutcome {
    /// 与当前协议版本一致的后台经 IPC `stop` 完成排空并释放状态根。
    Status(DaemonStatus),
    /// 协议版本较旧的后台经版本无关控制帧完成排空并释放状态根。
    CrossVersionControl {
        epoch: String,
        owners: usize,
        unconfirmed_operations: usize,
    },
    /// 不支持控制帧的历史旧后台经 `pidfd` 发送 `SIGTERM` 后已释放状态根，无法读取排空计数。
    CrossVersionSignal { peer_pid: i32 },
}

impl StopOutcome {
    /// 同版本停机时取出完整的 `DaemonStatus`；跨版本停机返回 `None`。
    pub fn into_status(self) -> Option<DaemonStatus> {
        match self {
            Self::Status(status) => Some(status),
            Self::CrossVersionControl { .. } | Self::CrossVersionSignal { .. } => None,
        }
    }
}

/// 单次状态探测的结果，供 `daemon` 子命令在不解析错误文案的前提下区分更旧协议的后台。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StatusProbe {
    /// 协议版本一致的后台已应答。
    Running {
        status: DaemonStatus,
        peer_pid: Option<i32>,
    },
    /// 对端在同一连接上回写了更旧的协议魔数。
    OlderBackend {
        peer_pid: Option<i32>,
        /// 发请求前读到的对端可执行文件路径（`/proc/<pid>/exe`），仅用于诊断展示。
        peer_executable: Option<PathBuf>,
        /// 与普通请求相同的版本不兼容错误。
        error: LocalError,
    },
}

/// 每个操作一个连接；持续读取任务拥有分帧进度，UI 的 next 可取消。
#[derive(Clone)]
pub struct LocalClient {
    layout: StateLayout,
}

impl LocalClient {
    pub fn new(layout: StateLayout) -> Self {
        Self { layout }
    }

    async fn response(&self, request: Request) -> Result<Response> {
        self.response_with_progress(request, || {}).await
    }

    async fn response_with_progress(
        &self,
        request: Request,
        progress: impl FnMut(),
    ) -> Result<Response> {
        let mut stream = endpoint::connect(self.layout.clone()).await?;
        ipc::write_request(&mut stream, &request).await?;
        ipc::ResponseReader::default()
            .read_with_progress(&mut stream, progress)
            .await
    }

    async fn call(&self, request: Request) -> Result<Response> {
        match self.response(request).await? {
            Response::Error(error) => Err(error),
            response => Ok(response),
        }
    }

    pub async fn status(&self) -> Result<DaemonStatus> {
        match self.probe_status(Duration::from_secs(5)).await? {
            StatusProbe::Running { status, .. } => Ok(status),
            StatusProbe::OlderBackend { error, .. } => Err(error),
        }
    }

    /// 在给定截止时间内探测一次后台状态，区分同协议应答与明确回写更旧协议魔数的后台。
    pub async fn probe_status(&self, timeout: Duration) -> Result<StatusProbe> {
        tokio::time::timeout(timeout, async {
            let mut stream = endpoint::connect(self.layout.clone()).await?;
            let peer_pid = ipc::peer_pid(&stream);
            // 必须在发请求前读取对端可执行文件路径：对端随后在同一连接上回写魔数，
            // 证明读取时这个进程号仍属于监听进程；应答后再读，进程可能已退出、进程号被复用。
            let peer_executable =
                peer_pid.and_then(|pid| std::fs::read_link(format!("/proc/{pid}/exe")).ok());
            match ipc::send_and_read_first_or_older::<_, Response>(&mut stream, &Request::Status)
                .await?
            {
                ipc::FirstResponse::Reply(Response::Status(status)) => {
                    Ok(StatusProbe::Running { status, peer_pid })
                }
                ipc::FirstResponse::Reply(Response::Error(error)) => Err(error),
                ipc::FirstResponse::Reply(_) => Err(protocol()),
                ipc::FirstResponse::OlderBackend { error, .. } => Ok(StatusProbe::OlderBackend {
                    peer_pid,
                    peer_executable,
                    error,
                }),
            }
        })
        .await
        .map_err(|_| LocalError::new(LocalErrorKind::Transport, "查询后台状态超时"))?
    }

    /// 回执之后再等 socket 撤下、`owner.lock` 可取得才返回，调用方随即可在同一 STATE_ROOT 启动新后台。
    /// 对更旧协议版本的后台，依次尝试版本无关控制帧与经 `pidfd` 发送 `SIGTERM` 兜底；对更新版本仍拒绝。
    /// 不等待后台进程退出。超时仍未放开时报 Stopping。
    pub async fn stop(&self) -> Result<StopOutcome> {
        self.stop_with_timeouts(STOP_RELEASE_TIMEOUT, SIGNAL_STOP_RELEASE_TIMEOUT)
            .await
    }

    /// 等待上限由调用方给出；正式入口固定用 `STOP_RELEASE_TIMEOUT` / `SIGNAL_STOP_RELEASE_TIMEOUT`，测试以短上限核验超时报错。
    #[cfg(any(test, feature = "test-utils"))]
    pub async fn stop_within(&self, timeout: Duration) -> Result<StopOutcome> {
        self.stop_with_timeouts(timeout, timeout).await
    }

    async fn stop_with_timeouts(
        &self,
        release_timeout: Duration,
        signal_timeout: Duration,
    ) -> Result<StopOutcome> {
        let (mut stream, first_socket_id) =
            endpoint::connect_with_identity(self.layout.clone()).await?;
        let first_peer_pid = ipc::peer_pid(&stream);
        // 必须在发请求前捕获对端 pidfd：只有当对端随后在同一连接上主动回写更旧协议魔数时，
        // 才证明其在打开句柄时仍存活且 PID 未复用，届时才允许用于实例身份核验与 SIGTERM 兜底。
        let captured_peer = ipc::capture_peer_process(&stream);
        let first =
            ipc::send_and_read_first_or_older::<_, Response>(&mut stream, &Request::Stop).await?;
        // 先放开连接，后台排空连接时不必等本端。
        drop(stream);
        match first {
            ipc::FirstResponse::Reply(Response::Status(status)) => {
                drop(captured_peer);
                endpoint::Departure::new(self.layout.clone())
                    .wait(release_timeout)
                    .await?;
                Ok(StopOutcome::Status(status))
            }
            ipc::FirstResponse::Reply(Response::Error(error)) => Err(error),
            ipc::FirstResponse::Reply(_) => Err(protocol()),
            ipc::FirstResponse::OlderBackend {
                peer_version,
                error,
            } => {
                self.stop_older_backend(
                    first_socket_id,
                    first_peer_pid,
                    peer_version,
                    captured_peer,
                    error,
                    release_timeout,
                    signal_timeout,
                )
                .await
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn stop_older_backend(
        &self,
        first_socket_id: (u64, u64),
        first_peer_pid: Option<i32>,
        peer_version: u8,
        captured_peer: std::io::Result<ipc::PeerProcess>,
        incompatible_error: LocalError,
        release_timeout: Duration,
        signal_timeout: Duration,
    ) -> Result<StopOutcome> {
        // 若首连捕获的旧进程已经退出，严禁向任何新替换的后台发送控制停机帧或信号。
        if let Ok(ref peer) = captured_peer
            && peer.has_exited().unwrap_or(true)
        {
            return Err(peer_replaced());
        }

        // 1. 优先尝试版本无关控制停机帧：发送前先核对 socket `(dev, ino)`、对端 PID 与旧 pidfd 仍存活，
        //    同时在线上携带首连确认的 `peer_version`，确保后台在窗口内被替换时绝不误停新实例。
        let (mut control_stream, second_socket_id) =
            endpoint::connect_with_identity(self.layout.clone()).await?;
        let second_peer_pid = ipc::peer_pid(&control_stream);
        let peer_exited = captured_peer
            .as_ref()
            .ok()
            .is_some_and(|peer| peer.has_exited().unwrap_or(true));
        if second_socket_id != first_socket_id
            || first_peer_pid.is_none()
            || second_peer_pid != first_peer_pid
            || peer_exited
        {
            drop(control_stream);
            return Err(peer_replaced());
        }

        let control_reply = ipc::exchange_control_stop(&mut control_stream, peer_version).await?;
        drop(control_stream);
        match control_reply {
            ipc::ControlStopExchange::Stopped(status) => {
                drop(captured_peer);
                endpoint::Departure::new(self.layout.clone())
                    .wait(release_timeout)
                    .await?;
                return Ok(StopOutcome::CrossVersionControl {
                    epoch: status.epoch,
                    owners: status.owners,
                    unconfirmed_operations: status.unconfirmed_operations,
                });
            }
            ipc::ControlStopExchange::PeerChanged { .. } => {
                return Err(peer_replaced());
            }
            ipc::ControlStopExchange::Unsupported => {}
        }

        // 2. 对不支持控制帧的历史旧后台，再次确认旧进程尚未退出后，经首连捕获的 pidfd 发送 SIGTERM。
        let Ok(peer) = captured_peer else {
            return Err(incompatible_error);
        };
        if peer.has_exited().unwrap_or(true) {
            return Err(peer_replaced());
        }
        let peer_pid = peer.pid();
        let signaled = peer.terminate().map_err(|io_err| {
            LocalError::new(
                LocalErrorKind::Transport,
                &format!(
                    "向旧版本后台（对端进程号：{peer_pid}）发送停机信号失败：{io_err}；请执行 `systemctl --user stop lattice` 或 `kill {peer_pid}`"
                ),
            )
        })?;
        drop(peer);
        if !signaled {
            return Err(peer_replaced());
        }
        let timeout_message = format!(
            "已向旧版本后台（对端进程号：{peer_pid}）发送停机信号，但未在期限内释放状态根；请稍后核对进程状态或查看 `systemctl --user status lattice`"
        );
        endpoint::Departure::new(self.layout.clone())
            .wait_with_timeout_message(signal_timeout, &timeout_message)
            .await?;
        Ok(StopOutcome::CrossVersionSignal { peer_pid })
    }

    /// 只取得同版本 stop 回执：后台已排空，但 listener、socket 与实例锁可能尚未放开。
    #[cfg(any(test, feature = "test-utils"))]
    pub async fn request_stop(&self) -> Result<(DaemonStatus, endpoint::Departure)> {
        let mut stream = endpoint::connect(self.layout.clone()).await?;
        ipc::write_request(&mut stream, &Request::Stop).await?;
        let response = ipc::ResponseReader::default().read(&mut stream).await?;
        // 先放开连接，后台排空连接时不必等本端。
        drop(stream);
        match response {
            Response::Status(status) => Ok((status, endpoint::Departure::new(self.layout.clone()))),
            Response::Error(error) => Err(error),
            _ => Err(protocol()),
        }
    }

    pub async fn auth_set(&self, provider: &str, key: String) -> Result<()> {
        match self
            .call(Request::AuthSet {
                provider: provider.into(),
                key: AuthKey::new(key),
            })
            .await?
        {
            Response::AuthUpdated => Ok(()),
            _ => Err(protocol()),
        }
    }

    pub async fn auth_login(&self, provider: &str) -> Result<LocalAuthLogin> {
        require_chatgpt(provider)?;
        let mut stream = endpoint::connect(self.layout.clone()).await?;
        ipc::write_request(
            &mut stream,
            &Request::AuthLogin {
                provider: provider.into(),
            },
        )
        .await?;
        Ok(LocalAuthLogin::from_stream(stream))
    }

    pub async fn auth_logout(&self, provider: &str) -> Result<RevocationStatus> {
        match self
            .call(Request::AuthLogout {
                provider: provider.into(),
            })
            .await?
        {
            Response::AuthLoggedOut(result) => Ok(result.revocation),
            _ => Err(protocol()),
        }
    }

    pub async fn auth_remove(&self, provider: &str) -> Result<AuthRemoval> {
        match self
            .call(Request::AuthRemove {
                provider: provider.into(),
            })
            .await?
        {
            Response::AuthRemoved(result) => Ok(result),
            _ => Err(protocol()),
        }
    }

    pub async fn auth_status(&self) -> Result<AuthStatus> {
        match self.call(Request::AuthStatus {}).await? {
            Response::AuthStatus(status) => Ok(status),
            _ => Err(protocol()),
        }
    }

    /// 写入指定 IM 平台的凭据，并触发后台重新挂载 IM 组件。
    pub async fn im_auth_set(&self, platform: &str, token: &str) -> Result<ImAuthSetReply> {
        match self
            .call(Request::ImAuthSet {
                platform: platform.into(),
                token: AuthKey::new(token.to_owned()),
            })
            .await?
        {
            Response::ImAuthSet(reply) => Ok(reply),
            _ => Err(protocol()),
        }
    }

    /// 删除指定 IM 平台的凭据，并触发后台重新挂载 IM 组件。
    pub async fn im_auth_remove(&self, platform: &str) -> Result<ImAuthRemoveReply> {
        match self
            .call(Request::ImAuthRemove {
                platform: platform.into(),
            })
            .await?
        {
            Response::ImAuthRemove(reply) => Ok(reply),
            _ => Err(protocol()),
        }
    }

    /// 查询全部受支持 IM 平台的本地凭据状态。
    pub async fn im_auth_status(&self) -> Result<ImAuthStatus> {
        match self.call(Request::ImAuthStatus {}).await? {
            Response::ImAuthStatus(status) => Ok(status),
            _ => Err(protocol()),
        }
    }

    /// 请求后台重新读取配置与凭据并重新挂载 IM 组件。
    pub async fn im_reload(&self) -> Result<ImMountResult> {
        match self.call(Request::ImReload {}).await? {
            Response::ImReload(result) => Ok(result),
            _ => Err(protocol()),
        }
    }

    pub async fn models(&self, query: ModelCandidateQuery) -> Result<ModelCandidates> {
        match self.call(Request::Models { query }).await? {
            Response::Models(models) => Ok(models),
            _ => Err(protocol()),
        }
    }

    /// 立即刷新模型元数据，并等待缓存替换或返回具体失败原因。
    pub async fn models_refresh(&self) -> Result<()> {
        match self.call(Request::ModelsRefresh {}).await? {
            Response::ModelsRefreshed => Ok(()),
            _ => Err(protocol()),
        }
    }

    pub async fn resolve_session(&self, prefix: &str) -> Result<SessionRecord> {
        match self
            .call(Request::ResolveSession {
                prefix: prefix.into(),
            })
            .await?
        {
            Response::Session(session) => Ok(session),
            _ => Err(protocol()),
        }
    }

    pub async fn list_sessions(&self) -> Result<Vec<SessionListRow>> {
        let mut result = Vec::new();
        let mut after = None;
        loop {
            let Response::Sessions(page) = self.call(Request::List { after, limit: 256 }).await?
            else {
                return Err(protocol());
            };
            if page.next.is_some_and(|next| Some(next) == after) {
                return Err(protocol());
            }
            result.extend(page.items);
            after = page.next;
            if after.is_none() {
                return Ok(result);
            }
        }
    }

    pub async fn log(&self, session_id: SessionId) -> Result<SessionLog> {
        match self.call(Request::Log { session_id }).await? {
            Response::Log(log) => Ok(log),
            _ => Err(protocol()),
        }
    }

    pub async fn usage(&self, session_id: &str) -> Result<SessionUsage> {
        match self
            .call(Request::Usage {
                session_id: session_id.into(),
            })
            .await?
        {
            Response::Usage(usage) => Ok(*usage),
            _ => Err(protocol()),
        }
    }

    pub async fn turn(&self, turn_id: TurnId) -> Result<TurnLookup> {
        match self.call(Request::Turn { turn_id }).await? {
            Response::Turn(turn) => Ok(turn),
            _ => Err(protocol()),
        }
    }

    /// 按 TurnId 找回接受结果；在途时每 25ms 再查，NotFound 与其他错误原样返回。
    pub async fn wait_for_turn(&self, turn_id: TurnId) -> Result<TurnDetail> {
        loop {
            match self.turn(turn_id).await? {
                TurnLookup::Accepted { turn } => return Ok(turn),
                TurnLookup::InFlight => tokio::time::sleep(Duration::from_millis(25)).await,
            }
        }
    }

    pub async fn configure(&self, request: ConfigureRequest) -> Result<ConfigureReply> {
        match self.call(Request::Configure { request }).await? {
            Response::Configure(reply) => Ok(reply),
            _ => Err(protocol()),
        }
    }

    /// 单次发送 configure，区分连接前或确定未提交的拒绝与连接建立后的结果不明。
    pub async fn configure_once(&self, request: ConfigureRequest) -> ConfigureOutcome {
        let mut stream = match endpoint::connect(self.layout.clone()).await {
            Ok(stream) => stream,
            Err(error) => return ConfigureOutcome::Rejected(error),
        };
        if let Err(error) = ipc::write_request(&mut stream, &Request::Configure { request }).await {
            return ConfigureOutcome::Unknown(error);
        }
        match ipc::ResponseReader::default().read(&mut stream).await {
            Ok(Response::Configure(reply)) => ConfigureOutcome::Configured(reply),
            Ok(Response::Error(error))
                if error.kind == LocalErrorKind::Internal
                    || error.recovery.as_ref().is_some_and(|r| {
                        matches!(
                            r.cause,
                            RecoveryCause::CommitFailed | RecoveryCause::WorkerFailed
                        )
                    }) =>
            {
                ConfigureOutcome::Unknown(error)
            }
            Ok(Response::Error(error)) => ConfigureOutcome::Rejected(error),
            Ok(_) => ConfigureOutcome::Unknown(protocol()),
            Err(error) => ConfigureOutcome::Unknown(error),
        }
    }

    pub async fn rewind(
        &self,
        session_id: SessionId,
        target: NodeId,
        workspace: std::path::PathBuf,
    ) -> Result<SessionRecord> {
        match self
            .call(Request::Rewind {
                session_id,
                target,
                workspace,
            })
            .await?
        {
            Response::Rewind(session) => Ok(session),
            _ => Err(protocol()),
        }
    }

    /// 对话向前翻页，一次取得 `HISTORY_TURNS` 轮。
    pub async fn history_before(
        &self,
        session_id: SessionId,
        before: NodeId,
    ) -> Result<LocalHistory> {
        self.fetch_history(session_id, before, HISTORY_TURNS).await
    }

    /// 指定轮数的翻页，只供测试使用。
    #[cfg(any(test, feature = "test-utils"))]
    pub async fn history(
        &self,
        session_id: SessionId,
        before: NodeId,
        turns: u32,
    ) -> Result<LocalHistory> {
        self.fetch_history(session_id, before, turns).await
    }

    async fn fetch_history(
        &self,
        session_id: SessionId,
        before: NodeId,
        turns: u32,
    ) -> Result<LocalHistory> {
        match self
            .call(Request::History {
                session_id,
                before,
                turns,
            })
            .await?
        {
            Response::History(history) => Ok(history),
            _ => Err(protocol()),
        }
    }

    #[cfg(any(test, feature = "test-utils"))]
    pub async fn submit(&self, request: TurnRequest) -> Result<Acceptance> {
        match self.call(Request::Submit { request }).await? {
            Response::Acceptance(value) => Ok(value),
            _ => Err(protocol()),
        }
    }

    /// 仅向会话追加一条用户输入而不触发轮次。
    pub async fn feed(&self, request: FeedRequest) -> Result<FeedReply> {
        match self.call(Request::Feed { request }).await? {
            Response::Feed(reply) => Ok(reply),
            _ => Err(protocol()),
        }
    }

    /// 向会话追加一条批量上下文编辑操作。
    pub async fn context_op(&self, request: ContextOpRequest) -> Result<ContextOpReply> {
        match self.call(Request::ContextOp { request }).await? {
            Response::ContextOp(reply) => Ok(reply),
            _ => Err(protocol()),
        }
    }

    /// 在不新增用户输入的前提下触发新轮次。
    pub async fn trigger(&self, request: TriggerRequest) -> Result<Acceptance> {
        match self.call(Request::Trigger { request }).await? {
            Response::Acceptance(value) => Ok(value),
            _ => Err(protocol()),
        }
    }

    /// 只读检视会话在当前位置或指定节点的有效模型上下文统计。
    pub async fn inspect(
        &self,
        session_id: SessionId,
        position: Option<NodeId>,
    ) -> Result<InspectReply> {
        match self
            .call(Request::Inspect {
                session_id,
                position,
            })
            .await?
        {
            Response::Inspect(reply) => Ok(reply),
            _ => Err(protocol()),
        }
    }

    pub(crate) async fn submit_once(&self, request: TurnRequest) -> SubmissionReply {
        self.send_acceptance_request(Request::Submit { request })
            .await
    }

    pub(crate) async fn trigger_once(&self, request: TriggerRequest) -> SubmissionReply {
        self.send_acceptance_request(Request::Trigger { request })
            .await
    }

    async fn send_acceptance_request(&self, request: Request) -> SubmissionReply {
        let mut stream = match endpoint::connect(self.layout.clone()).await {
            Ok(stream) => stream,
            Err(error) => return SubmissionReply::Rejected(error),
        };
        if let Err(error) = ipc::write_request(&mut stream, &request).await {
            return SubmissionReply::Disconnected(error);
        }
        match ipc::ResponseReader::default().read(&mut stream).await {
            Ok(Response::Acceptance(value)) => SubmissionReply::Accepted(value),
            Ok(Response::Error(error)) => SubmissionReply::Rejected(error),
            Ok(_) => SubmissionReply::Disconnected(protocol()),
            Err(error) => SubmissionReply::Disconnected(error),
        }
    }

    pub async fn cancel(&self, turn_id: TurnId) -> Result<bool> {
        match self.call(Request::Cancel { turn_id }).await? {
            Response::Cancelled { owned } => Ok(owned),
            _ => Err(protocol()),
        }
    }

    pub async fn session_head(&self, session_id: SessionId) -> Result<SessionHead> {
        match self.call(Request::Head { session_id }).await? {
            Response::Head(head) => Ok(head),
            _ => Err(protocol()),
        }
    }

    #[cfg(any(test, feature = "test-utils"))]
    pub async fn cancel_session(&self, session_id: SessionId) -> Result<(Option<TurnId>, bool)> {
        let head = self.session_head(session_id).await?;
        match head.turn_id.filter(|_| head.owner) {
            Some(turn_id) => Ok((Some(turn_id), self.cancel(turn_id).await?)),
            None => Ok((None, false)),
        }
    }

    /// 直接观察会话视图，只供测试使用；前端经 `LocalTurn` 观察。
    #[cfg(any(test, feature = "test-utils"))]
    pub async fn watch(&self, session_id: SessionId) -> Result<LocalWatch> {
        self.open_watch(session_id).await
    }

    pub(crate) async fn open_watch(&self, session_id: SessionId) -> Result<LocalWatch> {
        let mut stream = endpoint::connect(self.layout.clone()).await?;
        ipc::write_request(&mut stream, &Request::Watch { session_id }).await?;
        Ok(LocalWatch::from_stream(stream))
    }
}

/// 独立读任务保存帧的读取进度；取消一次 next 不会丢弃半帧。
pub struct LocalWatch {
    receiver: tokio::sync::mpsc::Receiver<Result<ViewUpdate>>,
    reader: tokio::task::JoinHandle<()>,
    pub(crate) progress: tokio::sync::watch::Receiver<tokio::time::Instant>,
}

impl LocalWatch {
    fn from_stream(mut stream: UnixStream) -> Self {
        let (sender, receiver) = tokio::sync::mpsc::channel(8);
        let (progress, progress_rx) = tokio::sync::watch::channel(tokio::time::Instant::now());
        let reader = tokio::spawn(async move {
            let mut frames = ipc::ResponseReader::default();
            loop {
                let response = tokio::select! {
                    biased;
                    _ = sender.closed() => break,
                    response = frames.next_with_progress(&mut stream, || { progress.send_replace(tokio::time::Instant::now()); }) => response,
                };
                let update = match response {
                    Ok(Some(Response::View(value))) => Ok(value),
                    Ok(Some(Response::Error(error))) => Err(error),
                    Ok(None) => break,
                    Ok(Some(_)) => Err(protocol()),
                    Err(error) => Err(error),
                };
                let failed = update.is_err();
                if sender.send(update).await.is_err() || failed {
                    break;
                }
            }
        });
        Self {
            receiver,
            reader,
            progress: progress_rx,
        }
    }

    pub async fn next(&mut self) -> Result<Option<ViewUpdate>> {
        self.receiver.recv().await.transpose()
    }
}

impl Drop for LocalWatch {
    fn drop(&mut self) {
        self.reader.abort();
    }
}

pub(crate) fn protocol() -> LocalError {
    LocalError::new(LocalErrorKind::Protocol, "unexpected local response")
}

fn peer_replaced() -> LocalError {
    LocalError::new(
        LocalErrorKind::Transport,
        "跨版本停机期间检测到后台实例已变更；未停止新后台，请执行 `lattice daemon status` 复核后再决定是否重试",
    )
}

pub(crate) enum SubmissionReply {
    Accepted(Acceptance),
    Rejected(LocalError),
    Disconnected(LocalError),
}

#[cfg(test)]
mod tests {
    use super::*;
    use lattice_conversation::{ModelId, ProviderId};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    async fn interrupted_view_poll_preserves_a_partial_frame() {
        tokio::time::timeout(Duration::from_secs(1), async {
            let (mut writer, reader) = UnixStream::pair().unwrap();
            let mut view = LocalWatch::from_stream(reader);
            writer.write_all(&ipc::MAGIC[..4]).await.unwrap();
            assert!(
                tokio::time::timeout(Duration::from_millis(10), view.next())
                    .await
                    .is_err()
            );
            let response = Response::View(ViewUpdate::Head(SessionHead {
                model: Some(ModelId::parse("test").unwrap()),
                provider: Some(ProviderId::parse("openai-compatible").unwrap()),
                effort: None,
                settings_version: serde_json::from_value(serde_json::json!("11".repeat(32)))
                    .unwrap(),
                session_id: crate::new_session_id(),
                position: NodeId(0),
                agent: "test".into(),
                turn_id: None,
                owner: false,
                diagnostic: None,
                path_replaced: false,
                has_older: None,
            }));
            let json = serde_json::to_string(&response).unwrap();
            let payload = serde_json::to_vec(&serde_json::json!({
                "message_id": 0,
                "offset": 0,
                "json_fragment": json,
                "complete": true,
            }))
            .unwrap();
            writer.write_all(&ipc::MAGIC[4..]).await.unwrap();
            writer.write_u32(payload.len() as u32).await.unwrap();
            writer.write_all(&payload).await.unwrap();
            assert!(matches!(
                view.next().await.unwrap(),
                Some(ViewUpdate::Head(SessionHead {
                    position: NodeId(0),
                    ..
                }))
            ));
        })
        .await
        .expect("观察流半帧读取超时");
    }

    #[tokio::test]
    async fn im_client_methods_roundtrip_wire_payloads_and_propagate_backend_errors() {
        use lattice_ipc::{ImCredentialStatus, ImPlatformAuthStatus};
        use std::{fs, os::unix::fs::PermissionsExt, path::PathBuf};
        use tokio::net::UnixListener;

        struct TempDir(PathBuf);
        impl Drop for TempDir {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }
        let temp = TempDir(PathBuf::from("/tmp").join(format!(
            "ltc-cli-im-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        )));
        fs::create_dir_all(&temp.0).unwrap();
        let layout = StateLayout::in_state_root(&temp.0);
        let local_dir = layout.lattice_root().join("local");
        fs::create_dir_all(&local_dir).unwrap();
        fs::set_permissions(layout.lattice_root(), fs::Permissions::from_mode(0o700)).unwrap();
        fs::set_permissions(&local_dir, fs::Permissions::from_mode(0o700)).unwrap();

        let sock_path = local_dir.join("api.sock");
        let listener = UnixListener::bind(&sock_path).unwrap();
        fs::set_permissions(&sock_path, fs::Permissions::from_mode(0o600)).unwrap();

        let server = tokio::spawn(async move {
            // 1. ImAuthSet
            let (mut s1, _) = listener.accept().await.unwrap();
            match ipc::read_request::<Request>(&mut s1).await.unwrap() {
                Request::ImAuthSet { platform, token } => {
                    assert_eq!(platform, "telegram");
                    assert_eq!(token.as_str(), "123456:BotSecretToken");
                }
                _ => panic!("预期 Request::ImAuthSet"),
            }
            ipc::ResponseWriter::default()
                .write(
                    &mut s1,
                    &Response::ImAuthSet(ImAuthSetReply {
                        mount: ImMountResult::Mounted,
                    }),
                )
                .await
                .unwrap();

            // 2. ImAuthStatus
            let (mut s2, _) = listener.accept().await.unwrap();
            assert!(matches!(
                ipc::read_request::<Request>(&mut s2).await.unwrap(),
                Request::ImAuthStatus {}
            ));
            ipc::ResponseWriter::default()
                .write(
                    &mut s2,
                    &Response::ImAuthStatus(ImAuthStatus {
                        platforms: vec![ImPlatformAuthStatus {
                            platform: "telegram".into(),
                            credential: ImCredentialStatus::Present,
                        }],
                    }),
                )
                .await
                .unwrap();

            // 3. ImReload
            let (mut s3, _) = listener.accept().await.unwrap();
            assert!(matches!(
                ipc::read_request::<Request>(&mut s3).await.unwrap(),
                Request::ImReload {}
            ));
            ipc::ResponseWriter::default()
                .write(
                    &mut s3,
                    &Response::ImReload(ImMountResult::UsingPreviousConfiguration {
                        reason: "im/config.toml 解析失败".into(),
                    }),
                )
                .await
                .unwrap();

            // 4. ImAuthRemove
            let (mut s4, _) = listener.accept().await.unwrap();
            match ipc::read_request::<Request>(&mut s4).await.unwrap() {
                Request::ImAuthRemove { platform } => assert_eq!(platform, "telegram"),
                _ => panic!("预期 Request::ImAuthRemove"),
            }
            ipc::ResponseWriter::default()
                .write(
                    &mut s4,
                    &Response::ImAuthRemove(ImAuthRemoveReply {
                        removed: true,
                        mount: ImMountResult::Unmounted {
                            reason: "缺少 Telegram 平台凭据（im/auth.json）".into(),
                        },
                    }),
                )
                .await
                .unwrap();

            // 5. Response::Error 传播
            let (mut s5, _) = listener.accept().await.unwrap();
            let _ = ipc::read_request::<Request>(&mut s5).await.unwrap();
            ipc::ResponseWriter::default()
                .write(
                    &mut s5,
                    &Response::Error(LocalError::new(
                        LocalErrorKind::BadRequest,
                        "不支持的 IM 平台",
                    )),
                )
                .await
                .unwrap();
        });

        let client = LocalClient::new(layout);
        let set_reply = client
            .im_auth_set("telegram", "123456:BotSecretToken")
            .await
            .unwrap();
        assert_eq!(set_reply.mount, ImMountResult::Mounted);

        let status = client.im_auth_status().await.unwrap();
        assert_eq!(status.platforms.len(), 1);
        assert_eq!(status.platforms[0].platform, "telegram");
        assert_eq!(status.platforms[0].credential, ImCredentialStatus::Present);

        let reload = client.im_reload().await.unwrap();
        assert_eq!(
            reload,
            ImMountResult::UsingPreviousConfiguration {
                reason: "im/config.toml 解析失败".into()
            }
        );

        let remove_reply = client.im_auth_remove("telegram").await.unwrap();
        assert!(remove_reply.removed);
        assert!(matches!(
            remove_reply.mount,
            ImMountResult::Unmounted { .. }
        ));

        let bad = client.im_auth_remove("discord").await.unwrap_err();
        assert_eq!(bad.kind, LocalErrorKind::BadRequest);
        assert_eq!(bad.message, "不支持的 IM 平台");

        server.await.unwrap();
    }

    #[tokio::test]
    async fn probe_status_distinguishes_older_backend_and_preserves_peer_executable() {
        use std::{fs, os::unix::fs::PermissionsExt, path::PathBuf};
        use tokio::net::UnixListener;

        struct TempDir(PathBuf);
        impl Drop for TempDir {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }
        let make_layout = |tag: &str| {
            let temp = TempDir(PathBuf::from("/tmp").join(format!(
                "ltc-probe-{tag}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            )));
            fs::create_dir_all(&temp.0).unwrap();
            let layout = StateLayout::in_state_root(&temp.0);
            let local_dir = layout.lattice_root().join("local");
            fs::create_dir_all(&local_dir).unwrap();
            fs::set_permissions(layout.lattice_root(), fs::Permissions::from_mode(0o700)).unwrap();
            fs::set_permissions(&local_dir, fs::Permissions::from_mode(0o700)).unwrap();
            let sock_path = local_dir.join("api.sock");
            let listener = UnixListener::bind(&sock_path).unwrap();
            fs::set_permissions(&sock_path, fs::Permissions::from_mode(0o600)).unwrap();
            (temp, layout, listener)
        };

        // 1. 更旧协议：单次探测识别为 OlderBackend，并带出发请求前读取的对端可执行文件路径；
        //    `status()` 仍只向外暴露同一个不兼容错误。
        let (_t1, layout, listener) = make_layout("older");
        let server = tokio::spawn(async move {
            for _ in 0..2 {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut magic = [0_u8; 8];
                stream.read_exact(&mut magic).await.unwrap();
                let older = ipc::magic_for_version(ipc::PROTOCOL_VERSION - 1);
                stream.write_all(&older).await.unwrap();
                stream.write_u32(4).await.unwrap();
                stream.write_all(b"null").await.unwrap();
            }
        });
        let client = LocalClient::new(layout);
        let probe = client
            .probe_status(Duration::from_secs(2))
            .await
            .expect("更旧协议后台应作为探测结果返回");
        match probe {
            StatusProbe::OlderBackend {
                peer_pid,
                peer_executable,
                error,
            } => {
                assert_eq!(peer_pid, Some(std::process::id() as i32));
                assert_eq!(
                    peer_executable,
                    Some(std::env::current_exe().expect("读取当前测试进程路径"))
                );
                assert_eq!(error.kind, LocalErrorKind::IncompatibleBackend);
                assert!(
                    error.message.contains("版本较旧且不兼容"),
                    "诊断：{}",
                    error.message
                );
            }
            other => panic!("更旧协议后台应识别为 OlderBackend，实际：{other:?}"),
        }
        let err = client
            .status()
            .await
            .expect_err("status() 对旧后台必须仍返回错误");
        assert_eq!(err.kind, LocalErrorKind::IncompatibleBackend);
        assert!(err.message.contains("版本较旧且不兼容"));
        server.await.unwrap();

        // 2. 更新协议：单次探测直接返回同一个拒绝错误。
        let (_t2, layout, listener) = make_layout("newer");
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut magic = [0_u8; 8];
            stream.read_exact(&mut magic).await.unwrap();
            let newer = ipc::magic_for_version(ipc::PROTOCOL_VERSION + 1);
            stream.write_all(&newer).await.unwrap();
            stream.write_u32(4).await.unwrap();
            stream.write_all(b"null").await.unwrap();
        });
        let err = LocalClient::new(layout)
            .probe_status(Duration::from_secs(2))
            .await
            .expect_err("更新协议后台必须拒绝");
        assert_eq!(err.kind, LocalErrorKind::IncompatibleBackend);
        assert!(err.message.contains("版本较新且不兼容"));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn stop_uses_control_frame_for_older_backend_and_refuses_newer_or_silent_close() {
        use std::{fs, os::unix::fs::PermissionsExt, path::PathBuf};
        use tokio::net::UnixListener;

        struct TempDir(PathBuf);
        impl Drop for TempDir {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }
        let make_layout = |tag: &str| {
            let temp = TempDir(PathBuf::from("/tmp").join(format!(
                "ltc-stop-{tag}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            )));
            fs::create_dir_all(&temp.0).unwrap();
            let layout = StateLayout::in_state_root(&temp.0);
            let local_dir = layout.lattice_root().join("local");
            fs::create_dir_all(&local_dir).unwrap();
            fs::set_permissions(layout.lattice_root(), fs::Permissions::from_mode(0o700)).unwrap();
            fs::set_permissions(&local_dir, fs::Permissions::from_mode(0o700)).unwrap();
            let sock_path = local_dir.join("api.sock");
            let listener = UnixListener::bind(&sock_path).unwrap();
            fs::set_permissions(&sock_path, fs::Permissions::from_mode(0o600)).unwrap();
            (temp, layout, sock_path, listener)
        };

        // 1. 更旧协议版本且支持 LTCTRL 控制帧：首连回写旧魔数，次连走 LTCTRL 完成排空并释放状态根。
        let (_t1, layout, sock_path, listener) = make_layout("ctrl");
        let server = tokio::spawn(async move {
            let (mut s1, _) = listener.accept().await.unwrap();
            let mut magic = [0_u8; 8];
            s1.read_exact(&mut magic).await.unwrap();
            let older = ipc::magic_for_version(ipc::PROTOCOL_VERSION - 1);
            s1.write_all(&older).await.unwrap();
            s1.write_u32(4).await.unwrap();
            s1.write_all(b"null").await.unwrap();
            drop(s1);

            let (mut s2, _) = listener.accept().await.unwrap();
            match ipc::read_incoming_request::<Request>(&mut s2)
                .await
                .unwrap()
            {
                ipc::IncomingRequest::Control(ipc::ControlRequest::Stop {
                    expected_protocol_version,
                }) => {
                    assert_eq!(expected_protocol_version, ipc::PROTOCOL_VERSION - 1);
                    ipc::write_control(
                        &mut s2,
                        &ipc::ControlStopResponse::Stopped(ipc::ControlStopStatus {
                            epoch: "legacy-42".into(),
                            owners: 0,
                            unconfirmed_operations: 1,
                        }),
                    )
                    .await
                    .unwrap();
                }
                _ => panic!("第二次连接必须发送版本无关控制停机帧"),
            }
            drop(s2);
            drop(listener);
            let _ = fs::remove_file(&sock_path);
        });
        let outcome = LocalClient::new(layout)
            .stop_within(Duration::from_secs(2))
            .await
            .unwrap();
        assert_eq!(
            outcome,
            StopOutcome::CrossVersionControl {
                epoch: "legacy-42".into(),
                owners: 0,
                unconfirmed_operations: 1,
            }
        );
        server.await.unwrap();

        // 2. 更新协议版本：拒绝停止，不尝试控制帧，也不发信号。
        let (_t2, layout, _sock_path, listener) = make_layout("newer");
        let server = tokio::spawn(async move {
            let (mut s1, _) = listener.accept().await.unwrap();
            let mut magic = [0_u8; 8];
            s1.read_exact(&mut magic).await.unwrap();
            let newer = ipc::magic_for_version(ipc::PROTOCOL_VERSION + 1);
            s1.write_all(&newer).await.unwrap();
            s1.write_u32(4).await.unwrap();
            s1.write_all(b"null").await.unwrap();
        });
        let err = LocalClient::new(layout)
            .stop_within(Duration::from_secs(2))
            .await
            .unwrap_err();
        assert_eq!(err.kind, LocalErrorKind::IncompatibleBackend);
        assert!(err.message.contains("版本较新且不兼容"));
        server.await.unwrap();

        // 3. 静默断连：报 Transport 提示，绝不进入信号兜底。
        let (_t3, layout, _sock_path, listener) = make_layout("silent");
        let server = tokio::spawn(async move {
            let (mut s1, _) = listener.accept().await.unwrap();
            let mut magic = [0_u8; 8];
            s1.read_exact(&mut magic).await.unwrap();
        });
        let err = LocalClient::new(layout)
            .stop_within(Duration::from_secs(2))
            .await
            .unwrap_err();
        assert_eq!(err.kind, LocalErrorKind::Transport);
        assert!(err.message.contains("systemctl --user stop lattice"));
        server.await.unwrap();

        // 4. 首连确认更旧协议后、次连前后台在同一路径上被更新版本实例替换：
        //    客户端必须在发控制帧前察觉 socket/进程变化并中止，绝不向新实例发送控制停机帧。
        let (_t4, layout, sock_path, listener) = make_layout("replaced-newer");
        let replacement_touched = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let touched_flag = std::sync::Arc::clone(&replacement_touched);
        let server = tokio::spawn(async move {
            let (mut s1, _) = listener.accept().await.unwrap();
            let mut magic = [0_u8; 8];
            s1.read_exact(&mut magic).await.unwrap();
            drop(listener);
            fs::remove_file(&sock_path).unwrap();
            let replacement = UnixListener::bind(&sock_path).unwrap();
            fs::set_permissions(&sock_path, fs::Permissions::from_mode(0o600)).unwrap();

            // 替换就绪后再向首连回写旧魔数，确保次连必然落在新监听套接字上。
            let older = ipc::magic_for_version(ipc::PROTOCOL_VERSION - 1);
            s1.write_all(&older).await.unwrap();
            s1.write_u32(4).await.unwrap();
            s1.write_all(b"null").await.unwrap();
            drop(s1);

            if let Ok(Ok((mut s2, _))) =
                tokio::time::timeout(Duration::from_millis(200), replacement.accept()).await
            {
                let mut probe = [0_u8; 1];
                if let Ok(Ok(n)) =
                    tokio::time::timeout(Duration::from_millis(200), s2.read(&mut probe)).await
                    && n > 0
                {
                    touched_flag.store(true, std::sync::atomic::Ordering::SeqCst);
                }
            }
        });
        let err = LocalClient::new(layout)
            .stop_within(Duration::from_secs(2))
            .await
            .unwrap_err();
        assert_eq!(err.kind, LocalErrorKind::Transport);
        assert!(
            err.message.contains("后台实例已变更") && err.message.contains("未停止新后台"),
            "替换竞态应明确中止且不误停新后台：{}",
            err.message
        );
        server.await.unwrap();
        assert!(
            !replacement_touched.load(std::sync::atomic::Ordering::SeqCst),
            "检测到后台替换后不得向新监听者发送控制停机字节"
        );
    }

    #[tokio::test]
    async fn stop_falls_back_to_pidfd_sigterm_for_legacy_older_child_process() {
        use std::{fs, os::unix::fs::PermissionsExt, path::PathBuf, process::Command};

        struct TempDir(PathBuf);
        impl Drop for TempDir {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }
        let temp = TempDir(PathBuf::from("/tmp").join(format!(
            "ltc-legacy-sig-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        )));
        fs::create_dir_all(&temp.0).unwrap();
        let layout = StateLayout::in_state_root(&temp.0);
        let local_dir = layout.lattice_root().join("local");
        fs::create_dir_all(&local_dir).unwrap();
        fs::set_permissions(layout.lattice_root(), fs::Permissions::from_mode(0o700)).unwrap();
        fs::set_permissions(&local_dir, fs::Permissions::from_mode(0o700)).unwrap();

        let exe = std::env::current_exe().expect("获取测试二进制路径");
        let mut child = Command::new(exe)
            .arg("--exact")
            .arg("client::tests::legacy_older_daemon_child_helper")
            .arg("--ignored")
            .arg("--nocapture")
            .env("LATTICE_TEST_LEGACY_STATE_ROOT", &temp.0)
            .spawn()
            .expect("拉起模拟历史旧后台子进程");
        let child_pid = child.id() as i32;

        let sock_path = local_dir.join("api.sock");
        let ready_deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !sock_path.exists() {
            assert!(
                std::time::Instant::now() < ready_deadline,
                "模拟历史旧后台未在 5 秒内发布 api.sock"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        let outcome = LocalClient::new(layout.clone())
            .stop_within(Duration::from_secs(5))
            .await
            .expect("跨版本信号兜底应成功停止历史旧后台并等待释放状态根");
        assert_eq!(
            outcome,
            StopOutcome::CrossVersionSignal {
                peer_pid: child_pid
            }
        );

        let exit_status = tokio::task::spawn_blocking(move || child.wait())
            .await
            .unwrap()
            .expect("等待模拟子进程退出");
        assert!(
            exit_status.success(),
            "模拟历史旧后台应收到 SIGTERM 并正常退出 0，实际：{exit_status:?}"
        );
        assert!(!sock_path.exists(), "历史旧后台退出后 api.sock 应已撤下");
    }

    #[tokio::test]
    #[ignore = "供 stop_falls_back_to_pidfd_sigterm_for_legacy_older_child_process 拉起真实独立进程"]
    async fn legacy_older_daemon_child_helper() {
        use std::{fs, os::unix::fs::PermissionsExt, path::PathBuf};
        use tokio::net::UnixListener;

        let Some(root) = std::env::var_os("LATTICE_TEST_LEGACY_STATE_ROOT") else {
            return;
        };
        let root = PathBuf::from(root);
        let layout = StateLayout::in_state_root(&root);
        let local_dir = layout.lattice_root().join("local");
        let lock_path = local_dir.join("owner.lock");
        let lock = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&lock_path)
            .unwrap();
        rustix::fs::flock(&lock, rustix::fs::FlockOperation::NonBlockingLockExclusive).unwrap();

        let sock_path = local_dir.join("api.sock");
        let listener = UnixListener::bind(&sock_path).unwrap();
        fs::set_permissions(&sock_path, fs::Permissions::from_mode(0o600)).unwrap();

        let mut sigterm =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).unwrap();
        let older = ipc::magic_for_version(ipc::PROTOCOL_VERSION - 1);
        let deadline = tokio::time::sleep(Duration::from_secs(10));
        tokio::pin!(deadline);

        loop {
            tokio::select! {
                _ = sigterm.recv() => break,
                _ = &mut deadline => panic!("等待 SIGTERM 超时"),
                accepted = listener.accept() => {
                    let (mut stream, _) = accepted.unwrap();
                    let mut header = [0_u8; 8];
                    let _ = stream.read_exact(&mut header).await;
                    let _ = stream.write_all(&older).await;
                    let _ = stream.write_u32(4).await;
                    let _ = stream.write_all(b"null").await;
                }
            }
        }
        drop(listener);
        let _ = fs::remove_file(&sock_path);
        drop(lock);
    }

    /// 跨进程竞态回归：独立旧协议子进程在首连确认旧版本后被另一进程（当前进程）替换，
    /// 分别覆盖“旧进程存活但 PID/socket 已变”与“旧进程已退出（pidfd 就绪）”两种窗口，
    /// 确认 `stop` 立即中止、既不向新监听者发送控制帧字节，也不误发 SIGTERM。
    #[tokio::test]
    async fn stop_aborts_when_older_child_process_is_replaced_before_second_connect() {
        use std::{
            fs,
            io::{BufRead, BufReader, Write},
            os::unix::fs::PermissionsExt,
            path::PathBuf,
            process::{Command, Stdio},
            sync::{
                Arc,
                atomic::{AtomicBool, Ordering},
            },
        };
        use tokio::net::UnixListener;

        struct TempDir(PathBuf);
        impl Drop for TempDir {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }

        for exit_after_reply in [false, true] {
            let temp = TempDir(PathBuf::from("/tmp").join(format!(
                "ltc-replace-race-{exit_after_reply}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            )));
            fs::create_dir_all(&temp.0).unwrap();
            let layout = StateLayout::in_state_root(&temp.0);
            let local_dir = layout.lattice_root().join("local");
            fs::create_dir_all(&local_dir).unwrap();
            fs::set_permissions(layout.lattice_root(), fs::Permissions::from_mode(0o700)).unwrap();
            fs::set_permissions(&local_dir, fs::Permissions::from_mode(0o700)).unwrap();
            let sock_path = local_dir.join("api.sock");

            let exe = std::env::current_exe().expect("获取测试二进制路径");
            let mut child = Command::new(exe)
                .arg("--exact")
                .arg("client::tests::legacy_older_daemon_replace_race_helper")
                .arg("--ignored")
                .arg("--nocapture")
                .env("LATTICE_TEST_REPLACE_STATE_ROOT", &temp.0)
                .env(
                    "LATTICE_TEST_REPLACE_EXIT_AFTER_REPLY",
                    if exit_after_reply { "1" } else { "0" },
                )
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .spawn()
                .expect("拉起跨进程替换竞态子进程");

            let mut stdin = child.stdin.take().unwrap();
            let stdout = BufReader::new(child.stdout.take().unwrap());
            let read_until_marker = |mut reader: BufReader<std::process::ChildStdout>,
                                     expected: &'static str| {
                let mut seen = Vec::new();
                loop {
                    let mut line = String::new();
                    let n = reader.read_line(&mut line).unwrap();
                    assert!(
                        n > 0,
                        "等待子进程标记 `{expected}` 时遇到 EOF；已读输出：{seen:?}"
                    );
                    if line.trim() == expected {
                        return reader;
                    }
                    seen.push(line);
                }
            };

            let stdout = tokio::task::spawn_blocking(move || read_until_marker(stdout, "BOUND"))
                .await
                .unwrap();

            let stop_layout = layout.clone();
            let stop_task = tokio::spawn(async move {
                LocalClient::new(stop_layout)
                    .stop_within(Duration::from_secs(3))
                    .await
            });

            let _stdout =
                tokio::task::spawn_blocking(move || read_until_marker(stdout, "UNLINKED"))
                    .await
                    .unwrap();

            // 由另一进程（当前进程）在原路径重新绑定替换监听者，然后放行子进程回写旧魔数。
            let replacement = UnixListener::bind(&sock_path).unwrap();
            fs::set_permissions(&sock_path, fs::Permissions::from_mode(0o600)).unwrap();
            let touched = Arc::new(AtomicBool::new(false));
            let touched_flag = Arc::clone(&touched);
            let replacement_task = tokio::spawn(async move {
                if let Ok(Ok((mut s2, _))) =
                    tokio::time::timeout(Duration::from_millis(300), replacement.accept()).await
                {
                    let mut probe = [0_u8; 1];
                    if let Ok(Ok(n)) =
                        tokio::time::timeout(Duration::from_millis(200), s2.read(&mut probe)).await
                        && n > 0
                    {
                        touched_flag.store(true, Ordering::SeqCst);
                    }
                }
            });

            stdin = tokio::task::spawn_blocking(move || {
                writeln!(stdin, "REPLY").unwrap();
                stdin.flush().unwrap();
                stdin
            })
            .await
            .unwrap();

            let err = tokio::time::timeout(Duration::from_secs(5), stop_task)
                .await
                .expect("跨进程替换竞态 stop 不应超时")
                .unwrap()
                .expect_err("检测到跨进程后台替换后必须中止");
            assert_eq!(err.kind, LocalErrorKind::Transport);
            assert!(
                err.message.contains("后台实例已变更") && err.message.contains("未停止新后台"),
                "exit_after_reply={exit_after_reply} 错误文案不符合预期：{}",
                err.message
            );
            replacement_task.await.unwrap();
            assert!(
                !touched.load(Ordering::SeqCst),
                "exit_after_reply={exit_after_reply} 不得向替换监听者发送控制帧字节"
            );

            if !exit_after_reply {
                // 确认旧子进程并未误收 SIGTERM，仍可按协议指令正常退出 0。
                assert!(
                    child.try_wait().unwrap().is_none(),
                    "替换中止时不得继续向旧子进程发送 SIGTERM"
                );
                tokio::task::spawn_blocking(move || {
                    let _ = writeln!(stdin, "EXIT");
                    let _ = stdin.flush();
                })
                .await
                .unwrap();
            } else {
                drop(stdin);
            }
            let status = tokio::task::spawn_blocking(move || child.wait())
                .await
                .unwrap()
                .unwrap();
            assert!(
                status.success(),
                "exit_after_reply={exit_after_reply} 子进程应正常退出 0，实际：{status:?}"
            );
        }
    }

    #[tokio::test]
    #[ignore = "供 stop_aborts_when_older_child_process_is_replaced_before_second_connect 拉起真实独立进程"]
    async fn legacy_older_daemon_replace_race_helper() {
        use std::{
            fs,
            io::{BufRead, Write},
            os::unix::fs::PermissionsExt,
            path::PathBuf,
        };
        use tokio::net::UnixListener;

        let Some(root) = std::env::var_os("LATTICE_TEST_REPLACE_STATE_ROOT") else {
            return;
        };
        let exit_after_reply =
            std::env::var("LATTICE_TEST_REPLACE_EXIT_AFTER_REPLY").as_deref() == Ok("1");
        let root = PathBuf::from(root);
        let layout = StateLayout::in_state_root(&root);
        let sock_path = layout.lattice_root().join("local/api.sock");
        let listener = UnixListener::bind(&sock_path).unwrap();
        fs::set_permissions(&sock_path, fs::Permissions::from_mode(0o600)).unwrap();

        {
            let mut out = std::io::stdout().lock();
            writeln!(out, "BOUND").unwrap();
            out.flush().unwrap();
        }

        let (mut stream, _) = listener.accept().await.unwrap();
        let mut header = [0_u8; 8];
        stream.read_exact(&mut header).await.unwrap();
        drop(listener);
        fs::remove_file(&sock_path).unwrap();
        {
            let mut out = std::io::stdout().lock();
            writeln!(out, "UNLINKED").unwrap();
            out.flush().unwrap();
        }

        // 等父进程完成替换套接字绑定后再向首连回写旧魔数。
        tokio::task::spawn_blocking(|| {
            let mut line = String::new();
            std::io::stdin().lock().read_line(&mut line).unwrap();
        })
        .await
        .unwrap();

        let older = ipc::magic_for_version(ipc::PROTOCOL_VERSION - 1);
        let _ = stream.write_all(&older).await;
        let _ = stream.write_u32(4).await;
        let _ = stream.write_all(b"null").await;
        drop(stream);

        if !exit_after_reply {
            tokio::task::spawn_blocking(|| {
                let mut line = String::new();
                let _ = std::io::stdin().lock().read_line(&mut line);
            })
            .await
            .unwrap();
        }
    }
}
