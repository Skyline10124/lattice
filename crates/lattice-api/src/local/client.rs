// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use super::{FrozenConfiguration, ipc, types::*};
use std::time::Duration;
use tokio::net::UnixStream;

/// 具体本地客户端。每个操作使用独立连接；Drop 只断开，不取消后台任务。
#[derive(Clone)]
pub struct LocalClient {
    layout: crate::StateLayout,
}

impl LocalClient {
    pub fn new(layout: crate::StateLayout) -> Self {
        Self { layout }
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
                Some(next) if next > offset => offset = next,
                Some(_) => return Err(protocol()),
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

    /// 惰性配置入口：找回、Busy 和 closure-only 恢复都不会执行 load。
    pub async fn submit_with<F, Fut>(&self, request: TurnRequest, load: F) -> Result<Acceptance>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<FrozenConfiguration>>,
    {
        let prior = self.lookup_or_recover(request.clone()).await?;
        if prior != Acceptance::NeedsConfiguration {
            return Ok(prior);
        }
        self.submit(request, Some(load().await?)).await
    }

    pub async fn recover(&self, session_id: &str) -> Result<SessionObservation> {
        match self
            .call(Request::Recover {
                session_id: session_id.into(),
            })
            .await?
        {
            Response::View(view) => Ok(*view),
            _ => Err(protocol()),
        }
    }

    /// true 表示后台已接管取消标志，不表示 durable Cancelled；false 表示该 Turn 没有 live 所有者。
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
        Ok(LocalSubscription::from_stream(stream))
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

pub struct LocalSubscription {
    receiver: tokio::sync::mpsc::Receiver<Result<Observation>>,
    reader: tokio::task::JoinHandle<()>,
}
impl LocalSubscription {
    fn from_stream(mut stream: UnixStream) -> Self {
        let (sender, receiver) = tokio::sync::mpsc::channel(8);
        let reader = tokio::spawn(async move {
            loop {
                let observation = match ipc::read(&mut stream).await {
                    Ok(Response::Observation(value)) => Ok(value),
                    Ok(Response::Error(error)) => Err(error),
                    Ok(_) => Err(protocol()),
                    Err(error) => Err(error),
                };
                let finished = matches!(
                    &observation,
                    Err(_)
                        | Ok(Observation::Finished { .. }
                            | Observation::Snapshot { owned: false, .. })
                );
                if sender.send(observation).await.is_err() || finished {
                    break;
                }
            }
        });
        Self { receiver, reader }
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
        let mut subscription = LocalSubscription::from_stream(reader);
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
