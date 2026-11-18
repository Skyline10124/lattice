// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use super::*;
use lattice_ipc::{RecoveryNeed, TurnRef};
use lattice_local::{LocalClient, new_session_id, new_turn_id};
use lattice_session::store::{NodeId, PublicTurnOutcome};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

pub(super) async fn bounded<T>(future: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(10), future)
        .await
        .expect("局部调度超时")
}

pub(super) struct TestEnv {
    pub(super) dir: tempfile::TempDir,
    pub(super) backend: Arc<Backend>,
    pub(super) service: super::super::service::Service,
    pub(super) client: LocalClient,
    pub(super) stop: tokio::sync::mpsc::Sender<()>,
    pub(super) serving: tokio::task::JoinHandle<Result<ServeExit>>,
}

impl TestEnv {
    pub(super) async fn start() -> Self {
        Self::start_with_drain(Duration::from_secs(60)).await
    }

    pub(super) async fn start_with_drain(drain_timeout: Duration) -> Self {
        let dir = tempfile::tempdir().expect("创建测试临时目录应成功");
        let layout = StateLayout::in_state_root(dir.path());
        let (stop, stop_rx) = tokio::sync::mpsc::channel(4);
        let (backend_tx, backend_rx) = oneshot::channel();
        let (service_tx, service_rx) = oneshot::channel();
        let serving_layout = layout.clone();
        let backend_layout = layout.clone();
        let home = dir.path().to_path_buf();
        let roots = vec![include_bytes!("test_root.der").to_vec()];
        let serving = tokio::spawn(async move {
            serve_with_backend_and_service(
                serving_layout,
                drain_timeout,
                stop_rx,
                Some(service_tx),
                move || {
                    let mut backend = Backend::with_tls_roots(
                        backend_layout,
                        Some(home),
                        roots,
                        Arc::new(crate::turn::NoTools),
                    );
                    Arc::get_mut(&mut backend)
                        .expect("测试后台构造期应独占引用")
                        .metadata = super::super::metadata::MetadataRefresh::without_network();
                    let _ = backend_tx.send(backend.clone());
                    backend
                },
            )
            .await
        });
        let backend = bounded(backend_rx).await.expect("应收到后台实例");
        let service = bounded(service_rx).await.expect("应收到进程内服务实例");
        Self {
            dir,
            backend,
            service,
            client: LocalClient::new(layout),
            stop,
            serving,
        }
    }

    pub(super) fn workspace(&self) -> std::path::PathBuf {
        LocalWorkspaceRoot::from_path(self.dir.path())
            .expect("工作区解析应成功")
            .canonical_path()
            .to_path_buf()
    }

    pub(super) async fn wait_idle(&self) {
        let mut changed = self.backend.changed.subscribe();
        bounded(async {
            while self.backend.has_active_work() {
                changed.changed().await.expect("后台变更通道不应关闭");
            }
        })
        .await;
    }

    pub(super) async fn shutdown(self) {
        self.wait_idle().await;
        let _ = self.stop.send(()).await;
        let exit = bounded(self.serving)
            .await
            .expect("后台任务不应崩溃")
            .expect("后台收尾不应报错");
        assert_eq!(exit.drain, DrainOutcome::Completed, "正常收尾应排空完成");
        assert!(
            exit.failure.is_none(),
            "后台收尾不应记录运行错误: {:?}",
            exit.failure
        );
    }
}

pub(super) fn configure_new_request(
    workspace: &std::path::Path,
    sid: SessionId,
) -> ConfigureRequest {
    ConfigureRequest {
        session: ConfigureSession::New(sid),
        workspace: workspace.to_path_buf(),
        agent: Some(AgentName::parse("local-test").expect("合法 agent 名称")),
        model: None,
        effort: None,
    }
}

pub(super) fn turn_request(
    workspace: &std::path::Path,
    sid: SessionId,
    input: &str,
) -> TurnRequest {
    TurnRequest {
        turn_id: new_turn_id(),
        session: sid,
        workspace: workspace.to_path_buf(),
        parts: vec![UserInputPart::Text { text: input.into() }],
        expected_settings: None,
    }
}

async fn read_http_request(socket: &mut tokio::net::TcpStream) -> serde_json::Value {
    let mut headers = Vec::new();
    while !headers.ends_with(b"\r\n\r\n") {
        headers.push(socket.read_u8().await.expect("读取请求头字节应成功"));
        assert!(headers.len() <= 65_536, "测试请求头超出上限");
    }
    let headers = String::from_utf8(headers).expect("请求头应为合法 UTF-8");
    let length: usize = headers
        .lines()
        .find_map(|line| {
            line.to_ascii_lowercase()
                .strip_prefix("content-length:")
                .map(|n| n.trim().parse().expect("合法 Content-Length"))
        })
        .expect("请求头必须包含 Content-Length");
    let mut body = vec![0; length];
    socket
        .read_exact(&mut body)
        .await
        .expect("读取完整请求体应成功");
    serde_json::from_slice(&body).expect("请求体应为合法 JSON")
}

#[tokio::test]
async fn in_process_lifecycle_streams_text_without_reasoning_ignores_watch_limit_and_matches_ipc() {
    let env = TestEnv::start().await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("绑定本地假模型端口应成功");
    let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
    super::super::tests::configure(env.dir.path(), &endpoint);
    let workspace = env.workspace();

    let (reasoning_sent_tx, reasoning_sent_rx) = oneshot::channel();
    let (send_first_text_tx, send_first_text_rx) = oneshot::channel();
    let (send_rest_tx, send_rest_rx) = oneshot::channel();
    let (send_finish_tx, send_finish_rx) = oneshot::channel();

    let provider_task = tokio::spawn(async move {
        let (mut socket, _) = bounded(listener.accept())
            .await
            .expect("应接受模型 HTTP 连接");
        let _req = bounded(read_http_request(&mut socket)).await;

        let chunk_reasoning = "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"私密推理一\"},\"finish_reason\":null}]}\n\n";
        let chunk_text_1 =
            "data: {\"choices\":[{\"delta\":{\"content\":\"第一段\"},\"finish_reason\":null}]}\n\n";
        let chunk_reasoning_2 = "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"私密推理二\"},\"finish_reason\":null}]}\n\n";
        let chunk_text_2 = "data: {\"choices\":[{\"delta\":{\"content\":\"，第二段\"},\"finish_reason\":null}]}\n\n";
        let chunk_finish = "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":12,\"completion_tokens\":8,\"total_tokens\":20}}\n\ndata: [DONE]\n\n";
        let total_len = chunk_reasoning.len()
            + chunk_text_1.len()
            + chunk_reasoning_2.len()
            + chunk_text_2.len()
            + chunk_finish.len();
        let header = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {total_len}\r\nConnection: close\r\n\r\n"
        );
        socket
            .write_all(header.as_bytes())
            .await
            .expect("写入响应头应成功");
        socket
            .write_all(chunk_reasoning.as_bytes())
            .await
            .expect("写入首段推理应成功");
        socket.flush().await.expect("刷新首段推理应成功");
        let _ = reasoning_sent_tx.send(());

        bounded(send_first_text_rx)
            .await
            .expect("应收到发送首段正文信号");
        socket
            .write_all(chunk_text_1.as_bytes())
            .await
            .expect("写入首段正文应成功");
        socket.flush().await.expect("刷新首段正文应成功");

        bounded(send_rest_rx).await.expect("应收到发送后续正文信号");
        socket
            .write_all(chunk_reasoning_2.as_bytes())
            .await
            .expect("写入第二段推理应成功");
        socket
            .write_all(chunk_text_2.as_bytes())
            .await
            .expect("写入第二段正文应成功");
        socket.flush().await.expect("刷新第二段正文应成功");

        bounded(send_finish_rx).await.expect("应收到发送结束帧信号");
        socket
            .write_all(chunk_finish.as_bytes())
            .await
            .expect("写入结束帧应成功");
        socket.flush().await.expect("刷新结束帧应成功");
    });

    let sid = new_session_id();
    let configured = env
        .service
        .configure(configure_new_request(&workspace, sid))
        .await
        .expect("进程内 configure(New) 应成功");
    assert_eq!(configured.head.session_id, sid);
    assert_eq!(configured.head.position, NodeId(0));

    // 占满全部 32 个外部 IPC watch 许可，验证进程内观察不消耗外部观察名额。
    let mut watches = Vec::new();
    for _ in 0..32 {
        let mut watch = env.client.watch(sid).await.expect("建立 IPC watch 应成功");
        let _ = bounded(watch.next()).await.expect("首帧会话头应到达");
        watches.push(watch);
    }
    let mut extra_watch = env
        .client
        .watch(sid)
        .await
        .expect("第 33 个 IPC watch 连接握手应成功");
    let busy_watch = bounded(extra_watch.next())
        .await
        .expect_err("第 33 个 IPC watch 首帧应因名额耗尽返回 Busy");
    assert_eq!(
        busy_watch.kind,
        LocalErrorKind::Busy,
        "外部 IPC watch 许可耗尽时应返回 Busy"
    );

    let req = turn_request(&workspace, sid, "你好");
    let turn_id = req.turn_id;
    let mut turn = env
        .service
        .submit(req)
        .await
        .expect("外部 IPC watch 满额时进程内 submit 仍应成功");
    assert_eq!(turn.session_id(), sid);
    assert_eq!(turn.turn_id(), turn_id);
    assert!(!turn.is_finished(), "模型未完成前轮次不应标记为已结束");

    bounded(reasoning_sent_rx)
        .await
        .expect("假模型应已发出推理增量");
    tokio::task::yield_now().await;
    assert_eq!(
        turn.draft_text(),
        "",
        "仅有推理增量时草稿快照必须保持空字符串，不得混入推理文本"
    );

    send_first_text_tx.send(()).unwrap();
    assert_eq!(
        bounded(turn.next_draft()).await.as_deref(),
        Some("第一段"),
        "首个正文增量到达后应只输出纯文本草稿"
    );
    assert_eq!(turn.draft_text(), "第一段");

    send_rest_tx.send(()).unwrap();
    assert_eq!(
        bounded(turn.next_draft()).await.as_deref(),
        Some("第一段，第二段"),
        "后续增量到达后应累积助手纯文本并剔除中段推理"
    );
    send_finish_tx.send(()).unwrap();
    assert_eq!(
        bounded(turn.next_draft()).await,
        None,
        "轮次收尾后 next_draft 应返回 None"
    );
    assert!(turn.is_finished(), "收尾后轮次句柄应报告已结束");
    assert_eq!(turn.draft_text(), "", "轮次终态后草稿快照应为空");

    let outcome = bounded(turn.outcome())
        .await
        .expect("读取终态结果快照应成功");
    assert_eq!(
        outcome.detail.summary.outcome,
        PublicTurnOutcome::Completed,
        "轮次终态应为 Completed"
    );
    assert_eq!(
        outcome.assistant_text, "第一段，第二段",
        "终态助手正文应精确等于最后一步纯文本，不含推理内容"
    );
    assert!(outcome.diagnostic.is_none(), "正常完成的轮次不应附带诊断");
    assert_eq!(
        bounded(turn.outcome())
            .await
            .expect("重复读取终态结果应幂等"),
        outcome,
        "重复调用 outcome 应返回同一结果"
    );

    // 验证 Service 的只读查询与 IPC 客户端查询结果完全一致。
    let ipc_head = env
        .client
        .session_head(sid)
        .await
        .expect("IPC 查询会话头应成功");
    let svc_head = env.service.head(sid).await.expect("进程内查询会话头应成功");
    assert_eq!(svc_head, ipc_head, "进程内与 IPC 的 head 查询应一致");

    let ipc_turn = env.client.turn(turn_id).await.expect("IPC 查询轮次应成功");
    let svc_turn = env
        .service
        .turn(turn_id)
        .await
        .expect("进程内查询轮次应成功");
    assert_eq!(svc_turn, ipc_turn, "进程内与 IPC 的 turn 查询应一致");
    assert_eq!(
        svc_turn,
        TurnLookup::Accepted {
            turn: outcome.detail.clone()
        },
        "turn 查询返回的详情应与终态句柄一致"
    );

    let ipc_usage = env
        .client
        .usage(&sid.to_string())
        .await
        .expect("IPC 查询用量应成功");
    let svc_usage = env.service.usage(sid).await.expect("进程内查询用量应成功");
    assert_eq!(svc_usage, ipc_usage, "进程内与 IPC 的 usage 查询应一致");

    drop(watches);
    drop(turn);
    bounded(provider_task).await.expect("假模型任务应正常退出");
    env.shutdown().await;
}

#[tokio::test]
async fn retry_clears_partial_draft_and_keeps_only_second_attempt_text() {
    let env = TestEnv::start().await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("绑定本地假模型端口应成功");
    let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
    super::super::tests::configure(env.dir.path(), &endpoint);
    let agent_path = env.dir.path().join(".lattice/agents/local-test/agent.toml");
    std::fs::write(
        &agent_path,
        "schema_version = 1\n[agent]\nname = 'local-test'\nmodel = 'local/local-test'\n[prompt]\nsystem = 'test'\n[llm.retry]\nmax_retries = 2\ninitial_delay_ms = 10\nmax_delay_ms = 20\n",
    )
    .expect("写入快速重试配置应成功");
    let workspace = env.workspace();

    let (drop_first_tx, drop_first_rx) = oneshot::channel();
    let (second_arrived_tx, second_arrived_rx) = oneshot::channel();
    let (release_second_tx, release_second_rx) = oneshot::channel();
    let (finish_second_tx, finish_second_rx) = oneshot::channel();

    let provider_task = tokio::spawn(async move {
        // 第一次尝试：发送推理与半截正文，随后中断响应体触发可重试错误。
        let (mut first_socket, _) = bounded(listener.accept())
            .await
            .expect("应接受第一次模型连接");
        let _ = bounded(read_http_request(&mut first_socket)).await;
        let partial = "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"失败尝试推理\"},\"finish_reason\":null}]}\n\ndata: {\"choices\":[{\"delta\":{\"content\":\"半截残留文字\"},\"finish_reason\":null}]}\n\n";
        let header = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: 999999\r\nConnection: close\r\n\r\n";
        first_socket
            .write_all(header.as_bytes())
            .await
            .expect("写入第一次响应头应成功");
        first_socket
            .write_all(partial.as_bytes())
            .await
            .expect("写入半截增量应成功");
        first_socket.flush().await.expect("刷新半截增量应成功");
        bounded(drop_first_rx)
            .await
            .expect("应收到断开第一次连接信号");
        drop(first_socket);

        // 第二次尝试：先通知测试核对草稿已清空，再返回完整回复。
        let (mut second_socket, _) = bounded(listener.accept())
            .await
            .expect("应接受第二次模型连接");
        let _ = bounded(read_http_request(&mut second_socket)).await;
        let _ = second_arrived_tx.send(());
        bounded(release_second_rx)
            .await
            .expect("应收到放行第二次响应信号");
        let draft_part = "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"成功尝试推理\"},\"finish_reason\":null}]}\n\ndata: {\"choices\":[{\"delta\":{\"content\":\"第二次正式回复\"},\"finish_reason\":null}]}\n\n";
        let finish_part =
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n";
        let len = draft_part.len() + finish_part.len();
        let header = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {len}\r\nConnection: close\r\n\r\n"
        );
        second_socket
            .write_all(header.as_bytes())
            .await
            .expect("写入第二次响应头应成功");
        second_socket
            .write_all(draft_part.as_bytes())
            .await
            .expect("写入第二次正文增量应成功");
        second_socket
            .flush()
            .await
            .expect("刷新第二次正文增量应成功");

        bounded(finish_second_rx)
            .await
            .expect("应收到结束第二次响应信号");
        second_socket
            .write_all(finish_part.as_bytes())
            .await
            .expect("写入第二次结束帧应成功");
        second_socket.flush().await.expect("刷新第二次结束帧应成功");
    });

    let sid = new_session_id();
    env.service
        .configure(configure_new_request(&workspace, sid))
        .await
        .expect("创建会话应成功");

    let mut turn = env
        .service
        .submit(turn_request(&workspace, sid, "测试重试"))
        .await
        .expect("提交轮次应成功");

    assert_eq!(
        bounded(turn.next_draft()).await.as_deref(),
        Some("半截残留文字"),
        "第一次尝试流式阶段应观察到半截正文"
    );
    assert_eq!(turn.draft_text(), "半截残留文字");

    drop_first_tx.send(()).unwrap();
    assert_eq!(
        bounded(turn.next_draft()).await.as_deref(),
        Some(""),
        "触发 LlmRetry 后必须立即清空草稿正文"
    );
    bounded(second_arrived_rx)
        .await
        .expect("第二次模型请求应到达");
    assert_eq!(
        turn.draft_text(),
        "",
        "第二次尝试尚未吐字时草稿快照应保持为空"
    );

    release_second_tx.send(()).unwrap();
    assert_eq!(
        bounded(turn.next_draft()).await.as_deref(),
        Some("第二次正式回复"),
        "第二次尝试的正文不能拼接第一次尝试的残留文字"
    );
    finish_second_tx.send(()).unwrap();
    assert_eq!(bounded(turn.next_draft()).await, None);

    let outcome = bounded(turn.outcome()).await.expect("终态读取应成功");
    assert_eq!(outcome.detail.summary.outcome, PublicTurnOutcome::Completed);
    assert_eq!(outcome.detail.steps.len(), 1);
    assert_eq!(
        outcome.detail.steps[0].attempts.len(),
        2,
        "持久步骤应记录两次模型尝试"
    );
    assert_eq!(
        outcome.assistant_text, "第二次正式回复",
        "最终助手正文只应包含第二次尝试内容"
    );

    bounded(provider_task).await.expect("假模型任务应正常退出");
    env.shutdown().await;
}

#[tokio::test]
async fn dropping_in_process_turn_does_not_cancel_execution() {
    let env = TestEnv::start().await;
    let mut provider = tests::Provider::start().await;
    provider.release.send_replace(false);
    super::super::tests::configure(env.dir.path(), &provider.endpoint);
    let workspace = env.workspace();

    let sid = new_session_id();
    env.service
        .configure(configure_new_request(&workspace, sid))
        .await
        .expect("创建会话应成功");

    let req = turn_request(&workspace, sid, "丢弃句柄仍应完成");
    let turn_id = req.turn_id;
    let turn = env
        .service
        .submit(req)
        .await
        .expect("提交轮次应返回观察句柄");
    bounded(&mut provider.arrived)
        .await
        .expect("模型请求应已到达");
    assert!(!turn.is_finished(), "闸门放行前轮次仍在运行");

    // 立即丢弃观察句柄，随后放行模型闸门。
    drop(turn);
    provider.release.send_replace(true);
    env.wait_idle().await;

    let TurnLookup::Accepted { turn: detail } = env
        .service
        .turn(turn_id)
        .await
        .expect("丢弃句柄后按轮次身份查询应成功")
    else {
        panic!("已接受轮次必须保持 Accepted");
    };
    assert_eq!(
        detail.summary.outcome,
        PublicTurnOutcome::Completed,
        "丢弃 InProcessTurn 不得取消后台正在执行的轮次"
    );

    let mut reattached = env
        .service
        .observe(sid, turn_id)
        .await
        .expect("结束后重新附着观察应成功");
    let outcome = bounded(reattached.outcome()).await.expect("读取结局应成功");
    assert_eq!(outcome.detail, detail);
    assert_eq!(outcome.assistant_text, "完成");

    assert_eq!(provider.finish().await.len(), 1);
    env.shutdown().await;
}

#[tokio::test]
async fn service_cancel_stops_owned_turn_returns_false_when_unowned_and_surfaces_store_errors() {
    let env = TestEnv::start().await;
    let mut provider = tests::Provider::start().await;
    provider.release.send_replace(false);
    super::super::tests::configure(env.dir.path(), &provider.endpoint);
    let workspace = env.workspace();

    let sid = new_session_id();
    env.service
        .configure(configure_new_request(&workspace, sid))
        .await
        .expect("创建会话应成功");

    let unknown_turn = new_turn_id();
    assert!(
        !env.service
            .cancel(unknown_turn)
            .await
            .expect("未接管且不存在的轮次按取消合同应返回 Ok(false)"),
        "不存在的轮次未由本进程接管时 cancel 应与 IPC 一致返回 false"
    );
    assert_eq!(
        env.service
            .turn(unknown_turn)
            .await
            .expect_err("查询不存在的轮次应返回 NotFound")
            .kind,
        LocalErrorKind::NotFound
    );
    assert_eq!(
        env.service
            .observe(sid, unknown_turn)
            .await
            .expect_err("附着不存在的轮次应返回 NotFound")
            .kind,
        LocalErrorKind::NotFound
    );

    let req = turn_request(&workspace, sid, "将被取消的轮次");
    let turn_id = req.turn_id;
    let mut turn = env.service.submit(req).await.expect("提交轮次应成功");
    bounded(&mut provider.arrived)
        .await
        .expect("模型请求应到达");

    assert!(
        env.service
            .cancel(turn_id)
            .await
            .expect("运行中调用 cancel 应成功"),
        "本进程正在执行的轮次调用 cancel 应返回 true"
    );
    assert!(
        env.service
            .cancel(turn_id)
            .await
            .expect("重复调用 cancel 应幂等"),
        "Owner 退休前重复调用 cancel 仍应幂等返回 true"
    );

    // 保持模型响应闸门未放行，验证取消会主动中断在途模型请求并以 Cancelled 收尾。
    let outcome = bounded(turn.outcome())
        .await
        .expect("被取消轮次应能读取终态结果");
    assert_eq!(
        outcome.detail.summary.outcome,
        PublicTurnOutcome::Cancelled,
        "显式取消后的轮次终态必须为 Cancelled"
    );
    assert_eq!(outcome.assistant_text, "");
    provider.release.send_replace(true);

    assert!(
        !env.service
            .cancel(turn_id)
            .await
            .expect("轮次结束后调用 cancel 应成功返回 false"),
        "轮次已收尾退场后再次调用 cancel 必须返回 false"
    );

    // 验证未接管分支不会吞掉持久库错误。
    env.backend.stores.disable_writes().await;
    let store_err = env
        .service
        .cancel(new_turn_id())
        .await
        .expect_err("会话库停写后未接管 cancel 必须透传会话库错误");
    assert_eq!(
        store_err.kind,
        LocalErrorKind::StoreUnavailable,
        "会话库停写时 cancel 不得将错误吞成 Ok(false)"
    );

    let _ = provider.finish().await;
    env.shutdown().await;
}

#[tokio::test]
async fn observe_reattaches_live_and_finished_turns_and_isolates_turn_id_across_later_turns_and_rewind()
 {
    let env = TestEnv::start().await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("绑定本地假模型端口应成功");
    let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
    super::super::tests::configure(env.dir.path(), &endpoint);
    let workspace = env.workspace();

    let (first_half_sent_tx, first_half_sent_rx) = oneshot::channel();
    let (send_second_half_tx, send_second_half_rx) = oneshot::channel();
    let (finish_first_tx, finish_first_rx) = oneshot::channel();

    let provider_task = tokio::spawn(async move {
        // 第 1 轮：先发“首轮前半”，等待重附着后再发“首轮后半”。
        let (mut sock1, _) = bounded(listener.accept())
            .await
            .expect("应接受第 1 轮模型连接");
        let _ = bounded(read_http_request(&mut sock1)).await;
        let part_a = "data: {\"choices\":[{\"delta\":{\"content\":\"首轮前半\"},\"finish_reason\":null}]}\n\n";
        let part_b = "data: {\"choices\":[{\"delta\":{\"content\":\"＋首轮后半\"},\"finish_reason\":null}]}\n\n";
        let part_done =
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n";
        let len = part_a.len() + part_b.len() + part_done.len();
        let header = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {len}\r\nConnection: close\r\n\r\n"
        );
        sock1.write_all(header.as_bytes()).await.unwrap();
        sock1.write_all(part_a.as_bytes()).await.unwrap();
        sock1.flush().await.unwrap();
        let _ = first_half_sent_tx.send(());

        bounded(send_second_half_rx)
            .await
            .expect("应收到发送第 1 轮后半段信号");
        sock1.write_all(part_b.as_bytes()).await.unwrap();
        sock1.flush().await.unwrap();

        bounded(finish_first_rx)
            .await
            .expect("应收到完成第 1 轮信号");
        sock1.write_all(part_done.as_bytes()).await.unwrap();
        sock1.flush().await.unwrap();

        // 连续服务第 2 至第 12 轮（共 11 轮），使第 1 轮移出最近 10 轮窗口。
        for round in 2..=12 {
            let (mut sock, _) = bounded(listener.accept())
                .await
                .expect("应接受后续轮次模型连接");
            let _ = bounded(read_http_request(&mut sock)).await;
            let body = format!(
                "data: {{\"choices\":[{{\"delta\":{{\"content\":\"第{round}轮正文\"}},\"finish_reason\":null}}]}}\n\ndata: {{\"choices\":[{{\"delta\":{{}},\"finish_reason\":\"stop\"}}]}}\n\ndata: [DONE]\n\n"
            );
            let len = body.len();
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {len}\r\nConnection: close\r\n\r\n{body}"
            );
            sock.write_all(resp.as_bytes()).await.unwrap();
        }
    });

    let sid = new_session_id();
    let other_sid = new_session_id();
    env.service
        .configure(configure_new_request(&workspace, sid))
        .await
        .expect("创建主会话应成功");
    env.service
        .configure(configure_new_request(&workspace, other_sid))
        .await
        .expect("创建隔离会话应成功");

    let req1 = turn_request(&workspace, sid, "第 1 轮输入");
    let turn1_id = req1.turn_id;
    let mut turn1_unconsumed = env.service.submit(req1).await.expect("提交第 1 轮应成功");

    bounded(first_half_sent_rx)
        .await
        .expect("第 1 轮前半段应已发出");

    // 跨会话附着必须拒绝为 NotFound。
    assert_eq!(
        env.service
            .observe(other_sid, turn1_id)
            .await
            .expect_err("跨会话 observe 必须返回 NotFound")
            .kind,
        LocalErrorKind::NotFound
    );

    // 在途重附着应立即读到当前半截草稿，并继续收到后半段与终态结局。
    let mut attached_live = env
        .service
        .observe(sid, turn1_id)
        .await
        .expect("在途轮次 observe 应成功");
    assert!(!attached_live.is_finished());
    assert_eq!(
        bounded(attached_live.next_draft()).await.as_deref(),
        Some("首轮前半"),
        "在途 observe 应读到已累积的前半段草稿"
    );

    send_second_half_tx.send(()).unwrap();
    assert_eq!(
        bounded(attached_live.next_draft()).await.as_deref(),
        Some("首轮前半＋首轮后半"),
        "在途 observe 应继续收到后半段草稿"
    );
    finish_first_tx.send(()).unwrap();
    assert_eq!(bounded(attached_live.next_draft()).await, None);
    let live_outcome = bounded(attached_live.outcome())
        .await
        .expect("在途 observe 读取终态应成功");
    assert_eq!(live_outcome.detail.turn_id, turn1_id);
    assert_eq!(live_outcome.assistant_text, "首轮前半＋首轮后半");

    // 在同一会话连续跑完第 2..=12 轮。
    for round in 2..=12 {
        let mut later = env
            .service
            .submit(turn_request(&workspace, sid, &format!("第 {round} 轮")))
            .await
            .expect("提交后续轮次应成功");
        let later_outcome = bounded(later.outcome())
            .await
            .expect("后续轮次终态读取应成功");
        assert_eq!(later_outcome.assistant_text, format!("第{round}轮正文"));
    }

    // 执行 rewind 退回到根节点，改变会话当前位置。
    env.client
        .rewind(sid, NodeId(0), workspace.clone())
        .await
        .expect("退回到根节点应成功");

    // 验证尚未进入 Accepted 的准入中轮次调用 observe 返回 NotFound。
    let sink = env.backend.observer(sid);
    let (paused_before_accept, release_before_accept) = tests::arm(&sink.before_accept);
    let inflight_req = turn_request(&workspace, sid, "准入前拦截");
    let inflight_id = inflight_req.turn_id;
    let service_for_inflight = env.service.clone();
    let inflight_submit =
        tokio::spawn(async move { service_for_inflight.submit(inflight_req).await });
    bounded(paused_before_accept)
        .await
        .expect("应停在接受事务之前");
    assert_eq!(
        env.service
            .turn(inflight_id)
            .await
            .expect("查询准入中轮次应成功"),
        TurnLookup::InFlight
    );
    assert_eq!(
        env.service
            .observe(sid, inflight_id)
            .await
            .expect_err("尚未落库接受的轮次调用 observe 应返回 NotFound")
            .kind,
        LocalErrorKind::NotFound
    );
    assert!(
        env.service
            .cancel(inflight_id)
            .await
            .expect("取消准入中轮次应成功")
    );
    release_before_accept.send(()).unwrap();
    let mut cancelled_inflight = bounded(inflight_submit)
        .await
        .unwrap()
        .expect("接受事务应完成");
    assert_eq!(
        bounded(cancelled_inflight.outcome())
            .await
            .expect("读取准入期取消轮次终态应成功")
            .detail
            .summary
            .outcome,
        PublicTurnOutcome::Cancelled
    );
    drop(sink);

    // 验证第 1 轮早已持有的句柄，以及此刻新调用的 observe，仍精确返回第 1 轮自己的详情与正文。
    assert_eq!(
        bounded(turn1_unconsumed.next_draft()).await,
        None,
        "已结束的第 1 轮句柄不应读到后续轮次的草稿"
    );
    let kept_outcome = bounded(turn1_unconsumed.outcome())
        .await
        .expect("第 1 轮旧句柄读取终态应成功");
    assert_eq!(kept_outcome, live_outcome, "旧句柄读取结果不得串入后续轮次");

    let mut reattached_after_rewind = env
        .service
        .observe(sid, turn1_id)
        .await
        .expect("跨多轮与 rewind 后重新 observe 第 1 轮应成功");
    assert!(reattached_after_rewind.is_finished());
    assert_eq!(bounded(reattached_after_rewind.next_draft()).await, None);
    let reattached_outcome = bounded(reattached_after_rewind.outcome())
        .await
        .expect("重附着读取第 1 轮终态应成功");
    assert_eq!(
        reattached_outcome, live_outcome,
        "重附着第 1 轮必须精确返回第 1 轮的 TurnDetail 与助手正文"
    );

    bounded(provider_task).await.expect("假模型任务应正常退出");
    env.shutdown().await;
}

#[tokio::test]
async fn drain_timeout_disabling_writes_returns_store_unavailable_on_in_process_outcome() {
    let env = TestEnv::start_with_drain(Duration::from_millis(200)).await;
    let mut provider = tests::Provider::start().await;
    provider.release.send_replace(false);
    super::super::tests::configure(env.dir.path(), &provider.endpoint);
    let workspace = env.workspace();

    let sid = new_session_id();
    env.service
        .configure(configure_new_request(&workspace, sid))
        .await
        .expect("创建会话应成功");
    env.wait_idle().await;

    let (_retiring, release_retire) = tests::arm(&env.backend.gates.retire);
    let mut turn = env
        .service
        .submit(turn_request(&workspace, sid, "排空超时轮次"))
        .await
        .expect("提交轮次应成功");
    bounded(&mut provider.arrived)
        .await
        .expect("模型请求应到达");

    let mut env = env;
    env.stop.send(()).await.expect("发送停机请求应成功");
    let exit = bounded(&mut env.serving)
        .await
        .expect("后台任务不应崩溃")
        .expect("排空超时停机不应报错");
    assert_eq!(exit.drain, DrainOutcome::TimedOut);

    // Owner 尚未退休，但会话库已停写：next_draft 应结束等待返回 None，outcome 应返回 StoreUnavailable。
    assert_eq!(
        bounded(turn.next_draft()).await,
        None,
        "会话库停写后 next_draft 不得挂起"
    );
    let err = bounded(turn.outcome())
        .await
        .expect_err("排空超时停写后 outcome 不得伪造结局");
    assert_eq!(
        err.kind,
        LocalErrorKind::StoreUnavailable,
        "停写后的 outcome 应返回 StoreUnavailable"
    );

    provider.release.send_replace(true);
    let _ = provider.finish().await;
    release_retire.send(()).expect("放行退休闸门应成功");
    env.wait_idle().await;
    drop(exit.instance);
}

#[tokio::test]
async fn confirmation_unknown_maps_to_recovery_error_in_process_while_ipc_keeps_wire_variant() {
    let env = TestEnv::start().await;
    let provider = tests::Provider::start().await;
    super::super::tests::configure(env.dir.path(), &provider.endpoint);
    let workspace = env.workspace();

    let sid = new_session_id();
    env.service
        .configure(configure_new_request(&workspace, sid))
        .await
        .expect("创建会话应成功");
    let sink = env.backend.observer(sid);

    // 1. 进程内 submit：注入 accepted_read_failure -> StoreUnavailable + CommitFailed 找回需求。
    let (accepted_gate, fail_read) = tests::arm(&sink.accepted_read_failure);
    let req_commit = turn_request(&workspace, sid, "进程内读回失败");
    let commit_turn_id = req_commit.turn_id;
    let svc_for_commit = env.service.clone();
    let submitting_commit = tokio::spawn(async move { svc_for_commit.submit(req_commit).await });
    bounded(accepted_gate)
        .await
        .expect("接受事务后应命中读回故障闸门");
    fail_read.send(()).unwrap();

    let commit_err = bounded(submitting_commit)
        .await
        .unwrap()
        .expect_err("接受后读回失败在进程内应返回带找回需求的错误");
    assert_eq!(commit_err.kind, LocalErrorKind::StoreUnavailable);
    assert_eq!(
        commit_err.recovery,
        Some(RecoveryNeed {
            turn: Some(TurnRef {
                session_id: sid,
                turn_id: commit_turn_id,
            }),
            cause: RecoveryCause::CommitFailed,
        }),
        "读回未确认时必须携带精确的 CommitFailed 找回身份"
    );
    env.wait_idle().await;

    let TurnLookup::Accepted {
        turn: recovered_commit,
    } = env
        .service
        .turn(commit_turn_id)
        .await
        .expect("按轮次身份找回应成功")
    else {
        panic!("已落库轮次找回应返回 Accepted");
    };
    assert_eq!(recovered_commit.turn_id, commit_turn_id);

    let mut observed_commit = env
        .service
        .observe(sid, commit_turn_id)
        .await
        .expect("找回后 observe 该轮应成功");
    let commit_outcome = bounded(observed_commit.outcome())
        .await
        .expect("读取故障轮次终态应成功");
    assert!(
        commit_outcome.diagnostic.is_some(),
        "执行期发生存储故障的轮次终态应携带内存诊断"
    );

    // 2. 进程内 submit：注入 forward_panic -> Internal + WorkerFailed 找回需求。
    let (panic_gate, trigger_panic) = tests::arm(&env.backend.gates.forward_panic);
    let req_worker = turn_request(&workspace, sid, "进程内转交异常");
    let worker_turn_id = req_worker.turn_id;
    let svc_for_worker = env.service.clone();
    let submitting_worker = tokio::spawn(async move { svc_for_worker.submit(req_worker).await });
    bounded(panic_gate).await.expect("应命中回执转交故障闸门");
    trigger_panic.send(()).unwrap();

    let worker_err = bounded(submitting_worker)
        .await
        .unwrap()
        .expect_err("转交异常在进程内应返回带 WorkerFailed 找回需求的错误");
    assert_eq!(worker_err.kind, LocalErrorKind::Internal);
    assert_eq!(
        worker_err.recovery,
        Some(RecoveryNeed {
            turn: Some(TurnRef {
                session_id: sid,
                turn_id: worker_turn_id,
            }),
            cause: RecoveryCause::WorkerFailed,
        }),
        "通道/转交异常时必须携带精确的 WorkerFailed 找回身份"
    );
    env.wait_idle().await;
    assert!(
        matches!(
            env.service
                .turn(worker_turn_id)
                .await
                .expect("WorkerFailed 后按轮次身份找回应成功"),
            TurnLookup::Accepted { .. }
        ),
        "独立 completion 收尾后按轮次身份查询应返回 Accepted"
    );

    // 2b. 进程内 submit：执行核心在接受事务提交后、回执发出前 panic -> Internal + WorkerFailed 找回需求。
    let (exec_panic_gate, trigger_exec_panic) = tests::arm(&sink.accepted_read_failure);
    let req_exec_panic = turn_request(&workspace, sid, "执行核心接受后异常");
    let exec_panic_turn_id = req_exec_panic.turn_id;
    let svc_for_exec_panic = env.service.clone();
    let submitting_exec_panic =
        tokio::spawn(async move { svc_for_exec_panic.submit(req_exec_panic).await });
    bounded(exec_panic_gate)
        .await
        .expect("应命中接受后执行核心闸门");
    // 丢弃发送端使执行核心在接受事务落库后触发 panic，验证 TurnAcceptance::ConfirmationUnknown 保留 WorkerFailed。
    drop(trigger_exec_panic);

    let exec_panic_err = bounded(submitting_exec_panic)
        .await
        .unwrap()
        .expect_err("执行核心接受后异常应返回带 WorkerFailed 找回需求的错误");
    assert_eq!(exec_panic_err.kind, LocalErrorKind::Internal);
    assert_eq!(
        exec_panic_err.recovery,
        Some(RecoveryNeed {
            turn: Some(TurnRef {
                session_id: sid,
                turn_id: exec_panic_turn_id,
            }),
            cause: RecoveryCause::WorkerFailed,
        }),
        "执行核心在接受事务后异常中断时不得误报为 CommitFailed"
    );
    env.wait_idle().await;
    assert!(
        matches!(
            env.service
                .turn(exec_panic_turn_id)
                .await
                .expect("执行核心异常后按轮次身份找回应成功"),
            TurnLookup::Accepted { .. }
        ),
        "接受事务已提交的轮次在执行核心异常后找回应返回 Accepted"
    );

    // 3. IPC Submit：同一故障仍原样返回线上 Acceptance::ConfirmationUnknown。
    let (ipc_gate, fail_ipc_read) = tests::arm(&sink.accepted_read_failure);
    let req_ipc = turn_request(&workspace, sid, "IPC 读回失败");
    let ipc_turn_id = req_ipc.turn_id;
    let client_for_ipc = env.client.clone();
    let submitting_ipc = tokio::spawn(async move { client_for_ipc.submit(req_ipc).await });
    bounded(ipc_gate).await.expect("IPC 提交应命中读回故障闸门");
    fail_ipc_read.send(()).unwrap();

    let ipc_acceptance = bounded(submitting_ipc)
        .await
        .unwrap()
        .expect("IPC 提交读回失败仍应返回线上 ConfirmationUnknown 响应");
    assert_eq!(
        ipc_acceptance,
        Acceptance::ConfirmationUnknown {
            turn_id: ipc_turn_id,
            session_id: sid,
        },
        "IPC 线上行为必须保持 ConfirmationUnknown 不变"
    );

    drop(sink);
    let _ = provider.finish().await;
    env.shutdown().await;
}

#[tokio::test]
async fn in_process_configure_new_cleans_up_expired_drafts_on_success_and_rejection() {
    let env = TestEnv::start().await;
    let provider = tests::Provider::start().await;
    super::super::tests::configure(env.dir.path(), &provider.endpoint);
    let workspace = env.workspace();

    let now = chrono::Utc::now();
    let expired_a = new_session_id();
    let fresh = new_session_id();
    configure_tests::seed_draft(
        &env.backend,
        &workspace,
        expired_a,
        now - chrono::Duration::hours(25),
    )
    .await;
    configure_tests::seed_draft(
        &env.backend,
        &workspace,
        fresh,
        now - chrono::Duration::hours(23),
    )
    .await;

    // configure(Existing) 不应触发草稿清理。
    env.service
        .configure(ConfigureRequest {
            session: ConfigureSession::Existing(fresh),
            workspace: workspace.clone(),
            agent: None,
            model: Some(ModelRef::parse("local/updated").unwrap()),
            effort: None,
        })
        .await
        .expect("进程内 configure(Existing) 应成功");
    env.wait_idle().await;
    assert!(
        env.service.head(expired_a).await.is_ok(),
        "configure(Existing) 不应删除过期草稿"
    );

    // 成功的进程内 configure(New) 应清理过期无观察者草稿，保留未过期草稿。
    let created_sid = new_session_id();
    env.service
        .configure(configure_new_request(&workspace, created_sid))
        .await
        .expect("进程内 configure(New) 应成功");
    env.wait_idle().await;
    assert_eq!(
        env.service
            .head(expired_a)
            .await
            .expect_err("过期草稿应已被清理")
            .kind,
        LocalErrorKind::NotFound,
        "进程内 configure(New) 成功后应异步删除过期草稿"
    );
    assert!(
        env.service.head(fresh).await.is_ok(),
        "未过期草稿应继续保留"
    );

    // 被拒绝的进程内 configure(New)（例如 SessionExists）同样应触发草稿清理。
    let expired_b = new_session_id();
    configure_tests::seed_draft(
        &env.backend,
        &workspace,
        expired_b,
        chrono::Utc::now() - chrono::Duration::hours(25),
    )
    .await;
    let dup_err = env
        .service
        .configure(configure_new_request(&workspace, created_sid))
        .await
        .expect_err("重复创建已有会话应返回冲突");
    assert_eq!(dup_err.conflict, Some(ConflictReason::SessionExists));
    env.wait_idle().await;
    assert_eq!(
        env.service
            .head(expired_b)
            .await
            .expect_err("configure(New) 失败后过期草稿也应被清理")
            .kind,
        LocalErrorKind::NotFound
    );

    let _ = provider.finish().await;
    env.shutdown().await;
}

#[tokio::test]
async fn stop_notification_resolves_for_subscribers_before_and_after_stopping() {
    let env = TestEnv::start().await;

    let mut before = env.service.stopping();
    let mut before_clone = before.clone();
    assert!(!before.is_stopping(), "停机前 is_stopping 应为 false");
    assert!(!before_clone.is_stopping(), "克隆句柄停机前也应为 false");

    let wait_before = tokio::spawn(async move {
        before.wait().await;
        before.is_stopping()
    });
    let wait_clone = tokio::spawn(async move {
        before_clone.wait().await;
        before_clone.is_stopping()
    });

    // 挂住元数据尾段闸门，确保后台已进入停机阶段但尚未完全退出，从而测试停机后新订阅。
    let (in_tail, release_tail) = tests::arm(&env.backend.gates.metadata_drain);
    env.stop.send(()).await.expect("发送停机信号应成功");

    assert!(
        bounded(wait_before).await.expect("停机前等待任务应完成"),
        "停机前订阅的 wait 返回时 is_stopping 必须为 true"
    );
    assert!(
        bounded(wait_clone).await.expect("克隆句柄等待任务应完成"),
        "克隆句柄 wait 返回时 is_stopping 必须为 true"
    );

    bounded(in_tail).await.expect("后台应进入停机尾段");

    // 后台进入停机之后再调用 service.stopping()，必须立即可见并立即从 wait 返回。
    let mut after = env.service.stopping();
    assert!(
        after.is_stopping(),
        "进入停机后新创建的 StopNotification 必须立即反映停机状态"
    );
    bounded(after.wait()).await;
    assert!(after.is_stopping());

    release_tail.send(()).expect("放行元数据尾段闸门应成功");
    let exit = bounded(env.serving)
        .await
        .expect("后台任务不应崩溃")
        .expect("停机收尾不应报错");
    assert_eq!(exit.drain, DrainOutcome::Completed);
    drop(exit.instance);
}
