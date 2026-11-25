// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 外部 crate 视角集成测试：通过 `serve_for_test_with_service` 取得同后台实例的进程内 `Service`，
//! 验证与本地 IPC `LocalClient` 共享同一套会话准入、流式正文观察与状态感知停机通知。

use lattice_local::{
    AgentName, Channel, ConfigureRequest, ConfigureSession, LocalClient, StateLayout, TurnLookup,
    TurnRequest, UserInputPart, new_session_id, new_turn_id,
};
use lattice_server::{
    DrainOutcome, InProcessTurn, InProcessTurnOutcome, LocalWorkspaceRoot, Service,
    StopNotification, serve_for_test_with_service,
};
use lattice_session::store::PublicTurnOutcome;
use std::{os::unix::fs::PermissionsExt, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::oneshot,
};

async fn bounded<T>(future: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(10), future)
        .await
        .expect("集成测试等待超时")
}

#[tokio::test]
async fn serve_for_test_with_service_shares_backend_with_ipc_and_notifies_stop() {
    let dir = tempfile::tempdir().expect("创建临时目录应成功");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("绑定本地 HTTP 端口应成功");
    let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());

    let agent_path = dir.path().join(".lattice/agents/local-test/agent.toml");
    std::fs::create_dir_all(agent_path.parent().unwrap()).unwrap();
    std::fs::write(
        &agent_path,
        "schema_version = 1\n[agent]\nname = 'local-test'\nmodel = 'local/local-test'\n[prompt]\nsystem = 'test'\n",
    )
    .unwrap();
    let lattice_dir = dir.path().join(".lattice");
    std::fs::set_permissions(&lattice_dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::write(
        lattice_dir.join("providers.toml"),
        format!(
            "[providers.local]\nprotocol='chat-completions'\nbase_url='{endpoint}'\nauth='bearer'\n"
        ),
    )
    .unwrap();
    lattice_llm::auth::AuthStore::new(lattice_dir)
        .set("local", "integration-test-secret")
        .unwrap();

    let (finish_stream_tx, finish_stream_rx) = oneshot::channel();
    let provider_task = tokio::spawn(async move {
        let (mut socket, _) = bounded(listener.accept())
            .await
            .expect("假模型应收到请求连接");
        let mut headers = Vec::new();
        while !headers.ends_with(b"\r\n\r\n") {
            headers.push(socket.read_u8().await.unwrap());
        }
        let header_str = String::from_utf8(headers).unwrap();
        let len: usize = header_str
            .lines()
            .find_map(|line| {
                line.to_ascii_lowercase()
                    .strip_prefix("content-length:")
                    .map(|v| v.trim().parse().unwrap())
            })
            .unwrap();
        let mut req_body = vec![0; len];
        socket.read_exact(&mut req_body).await.unwrap();

        let part_draft = "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"隐藏推理\"},\"finish_reason\":null}]}\n\ndata: {\"choices\":[{\"delta\":{\"content\":\"集成测试正文\"},\"finish_reason\":null}]}\n\n";
        let part_finish =
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n";
        let resp_len = part_draft.len() + part_finish.len();
        let header = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {resp_len}\r\nConnection: close\r\n\r\n"
        );
        socket.write_all(header.as_bytes()).await.unwrap();
        socket.write_all(part_draft.as_bytes()).await.unwrap();
        socket.flush().await.unwrap();

        bounded(finish_stream_rx)
            .await
            .expect("应收到观察到草稿后的结束流信号");
        socket.write_all(part_finish.as_bytes()).await.unwrap();
        socket.flush().await.unwrap();
    });

    let layout = StateLayout::in_state_root(dir.path());
    let (stop_tx, stop_rx) = tokio::sync::mpsc::channel(2);
    let (service_tx, service_rx) = oneshot::channel::<Service>();
    let serving_layout = layout.clone();
    let home = Some(dir.path().to_path_buf());
    let serving = tokio::spawn(async move {
        serve_for_test_with_service(
            serving_layout,
            home,
            Duration::from_secs(30),
            stop_rx,
            service_tx,
        )
        .await
    });

    let service = bounded(service_rx).await.expect("应取得进程内 Service");
    let client = LocalClient::new(layout);
    let mut stop_notice: StopNotification = service.stopping();
    assert!(!stop_notice.is_stopping());

    let workspace = LocalWorkspaceRoot::from_path(dir.path())
        .unwrap()
        .canonical_path()
        .to_path_buf();
    let sid = new_session_id();
    service
        .configure(ConfigureRequest {
            session: ConfigureSession::New(sid),
            workspace: workspace.clone(),
            agent: Some(AgentName::parse("local-test").unwrap()),
            model: None,
            effort: None,
        })
        .await
        .expect("进程内创建草稿会话应成功");

    let turn_id = new_turn_id();
    let mut turn: InProcessTurn = service
        .submit(TurnRequest::single(
            turn_id,
            sid,
            workspace,
            Channel::parse("cli").expect("固定通道名 cli 应合法"),
            vec![UserInputPart::Text {
                text: "跨接口核对".into(),
            }],
            None,
            None,
        ))
        .await
        .expect("进程内提交轮次应成功");

    assert_eq!(
        bounded(turn.next_draft()).await.as_deref(),
        Some("集成测试正文"),
        "结束帧发出前应稳定观察到纯文本草稿且不含推理内容"
    );
    finish_stream_tx.send(()).expect("放行模型结束帧应成功");
    assert_eq!(bounded(turn.next_draft()).await, None);

    let outcome: InProcessTurnOutcome = bounded(turn.outcome())
        .await
        .expect("读取轮次终态结果应成功");
    assert_eq!(outcome.detail.summary.outcome, PublicTurnOutcome::Completed);
    assert_eq!(outcome.assistant_text, "集成测试正文");
    assert!(outcome.diagnostic.is_none());

    assert_eq!(
        service.head(sid).await.unwrap(),
        client.session_head(sid).await.unwrap()
    );
    assert_eq!(
        service.turn(turn_id).await.unwrap(),
        TurnLookup::Accepted {
            turn: outcome.detail
        }
    );
    assert_eq!(
        service.usage(sid).await.unwrap(),
        client.usage(&sid.to_string()).await.unwrap()
    );

    stop_tx.send(()).await.expect("发送停机请求应成功");
    bounded(stop_notice.wait()).await;
    assert!(stop_notice.is_stopping());

    let mut late_notice = service.stopping();
    assert!(late_notice.is_stopping());
    bounded(late_notice.wait()).await;

    let exit = bounded(serving)
        .await
        .expect("后台任务不应崩溃")
        .expect("后台收尾不应报错");
    assert_eq!(exit.drain, DrainOutcome::Completed);
    drop(exit.instance);
    bounded(provider_task).await.unwrap();
}
