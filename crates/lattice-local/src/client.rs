// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 本地 IPC 客户端句柄、后台自动启动与观察流帧读取。

use std::time::Duration;

use lattice_ipc::{
    Acceptance, AuthKey, AuthRemoval, AuthStatus, ConfigureReply, ConfigureRequest, DaemonStatus,
    HISTORY_TURNS, LocalError, LocalErrorKind, LocalHistory, ModelCandidateQuery, ModelCandidates,
    NodeId, RecoveryCause, Request, Response, Result, RevocationStatus, SessionHead, SessionId,
    SessionListRow, SessionLog, SessionRecord, SessionUsage, TurnDetail, TurnId, TurnLookup,
    TurnRequest, ViewUpdate, ipc, require_chatgpt,
};
use lattice_store::StateLayout;
use tokio::net::UnixStream;

use crate::{endpoint, login::LocalAuthLogin, operation::ConfigureOutcome};

/// stop 回执后等待旧后台释放实例锁的上限；覆盖回执后收尾连接时的请求读取与写出超时。
const STOP_RELEASE_TIMEOUT: Duration = Duration::from_secs(30);

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
        match tokio::time::timeout(Duration::from_secs(5), self.call(Request::Status))
            .await
            .map_err(|_| LocalError::new(LocalErrorKind::Transport, "backend status timed out"))??
        {
            Response::Status(status) => Ok(status),
            _ => Err(protocol()),
        }
    }

    /// 回执之后再等 socket 撤下、`owner.lock` 可取得才返回，调用方随即可在同一 STATE_ROOT 启动新后台。
    /// 不等待后台进程退出。超过 `STOP_RELEASE_TIMEOUT` 仍未放开时报 Stopping，需用 status 复核。
    pub async fn stop(&self) -> Result<DaemonStatus> {
        self.stop_within(STOP_RELEASE_TIMEOUT).await
    }

    /// 等待上限由调用方给出；正式入口固定用 `STOP_RELEASE_TIMEOUT`，测试以短上限核验超时报错。
    #[cfg(any(test, feature = "test-utils"))]
    pub async fn stop_within(&self, timeout: Duration) -> Result<DaemonStatus> {
        self.stop_within_inner(timeout).await
    }

    #[cfg(not(any(test, feature = "test-utils")))]
    async fn stop_within(&self, timeout: Duration) -> Result<DaemonStatus> {
        self.stop_within_inner(timeout).await
    }

    async fn stop_within_inner(&self, timeout: Duration) -> Result<DaemonStatus> {
        let (status, departure) = self.request_stop().await?;
        departure.wait(timeout).await?;
        Ok(status)
    }

    /// 只取得 stop 回执：后台已排空，但 listener、socket 与实例锁可能尚未放开。
    #[cfg(any(test, feature = "test-utils"))]
    pub async fn request_stop(&self) -> Result<(DaemonStatus, endpoint::Departure)> {
        self.request_stop_inner().await
    }

    #[cfg(not(any(test, feature = "test-utils")))]
    async fn request_stop(&self) -> Result<(DaemonStatus, endpoint::Departure)> {
        self.request_stop_inner().await
    }

    async fn request_stop_inner(&self) -> Result<(DaemonStatus, endpoint::Departure)> {
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

    /// 对话向前翻页时默认取得十轮，显式轮数使用 history。
    pub async fn history_before(
        &self,
        session_id: SessionId,
        before: NodeId,
    ) -> Result<LocalHistory> {
        self.history(session_id, before, HISTORY_TURNS).await
    }

    pub async fn history(
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

    pub async fn submit(&self, request: TurnRequest) -> Result<Acceptance> {
        match self.call(Request::Submit { request }).await? {
            Response::Acceptance(value) => Ok(value),
            _ => Err(protocol()),
        }
    }

    pub(crate) async fn submit_once(&self, request: TurnRequest) -> SubmissionReply {
        let mut stream = match endpoint::connect(self.layout.clone()).await {
            Ok(stream) => stream,
            Err(error) => return SubmissionReply::Rejected(error),
        };
        if let Err(error) = ipc::write_request(&mut stream, &Request::Submit { request }).await {
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

    pub async fn cancel_session(&self, session_id: SessionId) -> Result<(Option<TurnId>, bool)> {
        let head = self.session_head(session_id).await?;
        match head.turn_id.filter(|_| head.owner) {
            Some(turn_id) => Ok((Some(turn_id), self.cancel(turn_id).await?)),
            None => Ok((None, false)),
        }
    }

    pub async fn watch(&self, session_id: SessionId) -> Result<LocalWatch> {
        let mut stream = endpoint::connect(self.layout.clone()).await?;
        ipc::write_request(&mut stream, &Request::Watch { session_id }).await?;
        Ok(LocalWatch::from_stream(stream))
    }

    /// 自启当前 CLI，只有端点缺失才启动；不替换不兼容后台。
    pub async fn ensure_running(&self) -> Result<DaemonStatus> {
        match self.status().await {
            Ok(status) => return Ok(status),
            Err(error) if error.kind == LocalErrorKind::NotRunning => {}
            Err(error) => return Err(error),
        }
        let state_root = self
            .layout
            .lattice_root()
            .parent()
            .ok_or_else(protocol)?
            .to_path_buf();
        tokio::task::spawn_blocking(move || {
            use std::os::unix::process::CommandExt;
            use std::process::{Command, Stdio};
            let mut command = Command::new(std::env::current_exe()?);
            command
                .args(["daemon", "serve", "--state-root"])
                .arg(&state_root)
                .current_dir("/")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            // fork 后只调用 async-signal-safe setsid，使后台脱离终端进程组。
            unsafe {
                command.pre_exec(|| {
                    if libc::setsid() < 0 {
                        Err(std::io::Error::last_os_error())
                    } else {
                        Ok(())
                    }
                });
            }
            let mut child = command.spawn()?;
            std::thread::spawn(move || {
                let _ = child.wait();
            });
            Ok::<_, std::io::Error>(())
        })
        .await
        .map_err(|_| protocol())??;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            match self.status().await {
                Ok(status) => return Ok(status),
                Err(error)
                    if error.kind == LocalErrorKind::NotRunning
                        && tokio::time::Instant::now() < deadline =>
                {
                    tokio::time::sleep(Duration::from_millis(25)).await
                }
                Err(error) => return Err(error),
            }
        }
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

pub(crate) enum SubmissionReply {
    Accepted(Acceptance),
    Rejected(LocalError),
    Disconnected(LocalError),
}

#[cfg(test)]
mod tests {
    use super::*;
    use lattice_conversation::{ModelId, ProviderId};
    use tokio::io::AsyncWriteExt;

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
}
