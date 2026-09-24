// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use super::{FrozenConfiguration, ipc, types::*};
use std::time::Duration;
use tokio::net::UnixStream;

/// 具体本地客户端。每个操作使用独立连接；Drop 只断开，不取消后台任务。
#[derive(Clone)]
pub struct LocalClient {
    layout: crate::StateLayout,
    #[cfg(test)]
    pub(super) finished_received: Option<std::sync::Arc<tokio::sync::Notify>>,
    #[cfg(test)]
    pub(super) body_retirement: Option<std::sync::Arc<super::client_tests::BodyRetirementGate>>,
    #[cfg(test)]
    pub(super) refix_retirement: Option<std::sync::Arc<super::client_tests::RefixRetirementGate>>,
}

impl LocalClient {
    pub fn new(layout: crate::StateLayout) -> Self {
        Self {
            layout,
            #[cfg(test)]
            finished_received: None,
            #[cfg(test)]
            body_retirement: None,
            #[cfg(test)]
            refix_retirement: None,
        }
    }

    async fn call(&self, request: Request) -> Result<Response> {
        let mut stream = ipc::connect(self.layout.clone()).await?;
        ipc::write(&mut stream, &request).await?;
        match ipc::read(&mut stream).await? {
            Response::Error(error) => Err(error),
            response => Ok(response),
        }
    }

    pub async fn status(&self) -> Result<DaemonStatus> {
        let response = tokio::time::timeout(Duration::from_secs(5), self.call(Request::Status))
            .await
            .map_err(|_| {
                LocalError::new(LocalErrorKind::Transport, "backend status timed out")
            })??;
        match response {
            Response::Status(status) => Ok(status),
            _ => Err(protocol()),
        }
    }

    /// 停止接受新任务并排空；连接丢失不证明后台已停止。
    pub async fn stop(&self) -> Result<DaemonStatus> {
        match self.call(Request::Stop).await? {
            Response::Status(status) => Ok(status),
            _ => Err(protocol()),
        }
    }
    /// 按固定 tip 分页；之后的 reset/checkout 不改变这些不可变节点。
    pub async fn history_page(
        &self,
        session_id: &str,
        head: &str,
        offset: usize,
        limit: usize,
    ) -> Result<Vec<String>> {
        match self
            .call(Request::History {
                session_id: session_id.into(),
                head: head.into(),
                offset,
                limit,
            })
            .await?
        {
            Response::History(value) => Ok(value),
            _ => Err(protocol()),
        }
    }

    /// 前端共同使用原有 provider-neutral 消息编码，不各自实现 JSONL 解析。
    pub async fn read_messages(
        &self,
        session_id: &str,
        commit_id: &str,
    ) -> Result<Vec<super::NativeMessage<super::BoundToolCall>>> {
        let mut messages = Vec::new();
        let mut remaining = String::new();
        let mut offset = 0;
        loop {
            let page = self
                .read_transcript_page(session_id, commit_id, offset, 65_536)
                .await?;
            remaining.push_str(&page.jsonl);
            while let Some(end) = remaining.find('\n') {
                let line = remaining.drain(..=end).collect::<String>();
                messages.push(
                    serde_json::from_str(line.trim_end_matches('\n')).map_err(|_| protocol())?,
                );
            }
            match page.next_offset {
                Some(next) => offset = next,
                None => break,
            }
        }
        if !remaining.is_empty() {
            messages.push(serde_json::from_str(&remaining).map_err(|_| protocol())?);
        }
        Ok(messages)
    }

    pub async fn list_sessions(&self) -> Result<Vec<crate::SessionGraphSummary>> {
        match self.call(Request::List).await? {
            Response::Sessions(value) => Ok(value),
            _ => Err(protocol()),
        }
    }

    pub async fn session_status(&self, session_id: &str) -> Result<crate::SessionStatus> {
        match self
            .call(Request::SessionStatus {
                session_id: session_id.into(),
            })
            .await?
        {
            Response::SessionStatus(value) => Ok(value),
            _ => Err(protocol()),
        }
    }
    /// 不重新 query；分页、merge 顺序和 pending 均依据传入的不可变对象。
    pub async fn read_history(
        &self,
        view: &SessionObservation,
        scope: HistoryScope,
    ) -> std::result::Result<LocalHistory, HistoryReadError> {
        let mut history = LocalHistory {
            view: view.clone(),
            entries: Vec::new(),
        };
        let session = &view.session.session_id;
        match scope {
            HistoryScope::SelectedTurn => {
                let selected = view
                    .requested_turn
                    .clone()
                    .or_else(|| view.active_turn.clone().map(TurnObservation::Pending));
                if let Some(turn) = selected {
                    history.entries.push(self.history_entry(view, turn).await?);
                }
            }
            HistoryScope::Session => {
                if let Some(head) = &view.head_commit_id {
                    let mut offset = 0;
                    loop {
                        let page = self
                            .history_page(session, head, offset, 128)
                            .await
                            .map_err(|error| HistoryReadError {
                                context: HistoryReadContext::Page {
                                    head: head.clone(),
                                    offset,
                                },
                                error,
                            })?;
                        if page.is_empty() {
                            break;
                        }
                        offset += page.len();
                        for object_id in page {
                            let turn =
                                self.read_commit(session, &object_id)
                                    .await
                                    .map_err(|error| HistoryReadError {
                                        context: HistoryReadContext::Commit { object_id },
                                        error,
                                    })?;
                            history.entries.push(
                                self.history_entry(
                                    view,
                                    TurnObservation::Committed(Box::new(turn)),
                                )
                                .await?,
                            );
                        }
                    }
                }
                if let Some(pending) = &view.active_turn {
                    history.entries.push(
                        self.history_entry(view, TurnObservation::Pending(pending.clone()))
                            .await?,
                    );
                }
            }
        }
        Ok(history)
    }

    async fn history_entry(
        &self,
        view: &SessionObservation,
        turn: TurnObservation,
    ) -> std::result::Result<LocalHistoryEntry, HistoryReadError> {
        let (object_id, agent_name) = match &turn {
            TurnObservation::Pending(pending) => {
                (&pending.journal_oid, &view.session.active_agent_name)
            }
            TurnObservation::Committed(saved) => (&saved.commit_id, &saved.agent_name),
        };
        let messages = self
            .read_messages(&view.session.session_id, object_id)
            .await
            .map_err(|error| HistoryReadError {
                context: HistoryReadContext::Transcript {
                    object_id: object_id.clone(),
                },
                error,
            })?;
        Ok(LocalHistoryEntry {
            agent_name: agent_name.clone(),
            turn,
            messages,
        })
    }

    pub async fn log(&self, session_id: &str) -> Result<Vec<crate::TurnLogEntry>> {
        match self
            .call(Request::Log {
                session_id: session_id.into(),
            })
            .await?
        {
            Response::Log(value) => Ok(value),
            _ => Err(protocol()),
        }
    }

    pub async fn diff(&self, session_id: &str, from: &str, to: &str) -> Result<crate::GraphDiff> {
        match self
            .call(Request::Diff {
                session_id: session_id.into(),
                from: from.into(),
                to: to.into(),
            })
            .await?
        {
            Response::Diff(value) => Ok(value),
            _ => Err(protocol()),
        }
    }

    pub async fn rewind(
        &self,
        session_id: &str,
        target: &str,
        workspace: std::path::PathBuf,
    ) -> Result<LocalRewindResult> {
        match self
            .call(Request::Rewind {
                session_id: session_id.into(),
                target: target.into(),
                workspace,
            })
            .await?
        {
            Response::Rewind(value) => Ok(value),
            _ => Err(protocol()),
        }
    }

    pub async fn open_session(&self, session_id: &str) -> Result<crate::GraphSession> {
        match self
            .call(Request::Open {
                session_id: session_id.into(),
            })
            .await?
        {
            Response::Session(session) => Ok(session),
            _ => Err(protocol()),
        }
    }

    pub async fn create_session(
        &self,
        request: crate::CreateSessionRequest,
    ) -> Result<crate::GraphSession> {
        match self.call(Request::Create { request }).await? {
            Response::Session(session) => Ok(session),
            _ => Err(protocol()),
        }
    }

    pub async fn write_session(
        &self,
        session_id: &str,
        change: SessionWrite,
    ) -> Result<SessionWriteResult> {
        match self
            .call(Request::Write {
                session_id: session_id.into(),
                change,
            })
            .await?
        {
            Response::Write(result) => Ok(result),
            _ => Err(protocol()),
        }
    }

    pub async fn query(
        &self,
        session_id: &str,
        turn_id: Option<TurnId>,
    ) -> Result<SessionObservation> {
        match self
            .call(Request::Query {
                session_id: session_id.into(),
                turn_id,
            })
            .await?
        {
            Response::View(view) => Ok(*view),
            _ => Err(protocol()),
        }
    }

    /// 返回一个固定对话节点及所有父边，不按当前 head 重新选择历史。
    pub async fn read_commit(
        &self,
        session_id: &str,
        commit_id: &str,
    ) -> Result<CommittedTurnObservation> {
        match self
            .call(Request::Commit {
                session_id: session_id.into(),
                commit_id: commit_id.into(),
            })
            .await?
        {
            Response::Commit(turn) => Ok(*turn),
            _ => Err(protocol()),
        }
    }

    /// UTF-8 字节页可跨 JSONL 行，调用者按 next_offset 累接后解码消息。
    pub async fn read_transcript_page(
        &self,
        session_id: &str,
        commit_id: &str,
        offset: usize,
        limit: usize,
    ) -> Result<TranscriptPage> {
        match self
            .call(Request::Transcript {
                session_id: session_id.into(),
                commit_id: commit_id.into(),
                offset,
                limit,
            })
            .await?
        {
            Response::Transcript(page) => Ok(page),
            _ => Err(protocol()),
        }
    }

    /// 找回同一请求或关闭旧 pending，均不要求新配置；只有 NeedsConfiguration 允许加载新配置。
    /// 此操作可能补确认屏障/关闭旧 Turn，不等同于纯 query。
    pub async fn lookup_or_recover(&self, request: TurnRequest) -> Result<Acceptance> {
        self.submit(request, None).await
    }

    pub async fn submit(
        &self,
        request: TurnRequest,
        configuration: Option<FrozenConfiguration>,
    ) -> Result<Acceptance> {
        match self
            .call(Request::Submit {
                request,
                configuration: configuration.map(Box::new),
            })
            .await?
        {
            Response::Acceptance(acceptance) => Ok(acceptance),
            _ => Err(protocol()),
        }
    }

    /// 惰性配置入口：首轮无配置探测直接返回 Existing 或 Busy 时不调用 load。
    /// 收到 NeedsConfiguration 后最多加载一次，后续准入仍可能因并发变化返回这些结果。
    pub async fn submit_with<F, Fut>(&self, request: TurnRequest, load: F) -> Result<Acceptance>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<FrozenConfiguration>>,
    {
        match self.submit_handshake(request, load, None).await? {
            SubmissionReply::Accepted(acceptance) => Ok(acceptance),
            SubmissionReply::NotSubmitted(_) => Err(protocol()),
            SubmissionReply::ConfigurationFailed(error) | SubmissionReply::BackendFailed(error) => {
                Err(error)
            }
        }
    }

    pub(super) async fn submit_handshake<F, Fut>(
        &self,
        request: TurnRequest,
        load: F,
        mut abandon: Option<tokio::sync::watch::Receiver<bool>>,
    ) -> Result<SubmissionReply>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<FrozenConfiguration>>,
    {
        // 保留后台错误回执的来源；低层 submit 会把它和本地传输失败合并为 Err。
        let mut stream = ipc::connect(self.layout.clone()).await?;
        ipc::write(
            &mut stream,
            &Request::Submit {
                request: request.clone(),
                configuration: None,
            },
        )
        .await?;
        let response = ipc::read(&mut stream).await?;
        drop(stream);
        let prior = match response {
            Response::Acceptance(acceptance) => acceptance,
            Response::Error(error) => return Ok(SubmissionReply::BackendFailed(error)),
            _ => return Err(protocol()),
        };
        if prior != Acceptance::NeedsConfiguration {
            return Ok(SubmissionReply::Accepted(prior));
        }
        if abandoned(&abandon) {
            return Ok(SubmissionReply::NotSubmitted(
                NotSubmittedStage::NeedsConfiguration,
            ));
        }
        let configuration = tokio::select! {
            biased;
            _ = async {
                match &mut abandon {
                    Some(receiver) => { while !*receiver.borrow_and_update() { if receiver.changed().await.is_err() { break; } } }
                    None => std::future::pending::<()>().await,
                }
            } => return Ok(SubmissionReply::NotSubmitted(NotSubmittedStage::FreezingConfiguration)),
            result = load() => match result {
                Ok(configuration) => configuration,
                Err(error) => return Ok(SubmissionReply::ConfigurationFailed(error)),
            },
        };
        let mut stream = ipc::connect(self.layout.clone()).await?;
        // 配置可能来自不可中止的阻塞工作；在真正发送关口再次核对放弃。
        if abandoned(&abandon) {
            return Ok(SubmissionReply::NotSubmitted(
                NotSubmittedStage::FreezingConfiguration,
            ));
        }
        ipc::write(
            &mut stream,
            &Request::Submit {
                request,
                configuration: Some(Box::new(configuration)),
            },
        )
        .await?;
        match ipc::read(&mut stream).await? {
            Response::Acceptance(value) if value != Acceptance::NeedsConfiguration => {
                Ok(SubmissionReply::Accepted(value))
            }
            Response::Error(error) => Ok(SubmissionReply::BackendFailed(error)),
            _ => Err(protocol()),
        }
    }

    /// 请求取消指定 Turn，并返回后台在判定点是否接管本次意图。
    ///
    /// `true` 表示后台已实际接管带配置的操作，并独立负责从准备到执行、完成或明确诊断的处理与收尾；
    /// 连接退出不撤销该责任。同身份无配置请求若共享该操作，也可以得到 `true`。
    /// `false` 表示判定点未接管取消；独立无配置探测、确认或会话写操作仍可能正在运行，
    /// 配置也可能仍由提交连接持有。它不证明没有后台工作或后来不会接受同身份请求。
    ///
    /// 操作明确拒绝、确认未建立 Turn 且监督退出后，新准入不继承旧取消；
    /// 未知发布或已有 pending/pin 仍按原事实核验。接管不等于持久 Cancelled，
    /// 迟到取消不改写已决定结果，也不回滚已发生效果；最终状态须另行查询。
    pub async fn cancel(&self, session_id: &str, turn_id: TurnId) -> Result<bool> {
        match self
            .call(Request::Cancel {
                session_id: session_id.into(),
                turn_id,
            })
            .await?
        {
            Response::Cancelled { owned } => Ok(owned),
            _ => Err(protocol()),
        }
    }

    pub async fn subscribe(
        &self,
        session_id: &str,
        turn_id: TurnId,
        cursor: Option<EventCursor>,
    ) -> Result<LocalSubscription> {
        let mut stream = ipc::connect(self.layout.clone()).await?;
        ipc::write(
            &mut stream,
            &Request::Subscribe {
                session_id: session_id.into(),
                turn_id,
                cursor,
            },
        )
        .await?;
        Ok(LocalSubscription::from_stream(
            stream,
            #[cfg(test)]
            self.finished_received.clone(),
            #[cfg(test)]
            self.refix_retirement.clone(),
        ))
    }

    /// 自启当前 CLI 的 daemon serve；该分支在 dotenv/runtime 之前清空继承环境。
    /// 只有端点不存在或拒绝连接时才自启；协议/身份不兼容不杀旧后台、不换版本重试。
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
            .ok_or_else(|| LocalError::new(LocalErrorKind::Internal, "local worker failed"))?
            .to_path_buf();
        tokio::task::spawn_blocking(move || {
            use std::os::unix::process::CommandExt;
            use std::process::{Command, Stdio};
            let mut command = Command::new(std::env::current_exe()?);
            command
                .args(["daemon", "serve", "--state-root"])
                .arg(&state_root)
                .env_clear()
                .env("LANG", "C.UTF-8")
                .env("HOME", state_root.join("lattice/local"))
                .env("XDG_CONFIG_HOME", state_root.join("lattice/local/config"))
                .env("XDG_STATE_HOME", &state_root)
                .current_dir("/")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            // fork 后只做 async-signal-safe 的 setsid；后台不属于前端终端进程组。
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

pub(super) enum SubmissionReply {
    Accepted(Acceptance),
    NotSubmitted(NotSubmittedStage),
    ConfigurationFailed(LocalError),
    BackendFailed(LocalError),
}

fn abandoned(receiver: &Option<tokio::sync::watch::Receiver<bool>>) -> bool {
    receiver.as_ref().is_some_and(|receiver| *receiver.borrow())
}

pub struct LocalSubscription {
    receiver: tokio::sync::mpsc::Receiver<Result<Observation>>,
    reader: tokio::task::JoinHandle<Option<Result<Observation>>>,
}
impl LocalSubscription {
    fn from_stream(
        mut stream: UnixStream,
        #[cfg(test)] finished_received: Option<std::sync::Arc<tokio::sync::Notify>>,
        #[cfg(test)] refix_retirement: Option<
            std::sync::Arc<super::client_tests::RefixRetirementGate>,
        >,
    ) -> Self {
        let (sender, receiver) = tokio::sync::mpsc::channel(8);
        let reader = tokio::spawn(async move {
            loop {
                let response = tokio::select! {
                    biased;
                    _ = sender.closed() => return None,
                    response = ipc::read(&mut stream) => response,
                };
                let observation = match response {
                    Ok(Response::Observation(value)) => Ok(value),
                    Ok(Response::Error(error)) => Err(error),
                    Ok(_) => Err(protocol()),
                    Err(error) => Err(error),
                };
                #[cfg(test)]
                if matches!(&observation, Ok(Observation::Finished { .. }))
                    && let Some(received) = &finished_received
                {
                    received.notify_one();
                }
                let finished = matches!(
                    &observation,
                    Err(_)
                        | Ok(Observation::Finished { .. }
                            | Observation::Snapshot { owned: false, .. })
                );
                #[cfg(test)]
                let queued_snapshot = refix_retirement
                    .as_ref()
                    .filter(|gate| gate.matches(&observation));
                if let Err(unsent) = sender.send(observation).await {
                    return Some(unsent.0);
                }
                #[cfg(test)]
                if let Some(gate) = queued_snapshot {
                    gate.snapshot_queued.notify_one();
                }
                if finished {
                    return None;
                }
            }
        });
        Self { receiver, reader }
    }

    /// 停止读取并收割至多8个排队帧及1个已解码但未入队的帧。
    /// 切换观察前不能 abort 丢失已经收到的 Finished。
    pub(super) async fn close(&mut self) -> Vec<Result<Observation>> {
        self.receiver.close();
        let last = (&mut self.reader)
            .await
            .unwrap_or_else(|_| Some(Err(protocol())));
        let mut received = Vec::new();
        while let Ok(observation) = self.receiver.try_recv() {
            received.push(observation);
        }
        received.extend(last);
        received
    }

    /// 可安全放在 select 中：帧读取由独立有界读取任务持有，取消 recv 不丢半帧。
    pub async fn next(&mut self) -> Result<Option<Observation>> {
        self.receiver.recv().await.transpose()
    }
}

impl Drop for LocalSubscription {
    fn drop(&mut self) {
        // 这里只销毁观察连接；后台执行与取消请求均不属于此任务。
        self.reader.abort();
    }
}

fn protocol() -> LocalError {
    LocalError::new(LocalErrorKind::Protocol, "unexpected local response")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncWriteExt;

    #[tokio::test]
    async fn interrupted_observation_poll_does_not_discard_a_partial_frame() {
        let (mut writer, reader) = UnixStream::pair().unwrap();
        let mut subscription = LocalSubscription::from_stream(reader, None, None);
        writer.write_all(b"LTLO").await.unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(10), subscription.next())
                .await
                .is_err()
        );
        let response = Response::Observation(Observation::Gap {
            cursor: EventCursor {
                epoch: "test".into(),
                sequence: 4,
            },
        });
        let payload = serde_json::to_vec(&response).unwrap();
        writer.write_all(b"CAL2").await.unwrap();
        writer.write_u32(payload.len() as u32).await.unwrap();
        writer.write_all(&payload).await.unwrap();
        assert!(
            matches!(subscription.next().await.unwrap(), Some(Observation::Gap { cursor }) if cursor.sequence == 4)
        );
        drop(writer);
        assert!(subscription.next().await.is_err());
    }
}
