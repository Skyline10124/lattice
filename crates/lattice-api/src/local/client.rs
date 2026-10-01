// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use super::{FrozenConfiguration, ipc, types::*};
use std::time::Duration;
use tokio::net::UnixStream;

/// 每个操作一个连接；持续读取任务拥有分帧进度，UI 的 next 可取消。
#[derive(Clone)]
pub struct LocalClient {
    layout: crate::StateLayout,
}
impl LocalClient {
    pub fn new(layout: crate::StateLayout) -> Self {
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
        let mut stream = ipc::connect(self.layout.clone()).await?;
        ipc::write(&mut stream, &request).await?;
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
    pub async fn stop(&self) -> Result<DaemonStatus> {
        match self.call(Request::Stop).await? {
            Response::Status(status) => Ok(status),
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
    pub async fn turn(&self, target: TurnTarget) -> Result<TurnDetail> {
        match self.call(Request::Turn { target }).await? {
            Response::Turn(turn) => Ok(turn),
            _ => Err(protocol()),
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
    pub async fn query(
        &self,
        session_id: Option<SessionId>,
        turn_id: Option<TurnId>,
    ) -> Result<ObservationSnapshot> {
        match self
            .call(Request::Query {
                session_id,
                turn_id,
            })
            .await?
        {
            Response::View(view) => Ok(*view),
            _ => Err(protocol()),
        }
    }
    pub async fn history_page(
        &self,
        anchor: HistoryAnchor,
        cursor: Option<HistoryCursor>,
        max_bytes: u32,
    ) -> Result<HistoryPageView> {
        self.history_page_with_progress(anchor, cursor, max_bytes, || {})
            .await
    }
    async fn history_page_with_progress(
        &self,
        anchor: HistoryAnchor,
        cursor: Option<HistoryCursor>,
        max_bytes: u32,
        progress: impl FnMut(),
    ) -> Result<HistoryPageView> {
        match self
            .response_with_progress(
                Request::History {
                    anchor,
                    cursor,
                    max_bytes,
                },
                progress,
            )
            .await?
        {
            Response::History(page) => Ok(page),
            Response::Error(error) => Err(error),
            _ => Err(protocol()),
        }
    }
    pub(super) async fn read_history_with_progress(
        &self,
        view: &ObservationSnapshot,
        mut progress: impl FnMut(),
    ) -> std::result::Result<LocalHistory, HistoryReadError> {
        let anchor = view.history.clone();
        let mut assembly = HistoryAssembly::new(anchor.clone());
        let mut cursor = None;
        loop {
            let context = HistoryReadContext::Page {
                anchor: anchor.clone(),
                cursor: cursor.clone(),
            };
            let page = self
                .history_page_with_progress(anchor.clone(), cursor.clone(), 65_536, &mut progress)
                .await
                .map_err(|error| HistoryReadError {
                    context: context.clone(),
                    error,
                })?;
            cursor = page.next.clone();
            assembly
                .push(page)
                .map_err(|error| HistoryReadError { context, error })?;
            if cursor.is_none() {
                break;
            }
        }
        assembly.finish().map_err(|error| HistoryReadError {
            context: HistoryReadContext::Worker,
            error,
        })
    }
    pub async fn audit_query(
        &self,
        workspace: std::path::PathBuf,
        selection: AuditSelection,
        after: Option<InvocationId>,
        limit: u32,
    ) -> Result<AuditPage> {
        match self
            .call(Request::AuditQuery {
                workspace,
                selection,
                after,
                limit,
            })
            .await?
        {
            Response::Audit(page) => Ok(page),
            _ => Err(protocol()),
        }
    }
    pub async fn audit_tail(
        &self,
        workspace: std::path::PathBuf,
        selection: AuditSelection,
        after: ChangeSeq,
        limit: u32,
    ) -> Result<AuditTailPage> {
        match self
            .call(Request::AuditTail {
                workspace,
                selection,
                after,
                limit,
            })
            .await?
        {
            Response::AuditTail(page) => Ok(page),
            _ => Err(protocol()),
        }
    }
    pub async fn audit_replay(
        &self,
        workspace: std::path::PathBuf,
        session_id: SessionId,
        turn_id: TurnId,
    ) -> Result<AuditReplay> {
        match self
            .call(Request::AuditReplay {
                workspace,
                session_id,
                turn_id,
            })
            .await?
        {
            Response::AuditReplay(page) => Ok(page),
            _ => Err(protocol()),
        }
    }
    pub async fn operator_status(&self, workspace: std::path::PathBuf) -> Result<OperatorStatus> {
        match self.call(Request::OperatorStatus { workspace }).await? {
            Response::OperatorStatus(status) => Ok(status),
            _ => Err(protocol()),
        }
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
    pub(super) async fn submit_handshake<F, Fut>(
        &self,
        request: TurnRequest,
        load: F,
        mut abandon: Option<tokio::sync::watch::Receiver<bool>>,
    ) -> Result<SubmissionReply>
    where
        F: FnOnce(String) -> Fut,
        Fut: std::future::Future<Output = Result<FrozenConfiguration>>,
    {
        let prior = match self
            .response(Request::Submit {
                request: request.clone(),
                configuration: None,
            })
            .await?
        {
            Response::Acceptance(acceptance) => acceptance,
            Response::Error(error) => return Ok(SubmissionReply::BackendFailed(error)),
            _ => return Err(protocol()),
        };
        let Acceptance::NeedsConfiguration { agent_name } = prior else {
            return Ok(SubmissionReply::Accepted(prior));
        };
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
            result = load(agent_name) => match result {
                Ok(configuration) => configuration,
                Err(error) => return Ok(SubmissionReply::ConfigurationFailed(error)),
            },
        };
        let mut stream = ipc::connect(self.layout.clone()).await?;
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
        match ipc::ResponseReader::default().read(&mut stream).await? {
            Response::Acceptance(value)
                if !matches!(value, Acceptance::NeedsConfiguration { .. }) =>
            {
                Ok(SubmissionReply::Accepted(value))
            }
            Response::Error(error) => Ok(SubmissionReply::BackendFailed(error)),
            _ => Err(protocol()),
        }
    }
    pub async fn cancel(&self, target: TurnTarget) -> Result<bool> {
        match self.call(Request::Cancel { target }).await? {
            Response::Cancelled { owned } => Ok(owned),
            _ => Err(protocol()),
        }
    }
    pub async fn subscribe(
        &self,
        target: TurnTarget,
        cursor: Option<EventCursor>,
    ) -> Result<LocalSubscription> {
        let mut stream = ipc::connect(self.layout.clone()).await?;
        ipc::write(&mut stream, &Request::Subscribe { target, cursor }).await?;
        Ok(LocalSubscription::from_stream(stream))
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
                .env_clear()
                .env("LANG", "C.UTF-8")
                .env("HOME", state_root.join("lattice/local"))
                .env("XDG_CONFIG_HOME", state_root.join("lattice/local/config"))
                .env("XDG_STATE_HOME", &state_root)
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

pub(super) enum SubmissionReply {
    Accepted(Acceptance),
    NotSubmitted(NotSubmittedStage),
    ConfigurationFailed(LocalError),
    BackendFailed(LocalError),
}
fn abandoned(receiver: &Option<tokio::sync::watch::Receiver<bool>>) -> bool {
    receiver.as_ref().is_some_and(|r| *r.borrow())
}

/// 历史读取错误保留具体 anchor/cursor，不折叠成普通连接 EOF。
pub(super) enum SubscriptionItem {
    Observation(Observation),
    HistoryError(HistoryReadError),
}
pub struct LocalSubscription {
    receiver: tokio::sync::mpsc::Receiver<Result<SubscriptionItem>>,
    reader: tokio::task::JoinHandle<Option<Result<SubscriptionItem>>>,
    pub(super) progress: tokio::sync::watch::Receiver<tokio::time::Instant>,
}
impl LocalSubscription {
    fn from_stream(mut stream: UnixStream) -> Self {
        let (sender, receiver) = tokio::sync::mpsc::channel(8);
        let (progress, progress_rx) = tokio::sync::watch::channel(tokio::time::Instant::now());
        let reader = tokio::spawn(async move {
            let mut frames = ipc::ResponseReader::default();
            loop {
                let response = tokio::select! {
                    biased;
                    _ = sender.closed() => return None,
                    response = frames.read_with_progress(&mut stream, || { progress.send_replace(tokio::time::Instant::now()); }) => response,
                };
                let observation = match response {
                    Ok(Response::Observation(value)) => Ok(SubscriptionItem::Observation(value)),
                    Ok(Response::HistoryError(error)) => Ok(SubscriptionItem::HistoryError(error)),
                    Ok(Response::Error(error)) => Err(error),
                    Ok(_) => Err(protocol()),
                    Err(error) => Err(error),
                };
                let finished = matches!(
                    &observation,
                    Err(_)
                        | Ok(SubscriptionItem::HistoryError(_))
                        | Ok(SubscriptionItem::Observation(Observation::Finished { .. }))
                ) || matches!(&observation, Ok(SubscriptionItem::Observation(Observation::Snapshot { view })) if !view.owned && view.selected_turn.as_ref().is_some_and(|t| t.summary.is_none()));
                if let Err(unsent) = sender.send(observation).await {
                    return Some(unsent.0);
                }
                if finished {
                    return None;
                }
            }
        });
        Self {
            receiver,
            reader,
            progress: progress_rx,
        }
    }
    pub(super) async fn close(&mut self) -> Vec<Result<SubscriptionItem>> {
        self.receiver.close();
        let last = (&mut self.reader)
            .await
            .unwrap_or_else(|_| Some(Err(protocol())));
        let mut received = Vec::new();
        while let Ok(item) = self.receiver.try_recv() {
            received.push(item);
        }
        received.extend(last);
        received
    }
    pub(super) async fn next_item(&mut self) -> Result<Option<SubscriptionItem>> {
        self.receiver.recv().await.transpose()
    }
    pub async fn next(&mut self) -> Result<Option<Observation>> {
        match self.next_item().await? {
            Some(SubscriptionItem::Observation(observation)) => Ok(Some(observation)),
            Some(SubscriptionItem::HistoryError(error)) => Err(error.error),
            None => Ok(None),
        }
    }
}
impl Drop for LocalSubscription {
    fn drop(&mut self) {
        self.reader.abort();
    }
}

/// 分片逐字重组且核对父链。服务端与客户端共用协议解码，不重新查询 current。
pub(super) struct HistoryAssembly {
    history: LocalHistory,
    partial: Option<(NodeId, String, String)>,
    next: Option<HistoryCursor>,
}
impl HistoryAssembly {
    pub(super) fn new(anchor: HistoryAnchor) -> Self {
        Self {
            history: LocalHistory {
                anchor,
                entries: Vec::new(),
            },
            partial: None,
            next: None,
        }
    }
    pub(super) fn push(&mut self, page: HistoryPageView) -> Result<()> {
        if page.anchor != self.history.anchor || (page.fragments.is_empty() && page.next.is_some())
        {
            return Err(protocol());
        }
        for part in page.fragments {
            let fragment = part.fragment;
            if let Some(next) = self.next.take()
                && (next.node != fragment.node || next.utf8_offset != fragment.utf8_offset)
            {
                return Err(protocol());
            }
            let (id, agent, text) =
                self.partial
                    .get_or_insert((fragment.node, part.agent_name.clone(), String::new()));
            if *id != fragment.node
                || *agent != part.agent_name
                || text.len() as u64 != fragment.utf8_offset
                || fragment.json_fragment.is_empty()
            {
                return Err(protocol());
            }
            text.push_str(&fragment.json_fragment);
            if fragment.complete {
                let (node, agent_name, text) = self.partial.take().expect("partial");
                let content: Node = serde_json::from_str(&text).map_err(|_| protocol())?;
                let previous = self
                    .history
                    .entries
                    .last()
                    .map_or(NodeId(0), |entry| entry.node);
                if node <= previous
                    || content.parent() != Some(previous)
                    || content.turn_id().is_none()
                {
                    return Err(protocol());
                }
                self.history.entries.push(HistoryEntry {
                    node,
                    agent_name,
                    content,
                });
            }
        }
        if let Some(next) = &page.next {
            if next.anchor != self.history.anchor {
                return Err(protocol());
            }
            if let Some((node, _, text)) = &self.partial
                && (next.node != *node || next.utf8_offset != text.len() as u64)
            {
                return Err(protocol());
            }
        } else if self.partial.is_some() {
            return Err(protocol());
        }
        self.next = page.next;
        Ok(())
    }
    pub(super) fn finish(self) -> Result<LocalHistory> {
        if self.partial.is_some()
            || self.next.is_some()
            || self.history.entries.last().map_or(NodeId(0), |e| e.node) != self.history.anchor.head
        {
            return Err(protocol());
        }
        Ok(self.history)
    }
}
pub(super) fn validate_history(history: &LocalHistory) -> Result<()> {
    let mut previous = NodeId(0);
    for entry in &history.entries {
        if entry.node <= previous
            || entry.content.parent() != Some(previous)
            || entry.content.turn_id().is_none()
        {
            return Err(protocol());
        }
        previous = entry.node;
    }
    if previous != history.anchor.head {
        return Err(protocol());
    }
    Ok(())
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
        let mut subscription = LocalSubscription::from_stream(reader);
        writer.write_all(b"LTLO").await.unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(10), subscription.next())
                .await
                .is_err()
        );
        let response = Response::Observation(Observation::Finished {
            turn: None,
            cursor: EventCursor {
                epoch: "test".into(),
                sequence: 4,
            },
            history: LocalHistory {
                anchor: HistoryAnchor {
                    session_id: SessionId::parse("00000000000000000000000000000001").unwrap(),
                    head: NodeId(0),
                },
                entries: vec![],
            },
        });
        let json = serde_json::to_string(&response).unwrap();
        let payload = serde_json::to_vec(
            &serde_json::json!({"message_id":0,"offset":0,"json_fragment":json,"complete":true}),
        )
        .unwrap();
        writer.write_all(b"CAL3").await.unwrap();
        writer.write_u32(payload.len() as u32).await.unwrap();
        writer.write_all(&payload).await.unwrap();
        assert!(
            matches!(subscription.next().await.unwrap(), Some(Observation::Finished { cursor, .. }) if cursor.sequence == 4)
        );
        assert!(subscription.next().await.unwrap().is_none());
    }
}
