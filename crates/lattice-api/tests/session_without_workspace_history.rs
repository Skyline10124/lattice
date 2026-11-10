// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 纯对话会话的新建与续写不创建、不依赖 `workspace-history.git`，且保持已有历史目录原样。

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::Duration;

use lattice_api::local::*;
use lattice_api::{LocalWorkspaceRoot, StateLayout};
use lattice_session::store::PublicTurnOutcome;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::oneshot;

struct LocalSseProvider {
    endpoint: String,
    shutdown: oneshot::Sender<()>,
    task: tokio::task::JoinHandle<usize>,
}

impl LocalSseProvider {
    async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
        let (shutdown, mut stopping) = oneshot::channel();
        let task = tokio::spawn(async move {
            let mut served = 0usize;
            loop {
                let (mut socket, _) = tokio::select! {
                    accepted = listener.accept() => accepted.unwrap(),
                    _ = &mut stopping => break,
                };
                let mut headers = Vec::new();
                while !headers.ends_with(b"\r\n\r\n") {
                    headers.push(socket.read_u8().await.unwrap());
                    assert!(headers.len() <= 65_536);
                }
                let headers_str = String::from_utf8(headers).unwrap();
                let length: usize = headers_str
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .map(|n| n.trim().parse().unwrap())
                    })
                    .unwrap();
                let mut req_body = vec![0; length];
                socket.read_exact(&mut req_body).await.unwrap();
                served += 1;
                let sse = format!(
                    "data: {{\"choices\":[{{\"delta\":{{\"content\":\"回复-{served}\"}},\"finish_reason\":null}}]}}\n\ndata: {{\"choices\":[{{\"delta\":{{}},\"finish_reason\":\"stop\"}}]}}\n\ndata: [DONE]\n\n"
                );
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{sse}",
                    sse.len()
                );
                socket.write_all(resp.as_bytes()).await.unwrap();
            }
            served
        });
        Self {
            endpoint,
            shutdown,
            task,
        }
    }

    async fn finish(self) -> usize {
        let _ = self.shutdown.send(());
        self.task.await.unwrap()
    }
}

fn configure_workspace_and_home(home: &Path, workspace: &Path, endpoint: &str) {
    let config_dir = home.join(".lattice");
    std::fs::create_dir_all(&config_dir).unwrap();
    std::fs::set_permissions(&config_dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::write(
        config_dir.join("providers.toml"),
        format!("[providers.local]\nprotocol = 'chat-completions'\nbase_url = '{endpoint}'\nauth = 'none'\n"),
    )
    .unwrap();

    let agent_dir = workspace.join(".lattice/agents/alpha");
    std::fs::create_dir_all(&agent_dir).unwrap();
    std::fs::set_permissions(
        workspace.join(".lattice"),
        std::fs::Permissions::from_mode(0o700),
    )
    .unwrap();
    std::fs::write(
        agent_dir.join("agent.toml"),
        "schema_version = 1\n[agent]\nname = 'alpha'\nmodel = 'local/test-model'\n[prompt]\nsystem = '简洁回答'\n",
    )
    .unwrap();
}

async fn wait_backend_ready(client: &LocalClient) {
    tokio::time::timeout(Duration::from_secs(10), async {
        while client.status().await.is_err() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("等待本地后台就绪超时");
}

async fn wait_turn_closed(client: &LocalClient, turn_id: TurnId) -> TurnDetail {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let TurnLookup::Accepted { turn } = client.turn(turn_id).await.unwrap()
                && turn.summary.progress.is_closed()
            {
                return turn;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("等待轮次关闭超时")
}

#[tokio::test]
async fn new_and_continued_sessions_neither_create_nor_require_workspace_history() {
    let state = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let workspace_dir = tempfile::tempdir().unwrap();
    let provider = LocalSseProvider::start().await;
    configure_workspace_and_home(home.path(), workspace_dir.path(), &provider.endpoint);

    let layout = StateLayout::in_state_root(state.path());
    let workspace = LocalWorkspaceRoot::from_path(workspace_dir.path()).unwrap();
    let projects_dir = layout.projects_dir();

    let serving = tokio::spawn(serve_for_test(layout.clone(), Some(home.path().into())));
    let client = LocalClient::new(layout);
    wait_backend_ready(&client).await;

    let session_id = new_session_id();
    let first_turn = new_turn_id();
    let accepted = client
        .submit(TurnRequest {
            turn_id: first_turn,
            session: SubmitSession::New(session_id),
            workspace: workspace.canonical_path().to_path_buf(),
            input: "第一轮对话".into(),
            agent: Some(AgentName::parse("alpha").unwrap()),
            model: None,
            effort: None,
        })
        .await
        .unwrap();
    assert!(matches!(accepted, Acceptance::Accepted { .. }));
    let first_detail = wait_turn_closed(&client, first_turn).await;
    assert_eq!(first_detail.summary.outcome, PublicTurnOutcome::Completed);
    assert!(!projects_dir.exists(), "新建会话不应创建 projects/ 目录");

    let second_turn = new_turn_id();
    let accepted = client
        .submit(TurnRequest {
            turn_id: second_turn,
            session: SubmitSession::Existing(session_id),
            workspace: workspace.canonical_path().to_path_buf(),
            input: "第二轮续写".into(),
            agent: None,
            model: None,
            effort: None,
        })
        .await
        .unwrap();
    assert!(matches!(accepted, Acceptance::Accepted { .. }));
    let second_detail = wait_turn_closed(&client, second_turn).await;
    assert_eq!(second_detail.summary.outcome, PublicTurnOutcome::Completed);
    assert!(
        !projects_dir.exists(),
        "续写会话不应创建或要求 projects/ 目录"
    );

    client.stop().await.unwrap();
    serving.await.unwrap().unwrap();
    assert_eq!(provider.finish().await, 2);
}

#[tokio::test]
async fn preexisting_workspace_history_directory_is_left_untouched_across_turns() {
    let state = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let workspace_dir = tempfile::tempdir().unwrap();
    let provider = LocalSseProvider::start().await;
    configure_workspace_and_home(home.path(), workspace_dir.path(), &provider.endpoint);

    let layout = StateLayout::in_state_root(state.path());
    let workspace = LocalWorkspaceRoot::from_path(workspace_dir.path()).unwrap();
    let history_dir = layout
        .projects_dir()
        .join("legacy-project")
        .join("workspace-history.git");
    std::fs::create_dir_all(history_dir.join("refs/heads")).unwrap();
    let head_sentinel = history_dir.join("HEAD");
    let extra_sentinel = history_dir.join("refs/heads/legacy-sentinel");
    std::fs::write(&head_sentinel, "not-a-valid-git-head\n").unwrap();
    std::fs::write(&extra_sentinel, "legacy-history-bytes").unwrap();

    let serving = tokio::spawn(serve_for_test(layout.clone(), Some(home.path().into())));
    let client = LocalClient::new(layout);
    wait_backend_ready(&client).await;

    let session_id = new_session_id();
    let first_turn = new_turn_id();
    client
        .submit(TurnRequest {
            turn_id: first_turn,
            session: SubmitSession::New(session_id),
            workspace: workspace.canonical_path().to_path_buf(),
            input: "新建会话".into(),
            agent: Some(AgentName::parse("alpha").unwrap()),
            model: None,
            effort: None,
        })
        .await
        .unwrap();
    let first_detail = wait_turn_closed(&client, first_turn).await;
    assert_eq!(first_detail.summary.outcome, PublicTurnOutcome::Completed);

    let second_turn = new_turn_id();
    client
        .submit(TurnRequest {
            turn_id: second_turn,
            session: SubmitSession::Existing(session_id),
            workspace: workspace.canonical_path().to_path_buf(),
            input: "续写会话".into(),
            agent: None,
            model: None,
            effort: None,
        })
        .await
        .unwrap();
    let second_detail = wait_turn_closed(&client, second_turn).await;
    assert_eq!(second_detail.summary.outcome, PublicTurnOutcome::Completed);

    assert_eq!(
        std::fs::read_to_string(&head_sentinel).unwrap(),
        "not-a-valid-git-head\n",
        "遗留历史目录中的 HEAD 不应被读取校验或改写"
    );
    assert_eq!(
        std::fs::read_to_string(&extra_sentinel).unwrap(),
        "legacy-history-bytes",
        "遗留历史目录中的引用内容应保持原样"
    );
    let entries: Vec<_> = std::fs::read_dir(&history_dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(entries.len(), 2, "遗留历史目录不应新增文件");

    client.stop().await.unwrap();
    serving.await.unwrap().unwrap();
    assert_eq!(provider.finish().await, 2);
}
