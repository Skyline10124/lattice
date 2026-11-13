// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0
use super::*;
use lattice_llm::auth::RevocationStatus;
use lattice_local::{LocalClient, new_session_id, new_turn_id};
type Gate = (oneshot::Sender<()>, oneshot::Receiver<()>);
#[derive(Default)]
pub(super) struct Gates {
    pub registered: Mutex<Option<Gate>>,
    pub prepare: Mutex<Option<Gate>>,
    pub capture: Mutex<Option<Gate>>,
    pub capture_before: Mutex<Option<Gate>>,
    pub retire: Mutex<Option<Gate>>,
    pub forward_panic: Mutex<Option<Gate>>,
    pub rewind_committed: Mutex<Option<Gate>>,
    pub capture_checked: Mutex<Option<Gate>>,
    pub watch_wait: Mutex<Option<Gate>>,
    pub watch_done: Mutex<Option<Gate>>,
    pub watch_waiting_owner: Mutex<Option<Gate>>,
    pub cleanup_scanned: Mutex<Option<Gate>>,
    pub cleanup_claimed: Mutex<Option<Gate>>,
    /// 元数据收尾尾段（Tail 1）内。
    pub metadata_drain: Mutex<Option<Gate>>,
    /// 回执与排空之后、关闭 listener 之前。
    pub listener_close: Mutex<Option<Gate>>,
    /// 连接收尾之后、阻塞线程丢掉 Backend 之前。
    pub final_release: Mutex<Option<Gate>>,
}
pub(super) fn arm(slot: &Mutex<Option<Gate>>) -> (oneshot::Receiver<()>, oneshot::Sender<()>) {
    let (entered, entry) = oneshot::channel();
    let (release, resume) = oneshot::channel();
    assert!(lock(slot).replace((entered, resume)).is_none());
    (entry, release)
}
pub(super) async fn pause(slot: &Mutex<Option<Gate>>) {
    let gate = lock(slot).take();
    if let Some((entered, release)) = gate {
        let _ = entered.send(());
        bounded(release).await.unwrap();
    }
}
pub(super) fn pause_blocking(slot: &Mutex<Option<Gate>>) {
    let gate = lock(slot).take();
    if let Some((entered, release)) = gate {
        let (tx, rx) = std::sync::mpsc::channel();
        tokio::spawn(async move {
            let _ = tx.send(bounded(release).await);
        });
        let _ = entered.send(());
        rx.recv_timeout(Duration::from_secs(10))
            .expect("局部调度超时")
            .unwrap();
    }
}
pub(super) async fn fail_forwarding(slot: &Mutex<Option<Gate>>) {
    let gate = lock(slot).take();
    if let Some((entered, release)) = gate {
        let _ = entered.send(());
        bounded(release).await.unwrap();
        panic!("注入回执转交失败，监督者仍须等待独立 completion");
    }
}
async fn bounded<T>(future: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(10), future)
        .await
        .expect("局部调度超时")
}
pub(in crate::local) struct Provider {
    pub(in crate::local) endpoint: String,
    pub(super) arrived: oneshot::Receiver<()>,
    pub(super) release: watch::Sender<bool>,
    shutdown: oneshot::Sender<()>,
    task: tokio::task::JoinHandle<Vec<serde_json::Value>>,
}
impl Provider {
    pub(in crate::local) async fn start() -> Self {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
        let (release, mut gate) = watch::channel(true);
        let (arrival, arrived) = oneshot::channel();
        let (shutdown, mut stopping) = oneshot::channel();
        let task = tokio::spawn(async move {
            let mut arrival = Some(arrival);
            let mut requests = Vec::new();
            loop {
                let (mut socket, _) = tokio::select! { accepted = listener.accept() => accepted.unwrap(), _ = &mut stopping => break };
                bounded(async {
                    let mut headers = Vec::new();
                    while !headers.ends_with(b"\r\n\r\n") { headers.push(socket.read_u8().await.unwrap()); assert!(headers.len() <= 65_536); }
                    let headers = String::from_utf8(headers).unwrap();
                    let length: usize = headers.lines().find_map(|line| line.to_ascii_lowercase().strip_prefix("content-length:").map(|n| n.trim().parse().unwrap())).unwrap();
                    assert!(length < 4 * 1024 * 1024);
                    let mut body = vec![0; length]; socket.read_exact(&mut body).await.unwrap();
                    requests.push(serde_json::from_slice(&body).unwrap());
                    if let Some(arrival) = arrival.take() { let _ = arrival.send(()); }
                    while !*gate.borrow_and_update() { gate.changed().await.unwrap(); }
                    let body = "data: {\"choices\":[{\"delta\":{\"content\":\"完成\"},\"finish_reason\":null}]}\n\ndata: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n";
                    // 取消允许先断连接，正向返回由持久 outcome 验证。
                    let length = body.len();
                    let _ = socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {length}\r\nConnection: close\r\n\r\n{body}").as_bytes()).await;
                }).await;
            }
            requests
        });
        Self {
            endpoint,
            arrived,
            release,
            shutdown,
            task,
        }
    }
    pub(in crate::local) async fn finish(self) -> Vec<serde_json::Value> {
        self.release.send_replace(true);
        self.shutdown.send(()).unwrap();
        bounded(self.task).await.unwrap()
    }
}

struct TestServerTask {
    stop: tokio::sync::mpsc::Sender<()>,
    handle: tokio::task::JoinHandle<Result<DrainOutcome>>,
}

async fn backend() -> (tempfile::TempDir, Arc<Backend>, LocalClient, TestServerTask) {
    backend_with_roots(vec![include_bytes!("test_root.der").to_vec()]).await
}
async fn backend_with_roots(
    roots: Vec<Vec<u8>>,
) -> (tempfile::TempDir, Arc<Backend>, LocalClient, TestServerTask) {
    let dir = tempfile::tempdir().unwrap();
    let layout = StateLayout::in_state_root(dir.path());
    let (stop_tx, stop_rx) = tokio::sync::mpsc::channel(4);
    let (ready_tx, ready_rx) = oneshot::channel();
    let serving_layout = layout.clone();
    let backend_layout = layout.clone();
    let home = dir.path().to_path_buf();
    let handle = tokio::spawn(async move {
        let exit = serve_with_backend(
            serving_layout,
            Duration::from_secs(60),
            stop_rx,
            move || {
                let mut backend = Backend::with_tls_roots(
                    backend_layout,
                    Some(home),
                    roots,
                    Arc::new(crate::turn::NoTools),
                );
                Arc::get_mut(&mut backend)
                    .expect("unshared test backend")
                    .metadata = super::super::metadata::MetadataRefresh::without_network();
                let _ = ready_tx.send(backend.clone());
                backend
            },
        )
        .await?;
        drop(exit.instance);
        match exit.failure {
            Some(err) => Err(err),
            None => Ok(exit.drain),
        }
    });
    let backend = bounded(ready_rx).await.unwrap();
    (
        dir,
        backend,
        LocalClient::new(layout),
        TestServerTask {
            stop: stop_tx,
            handle,
        },
    )
}
fn configure_new(dir: &std::path::Path, sid: SessionId) -> ConfigureRequest {
    ConfigureRequest {
        session: ConfigureSession::New(sid),
        workspace: dir.into(),
        agent: Some(AgentName::parse("local-test").unwrap()),
        model: None,
        effort: None,
    }
}
fn raw_request(dir: &std::path::Path, sid: SessionId) -> TurnRequest {
    TurnRequest {
        turn_id: new_turn_id(),
        session: sid,
        workspace: dir.into(),
        input: "测试".into(),
        expected_settings: None,
    }
}
async fn create_request(client: &LocalClient, dir: &std::path::Path) -> TurnRequest {
    let sid = new_session_id();
    client.configure(configure_new(dir, sid)).await.unwrap();
    raw_request(dir, sid)
}
fn config(dir: &std::path::Path, endpoint: &str) {
    super::super::tests::configure(dir, endpoint);
}
fn invalid(dir: &std::path::Path) {
    config(dir, "http://127.0.0.1:1/v1");
    std::fs::write(dir.join(".lattice/providers.toml"), "invalid toml").unwrap();
}
async fn idle(backend: &Backend) {
    let mut changed = backend.changed.subscribe();
    bounded(async {
        while backend.has_active_work() {
            changed.changed().await.unwrap();
        }
    })
    .await;
}
async fn finish(backend: &Backend, task: TestServerTask) {
    idle(backend).await;
    let _ = task.stop.send(()).await;
    if let Err(err) = bounded(task.handle).await.unwrap() {
        panic!("后台收尾不应报错: {err:?}");
    }
}

#[tokio::test]
async fn prompt_byte_limits_are_checked_before_acceptance_and_dispatch() {
    for (size, accepted) in [(2_097_152, true), (2_097_153, false)] {
        let (dir, backend, client, task) = backend().await;
        let provider = Provider::start().await;
        config(dir.path(), &provider.endpoint);
        let path = dir.path().join(".lattice/agents/local-test/agent.toml");
        let source = std::fs::read_to_string(&path).unwrap();
        let prompt = "x".repeat(size);
        std::fs::write(
            &path,
            source.replace("system = 'test'", &format!("system = '{prompt}'")),
        )
        .unwrap();
        let sid = new_session_id();
        let configured = bounded(client.configure(configure_new(dir.path(), sid))).await;
        idle(&backend).await;
        if accepted {
            configured.unwrap();
            let input = raw_request(dir.path(), sid);
            let result = bounded(client.submit(input)).await;
            idle(&backend).await;
            assert!(matches!(result.unwrap(), Acceptance::Accepted { .. }));
        } else {
            let error = configured.unwrap_err();
            assert_eq!(error.kind, LocalErrorKind::BadRequest);
            assert!(error.message.contains(path.to_str().unwrap()), "{error}");
            assert!(error.message.contains("2097152"), "{error}");
            assert_eq!(
                client.session_head(sid).await.unwrap_err().kind,
                LocalErrorKind::NotFound
            );
        }
        assert_eq!(provider.finish().await.len(), usize::from(accepted));
        finish(&backend, task).await;
    }
}

/// 没有可用的根证书时后台照常启动、照常回答查询；https 端点在接受前按配置错误拒绝，
/// http 端点照常放行。旧实现把整个网关置为配置错误，http 提交也会被拒绝。
#[tokio::test]
async fn without_tls_roots_status_answers_http_is_accepted_and_https_is_rejected() {
    let (dir, backend, client, task) = backend_with_roots(Vec::new()).await;
    bounded(client.status())
        .await
        .expect("status must answer without usable TLS roots");

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let https = format!("https://{}/v1", listener.local_addr().unwrap());
    let connections = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = connections.clone();
    let accepting = tokio::spawn(async move {
        while let Ok(_socket) = listener.accept().await {
            counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
    });
    config(dir.path(), &https);
    let input = create_request(&client, dir.path()).await;
    let error = bounded(client.submit(input.clone())).await.unwrap_err();
    idle(&backend).await;
    assert_eq!(
        error.kind,
        LocalErrorKind::BadRequest,
        "https without roots must be rejected before acceptance: {error}"
    );
    assert!(
        error.message.contains("configuration error")
            && error.message.contains("no usable TLS root certificates"),
        "rejection must be the configuration error naming the missing roots: {error}"
    );
    assert_eq!(
        client.turn(input.turn_id).await.unwrap_err().kind,
        LocalErrorKind::NotFound,
        "rejected https submit must not create a Turn"
    );
    assert_eq!(
        connections.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "https provider must not be contacted"
    );
    accepting.abort();

    let provider = Provider::start().await;
    config(dir.path(), &provider.endpoint);
    let accepted = bounded(client.submit(raw_request(dir.path(), input.session))).await;
    idle(&backend).await;
    assert!(
        matches!(accepted, Ok(Acceptance::Accepted { .. })),
        "http submit must be accepted without TLS roots: {accepted:?}"
    );
    assert_eq!(
        provider.finish().await.len(),
        1,
        "http provider received the request"
    );
    bounded(client.status())
        .await
        .expect("status still answers after the rejection");
    finish(&backend, task).await;
}

#[tokio::test]
async fn removed_lancet_or_tools_section_is_rejected_before_acceptance() {
    let (dir, backend, client, task) = backend().await;
    let provider = Provider::start().await;
    config(dir.path(), &provider.endpoint);
    let profile = dir.path().join(".lattice/agents/local-test/agent.toml");
    let source = std::fs::read_to_string(&profile).unwrap();
    for (extra, section) in [
        ("[lancet.policy]\npath='policy.toml'\n", "[lancet]"),
        ("[tools]\nenabled=['observe__workspace']\n", "[tools]"),
    ] {
        std::fs::write(&profile, format!("{source}\n{extra}")).unwrap();
        let sid = new_session_id();
        let error = bounded(client.configure(configure_new(dir.path(), sid)))
            .await
            .unwrap_err();
        assert_eq!(error.kind, LocalErrorKind::BadRequest);
        assert!(
            error
                .message
                .contains(&format!("forbidden top-level section {section}")),
            "unexpected error message: {}",
            error.message
        );
        idle(&backend).await;
        assert!(matches!(
            client.list_sessions().await.unwrap_err().store.as_deref(),
            Some(StoreDiagnostic::NotInitialized { .. })
        ));
    }
    assert!(provider.finish().await.is_empty());
    finish(&backend, task).await;
}

#[tokio::test]
async fn legacy_store_rejection_before_execution_does_not_leave_unconfirmed_cleanup() {
    let (dir, backend, client, task) = backend().await;
    let legacy = backend.layout.lattice_root().join("sessions.git");
    std::fs::create_dir_all(&legacy).unwrap();
    config(dir.path(), "http://127.0.0.1:1/v1");
    let sid = new_session_id();
    let error = client
        .configure(configure_new(dir.path(), sid))
        .await
        .unwrap_err();
    assert!(
        matches!(error.store.as_deref(), Some(StoreDiagnostic::LegacyStores { paths }) if paths == std::slice::from_ref(&legacy))
    );
    let submit_err = client
        .submit(raw_request(dir.path(), sid))
        .await
        .unwrap_err();
    assert!(
        matches!(submit_err.store.as_deref(), Some(StoreDiagnostic::LegacyStores { paths }) if paths == std::slice::from_ref(&legacy))
    );
    idle(&backend).await;
    assert_eq!(
        client.stop().await.unwrap().unconfirmed_operations,
        0,
        "接受前拒绝没有需要确认的写入或收尾"
    );
    finish(&backend, task).await;
}
#[tokio::test]
async fn admission_identity_conflicts_busy_and_departed_submitter_never_accepts_turn() {
    let (dir, backend, client, task) = backend().await;
    config(dir.path(), "http://127.0.0.1:1/v1");
    let input = create_request(&client, dir.path()).await;
    let sid = input.session;
    let (entered, release) = arm(&backend.gates.registered);
    let mut wire = lattice_local::test_support::connect(backend.layout.clone())
        .await
        .unwrap();
    ipc::write(
        &mut wire,
        &Request::Submit {
            request: input.clone(),
        },
    )
    .await
    .unwrap();
    bounded(entered).await.unwrap();
    assert!(matches!(
        client.turn(input.turn_id).await.unwrap(),
        TurnLookup::InFlight
    ));
    let conflict = client
        .submit({
            invalid(dir.path());
            input.clone()
        })
        .await
        .unwrap_err();
    assert_eq!(
        (conflict.kind, conflict.conflict),
        (LocalErrorKind::Conflict, Some(ConflictReason::TurnIdUsed))
    );
    let mut race = input.clone();
    race.turn_id = new_turn_id();
    assert_eq!(
        client
            .submit({
                invalid(dir.path());
                race.clone()
            })
            .await
            .unwrap_err()
            .kind,
        LocalErrorKind::Busy
    );
    assert!(client.status().await.unwrap().owners >= 1);
    drop(wire);
    release.send(()).unwrap();
    idle(&backend).await;
    assert_eq!(
        client.turn(input.turn_id).await.unwrap_err().kind,
        LocalErrorKind::NotFound
    );
    assert_eq!(
        client.session_head(sid).await.unwrap().position,
        NodeId(0),
        "提前离场的提交者不能写入轮次节点"
    );
    let provider = Provider::start().await;
    assert!(
        matches!(client.submit({ config(dir.path(), &provider.endpoint); race.clone() }).await.unwrap(), Acceptance::Accepted { turn } if turn.session_id == sid)
    );
    idle(&backend).await;
    assert!(matches!(
        client.turn(race.turn_id).await.unwrap(),
        TurnLookup::Accepted { .. }
    ));
    assert_eq!(provider.finish().await.len(), 1);
    finish(&backend, task).await;
}
#[tokio::test]
async fn identity_table_prioritizes_used_turn_binding_busy_and_configure_collision() {
    let (dir, backend, client, task) = backend().await;
    let mut provider = Provider::start().await;
    provider.release.send_replace(false);
    config(dir.path(), &provider.endpoint);
    let input = create_request(&client, dir.path()).await;
    let sid = input.session;
    client.submit(input.clone()).await.unwrap();
    bounded(&mut provider.arrived).await.unwrap();
    let wrong = tempfile::tempdir().unwrap();
    let mut reused = input.clone();
    reused.session = new_session_id();
    reused.workspace = wrong.path().into();
    let error = client
        .submit({
            invalid(dir.path());
            reused
        })
        .await
        .unwrap_err();
    assert_eq!(error.conflict, Some(ConflictReason::TurnIdUsed));
    let mut missing = input.clone();
    missing.turn_id = new_turn_id();
    missing.session = new_session_id();
    assert_eq!(
        client
            .submit({
                invalid(dir.path());
                missing
            })
            .await
            .unwrap_err()
            .kind,
        LocalErrorKind::NotFound
    );
    let mut appended = input.clone();
    appended.turn_id = new_turn_id();
    appended.session = sid;
    appended.workspace = wrong.path().into();
    assert_eq!(
        client
            .submit({
                invalid(dir.path());
                appended.clone()
            })
            .await
            .unwrap_err()
            .conflict,
        Some(ConflictReason::BindingMismatch)
    );
    appended.workspace = input.workspace.clone();
    assert_eq!(
        client
            .submit({
                invalid(dir.path());
                appended
            })
            .await
            .unwrap_err()
            .kind,
        LocalErrorKind::Busy
    );
    assert_eq!(
        client
            .configure(configure_new(dir.path(), sid))
            .await
            .unwrap_err()
            .kind,
        LocalErrorKind::Busy
    );
    assert!(client.list_sessions().await.unwrap()[0].owner);
    assert!(!client.cancel(new_turn_id()).await.unwrap());
    assert!(client.cancel(input.turn_id).await.unwrap());
    assert!(client.cancel(input.turn_id).await.unwrap());
    provider.release.send_replace(true);
    idle(&backend).await;
    assert_eq!(
        client
            .configure(configure_new(dir.path(), sid))
            .await
            .unwrap_err()
            .conflict,
        Some(ConflictReason::SessionExists)
    );
    assert_eq!(
        client
            .rewind(
                sid,
                NodeId(0),
                crate::LocalWorkspaceRoot::from_path(wrong.path())
                    .unwrap()
                    .canonical_path()
                    .to_path_buf(),
            )
            .await
            .unwrap_err()
            .conflict,
        Some(ConflictReason::BindingMismatch),
        "在另一个项目目录下执行 rewind 必须返回项目绑定不一致冲突"
    );
    assert!(!client.cancel(input.turn_id).await.unwrap());
    assert!(!client.list_sessions().await.unwrap()[0].owner);
    let error = client
        .submit({
            invalid(dir.path());
            input.clone()
        })
        .await
        .unwrap_err();
    assert_eq!(error.conflict, Some(ConflictReason::TurnIdUsed));
    assert_eq!(provider.finish().await.len(), 1);
    finish(&backend, task).await;
}
#[tokio::test]
async fn preparation_cancel_is_owned_and_rejected_request_does_not_poison_retry() {
    let (dir, backend, client, task) = backend().await;
    config(dir.path(), "http://127.0.0.1:1/v1");
    let input = create_request(&client, dir.path()).await;
    let (entered, release) = arm(&backend.gates.prepare);
    let submit = {
        let client = client.clone();
        let input = input.clone();
        invalid(dir.path());
        tokio::spawn(async move { client.submit(input).await })
    };
    bounded(entered).await.unwrap();
    assert!(client.cancel(input.turn_id).await.unwrap());
    release.send(()).unwrap();
    assert_eq!(
        bounded(submit).await.unwrap().unwrap_err().kind,
        LocalErrorKind::BadRequest
    );
    idle(&backend).await;
    let provider = Provider::start().await;
    let acceptance = client
        .submit({
            config(dir.path(), &provider.endpoint);
            input.clone()
        })
        .await
        .unwrap();
    assert!(matches!(acceptance, Acceptance::Accepted { .. }));
    idle(&backend).await;
    let TurnLookup::Accepted { turn } = client.turn(input.turn_id).await.unwrap() else {
        panic!("accepted")
    };
    assert_eq!(
        turn.summary.outcome,
        crate::local::PublicTurnOutcome::Completed
    );
    assert_eq!(provider.finish().await.len(), 1);
    finish(&backend, task).await;
}
#[tokio::test]
async fn owner_retirement_updates_watch_and_stop_drains_the_same_cancellation_carrier() {
    let (dir, backend, client, task) = backend().await;
    let mut provider = Provider::start().await;
    provider.release.send_replace(false);
    config(dir.path(), &provider.endpoint);
    let input = create_request(&client, dir.path()).await;
    client.submit(input.clone()).await.unwrap();
    bounded(&mut provider.arrived).await.unwrap();
    let mut view = client.watch(input.session).await.unwrap();
    assert!(matches!(
        view.next().await.unwrap().unwrap(),
        ViewUpdate::Head(SessionHead { owner: true, .. })
    ));
    let (at_close, release_close) = arm(&backend.gates.listener_close);
    let stop = {
        let client = client.clone();
        tokio::spawn(async move { client.request_stop().await })
    };
    let mut changed = backend.changed.subscribe();
    bounded(async {
        while !backend.status().stopping {
            tokio::task::yield_now().await;
        }
    })
    .await;
    assert!(!stop.is_finished());
    // Owner 尚未排空时只关闭登录，认证生命周期仍可接纳操作。
    assert_eq!(
        backend.chatgpt_auth().unwrap().logout().await.unwrap(),
        RevocationStatus::NotNeeded
    );
    assert_eq!(
        client
            .submit({
                invalid(dir.path());
                raw_request(dir.path(), new_session_id())
            })
            .await
            .unwrap_err()
            .kind,
        LocalErrorKind::Stopping
    );
    assert!(client.cancel(input.turn_id).await.unwrap());
    provider.release.send_replace(true);
    bounded(async {
        while backend.status().owners != 0 {
            changed.changed().await.unwrap();
        }
    })
    .await;
    let (stop_status, stop_departure) = bounded(stop).await.unwrap().unwrap();
    assert_eq!(stop_status.owners, 0);
    bounded(at_close).await.unwrap();
    assert!(backend.chatgpt_auth().unwrap().logout().await.is_err());
    bounded(async {
        loop {
            if matches!(
                view.next().await.unwrap().unwrap(),
                ViewUpdate::Head(SessionHead { owner: false, .. })
            ) {
                break;
            }
        }
    })
    .await;
    drop(view);
    release_close.send(()).unwrap();
    bounded(stop_departure.wait(Duration::from_secs(5)))
        .await
        .expect("放行连接收尾门控后 socket 与实例锁应释放");
    assert_eq!(provider.finish().await.len(), 1);
    finish(&backend, task).await;
}
async fn seed_round(backend: &Backend, sid: SessionId, fresh: bool) -> TurnId {
    use lattice_session::store::*;
    let api = backend.stores.acquire(true).await.unwrap();
    let id = new_turn_id();
    let project =
        crate::LocalWorkspaceRoot::from_path(backend.layout.lattice_root().parent().unwrap())
            .unwrap()
            .project_path();
    api.blocking(move |api| {
        let guard = api.store.try_lock_session(sid)?;
        let config = FrozenTurnConfig {
            effort: None,
            agent_name: AgentName::parse("local-test").unwrap(),
            model: ModelId::parse("test").unwrap(),
            provider: ProviderId::parse("local").unwrap(),
            system_prompt: None,
            catalog: ToolCatalogSnapshot::new(vec![]).unwrap(),
            limits: TurnExecutionLimits {
                timeout_secs: lattice_session::TurnTimeout::default(),
                max_model_steps: 32,
                max_model_attempts: 2,
            },
        };
        if fresh {
            let settings = SessionSettings::for_test(&config);
            api.store
                .create_session(&guard, project.clone(), settings, chrono::Utc::now())?;
        }
        let settings_version = api.store.begin_read()?.session_settings(sid)?.version()?;
        api.store.accept_turn(
            &guard,
            AcceptTurn {
                session_id: sid,
                turn_id: id,
                project_path: project,
                input: "seed".into(),
                accepted_at: chrono::Utc::now(),
                config,
                settings_version,
            },
        )?;
        Ok(())
    })
    .await
    .unwrap();
    id
}
async fn close_seed(backend: &Backend, sid: SessionId, turn_id: TurnId) {
    let io = CoordinatorStoreIo::acquire_api(backend.open().await.unwrap(), sid)
        .await
        .unwrap();
    io.close_step(lattice_session::store::CloseStep::StopBetweenRequests {
        turn_id,
        cause: lattice_session::store::StopCause::Cancelled,
    })
    .await
    .unwrap();
}
#[tokio::test]
async fn watch_starts_with_ten_turns_history_has_whole_earlier_turns_and_rewind_replaces_path() {
    let (dir, backend, client, task) = backend().await;
    let sid = new_session_id();
    for i in 0..12 {
        let turn = seed_round(&backend, sid, i == 0).await;
        close_seed(&backend, sid, turn).await;
    }
    let mut view = client.watch(sid).await.unwrap();
    assert!(matches!(
        view.next().await.unwrap().unwrap(),
        ViewUpdate::Head(SessionHead {
            position: NodeId(24),
            path_replaced: false,
            owner: false,
            ..
        })
    ));
    let mut current = Vec::new();
    for _ in 0..20 {
        let ViewUpdate::Entry { entry } = view.next().await.unwrap().unwrap() else {
            panic!("entry")
        };
        current.push(entry);
    }
    assert!(matches!(
        current[0].as_ref(),
        ConversationEntry::UserMessage {
            ordinal: 3,
            node: NodeId(5),
            ..
        }
    ));
    assert_eq!(current.last().unwrap().node(), NodeId(24));
    let earlier = client.history(sid, NodeId(5), 10).await.unwrap();
    assert_eq!(earlier.entries.len(), 4);
    assert!(matches!(
        earlier.entries[0],
        ConversationEntry::UserMessage { ordinal: 1, .. }
    ));
    client
        .rewind(sid, NodeId(2), dir.path().into())
        .await
        .unwrap();
    let mut replaced = false;
    let mut entries = Vec::new();
    bounded(async {
        loop {
            match view.next().await.unwrap().unwrap() {
                ViewUpdate::Head(head) if head.path_replaced => {
                    assert_eq!(head.position, NodeId(2));
                    replaced = true;
                    entries.clear();
                }
                ViewUpdate::Entry { entry } if replaced => {
                    let last = entry.node() == NodeId(2);
                    entries.push(entry);
                    if last {
                        break;
                    }
                }
                _ => {}
            }
        }
    })
    .await;
    assert_eq!(entries.len(), 2);
    assert_eq!(client.history(sid, NodeId(5), 10).await.unwrap(), earlier);
    assert!(entries.iter().all(|entry| entry.node() <= NodeId(2)));
    drop(view);
    finish(&backend, task).await;
}
#[tokio::test]
async fn diagnostic_and_owner_only_heads_repeat_the_current_entry() {
    let (dir, backend, client, task) = backend().await;
    let mut provider = Provider::start().await;
    provider.release.send_replace(false);
    config(dir.path(), &provider.endpoint);
    let input = create_request(&client, dir.path()).await;
    let sid = input.session;
    client.submit(input.clone()).await.unwrap();
    bounded(&mut provider.arrived).await.unwrap();
    let sink = backend.observer(sid);
    let (waiting, resume) = arm(&backend.gates.watch_wait);
    let mut view = client.watch(sid).await.unwrap();
    assert!(matches!(
        view.next().await.unwrap(),
        Some(ViewUpdate::Head(SessionHead { owner: true, .. }))
    ));
    view.next().await.unwrap().unwrap();
    let initial = view.next().await.unwrap().unwrap();
    bounded(waiting).await.unwrap();

    let diagnostic = LocalError::new(LocalErrorKind::Internal, "仅诊断变化");
    lock(&backend.admission)
        .diagnostics
        .insert(input.turn_id, diagnostic.clone());
    // 仅唤醒推送而不标记任何节点，不能借节点变化掩盖缺失的补推。
    sink.notify_without_marking();
    let (waiting, resume_next) = arm(&backend.gates.watch_wait);
    resume.send(()).unwrap();
    assert!(
        matches!(view.next().await.unwrap(), Some(ViewUpdate::Head(SessionHead { owner: true, diagnostic: Some(error), .. })) if error == diagnostic)
    );
    let repeated = tokio::time::timeout(Duration::from_secs(1), view.next()).await;
    assert!(
        matches!(repeated, Ok(Ok(Some(ref update))) if *update == initial),
        "仅诊断变化的 head 后也必须补推当前位置条目"
    );
    bounded(waiting).await.unwrap();

    let (retiring, retire) = arm(&backend.gates.retire);
    provider.release.send_replace(true);
    bounded(retiring).await.unwrap();
    let (waiting, resume_final) = arm(&backend.gates.watch_wait);
    resume_next.send(()).unwrap();
    assert!(matches!(
        view.next().await.unwrap(),
        Some(ViewUpdate::Head(SessionHead { owner: true, .. }))
    ));
    let final_entry = view.next().await.unwrap().unwrap();
    assert!(
        matches!(&final_entry, ViewUpdate::Entry { entry } if matches!(entry.as_ref(), ConversationEntry::Step { summary: Some(summary), .. } if summary.progress.is_closed()))
    );
    bounded(waiting).await.unwrap();
    retire.send(()).unwrap();
    idle(&backend).await;
    resume_final.send(()).unwrap();
    assert!(matches!(
        view.next().await.unwrap(),
        Some(ViewUpdate::Head(SessionHead { owner: false, .. }))
    ));
    assert_eq!(
        bounded(view.next()).await.unwrap().unwrap(),
        final_entry,
        "仅 Owner 变化也补推同一条目"
    );
    drop((view, sink));
    assert_eq!(provider.finish().await.len(), 1);
    finish(&backend, task).await;
}
#[tokio::test]
async fn slow_observer_reads_latest_entries_suffixes_and_attempt_boundaries() {
    use lattice_session::store::*;
    let (_dir, backend, _client, task) = backend().await;
    let sid = new_session_id();
    let turn_id = seed_round(&backend, sid, true).await;
    let sink = backend.observer(sid);
    sink.emit(crate::turn::TurnEvent::TurnStarted {
        turn: TurnRef {
            session_id: sid,
            turn_id,
        },
        user_node: NodeId(1),
    });
    let slow = sink.register();
    let fast = sink.register();
    let io = CoordinatorStoreIo::acquire_api(backend.open().await.unwrap(), sid)
        .await
        .unwrap();
    let mut key = ModelAttemptKey {
        turn_id,
        model_step_ordinal: 0,
        attempt_ordinal: 0,
    };
    let attempt_origin = AttemptOrigin {
        protocol: AttemptProtocol::ChatCompletions,
        provider_id: ProviderId::parse("local").unwrap(),
    };
    io.start_request(key, attempt_origin.clone()).await.unwrap();
    sink.emit(crate::turn::TurnEvent::ModelStarted {
        key,
        origin: attempt_origin.clone(),
        model: "test".into(),
    });
    sink.emit(crate::turn::TurnEvent::TokenDelta {
        key,
        content: "你".into(),
    });
    sink.emit(crate::turn::TurnEvent::TokenDelta {
        key,
        content: "好".into(),
    });
    let capture = |cursor: Arc<Mutex<crate::turn::observer::PushState>>| {
        let sink = sink.clone();
        let api = backend.stores.clone();
        async move {
            let api = api.acquire(false).await.unwrap();
            api.blocking(move |api| Ok(sink.capture(&api.store, sid, &cursor)?))
                .await
                .unwrap()
        }
    };
    let batch = capture(fast.clone()).await;
    let text = batch
        .iter()
        .find_map(|update| {
            if let ViewUpdate::Text(text) = update {
                Some(text)
            } else {
                None
            }
        })
        .unwrap();
    assert_eq!(
        text.segments,
        vec![TextSegment::Text {
            text: "你好".into()
        }]
    );
    assert_eq!(text.offset, 0);
    sink.sent(&fast, text);
    sink.emit(crate::turn::TurnEvent::TokenDelta {
        key,
        content: "！".into(),
    });
    let batch = capture(fast.clone()).await;
    assert!(
        matches!(batch.as_slice(), [ViewUpdate::Text(TextUpdate { offset: 6, segments, .. })] if segments == &[TextSegment::Text { text: "！".into() }])
    );
    io.stop_attempt(AttemptStopped {
        usage_kind: lattice_conversation::UsageCompleteness::Partial,
        cost: None,
        key,
        origin: attempt_origin.clone(),
        usage: None,
        cause: StopCause::ProviderError { retryable: true },
        retry_delay_ms: Some(1),
        diagnostic: AttemptDiagnostic {
            provider_error: Some("retry".into()),
            ..Default::default()
        },
    })
    .await
    .unwrap();
    sink.emit(crate::turn::TurnEvent::LlmRetry {
        key,
        delay_ms: 1,
        notice: Notice {
            level: NoticeLevel::Warn,
            text: "retry".into(),
        },
    });
    key.attempt_ordinal = 1;
    io.start_request(key, attempt_origin.clone()).await.unwrap();
    sink.emit(crate::turn::TurnEvent::ModelStarted {
        key,
        origin: attempt_origin.clone(),
        model: "test".into(),
    });
    sink.emit(crate::turn::TurnEvent::ReasoningDelta {
        key,
        content: "新尝试".into(),
    });
    let batch = capture(slow.clone()).await;
    assert!(batch.iter().any(|update| matches!(update, ViewUpdate::Entry { entry } if matches!(entry.as_ref(), ConversationEntry::Step { step, retries, .. } if step.attempts.len() == 2 && retries.len() == 1))));
    assert!(
        matches!(batch.last(), Some(ViewUpdate::Text(TextUpdate { attempt: 1, offset: 0, segments, .. })) if segments == &[TextSegment::Reasoning { text: "新尝试".into() }])
    );
    let ViewUpdate::Text(text) = batch.last().unwrap() else {
        panic!("text")
    };
    sink.sent(&slow, text);
    sink.emit(crate::turn::TurnEvent::TokenDelta {
        key,
        content: "继续".into(),
    });
    let batch = capture(slow.clone()).await;
    assert!(
        matches!(batch.as_slice(), [ViewUpdate::Text(TextUpdate { attempt: 1, offset: 9, segments, .. })] if segments == &[TextSegment::Text { text: "继续".into() }])
    );
    // 关闭已提交但变化尚未标记，初次推送必须读到最终节点，旧尝试的文字不能复活。
    let simultaneous = sink.register();
    io.stop_attempt(AttemptStopped {
        usage_kind: lattice_conversation::UsageCompleteness::Partial,
        cost: None,
        key,
        origin: attempt_origin.clone(),
        usage: None,
        cause: StopCause::Cancelled,
        retry_delay_ms: None,
        diagnostic: Default::default(),
    })
    .await
    .unwrap();
    let batch = capture(simultaneous).await;
    let api = backend.open().await.unwrap();
    let persisted = api
        .read(move |r| {
            r.entry(NodeRef {
                session_id: sid,
                node: NodeId(2),
            })
        })
        .await
        .unwrap();
    assert!(
        batch
            .iter()
            .any(|update| matches!(update, ViewUpdate::Entry { entry } if **entry == persisted))
    );
    assert!(
        !batch
            .iter()
            .any(|update| matches!(update, ViewUpdate::Text(_)))
    );
    // 已有观察者的纯文字推送同样必须以已提交的 Ended 为准。
    let batch = capture(slow).await;
    assert!(batch.is_empty());
    drop((api, io));
    finish(&backend, task).await;
}
#[tokio::test]
async fn owner_retiring_during_capture_never_pairs_an_old_entry_with_no_owner() {
    let (dir, backend, client, task) = backend().await;
    let mut provider = Provider::start().await;
    provider.release.send_replace(false);
    config(dir.path(), &provider.endpoint);
    let input = create_request(&client, dir.path()).await;
    client.submit(input.clone()).await.unwrap();
    bounded(&mut provider.arrived).await.unwrap();
    let (entered, release) = arm(&backend.gates.capture);
    let mut view = client.watch(input.session).await.unwrap();
    bounded(entered).await.unwrap();
    provider.release.send_replace(true);
    idle(&backend).await;
    release.send(()).unwrap();
    assert!(matches!(
        view.next().await.unwrap().unwrap(),
        ViewUpdate::Head(SessionHead {
            owner: false,
            path_replaced: false,
            ..
        })
    ));
    let mut entries = Vec::new();
    for _ in 0..2 {
        let ViewUpdate::Entry { entry } = view.next().await.unwrap().unwrap() else {
            panic!("entry")
        };
        entries.push(entry);
    }
    let persisted = backend
        .open()
        .await
        .unwrap()
        .read(move |r| {
            r.entry(NodeRef {
                session_id: input.session,
                node: NodeId(2),
            })
        })
        .await
        .unwrap();
    assert_eq!(*entries[1], persisted);
    assert!(
        matches!(entries[1].as_ref(), ConversationEntry::Step { summary: Some(summary), .. } if summary.progress.is_closed())
    );
    drop(view);
    assert_eq!(provider.finish().await.len(), 1);
    finish(&backend, task).await;
}
#[tokio::test]
async fn owner_appearing_and_retiring_during_capture_reloads_the_final_entry() {
    let (dir, backend, client, task) = backend().await;
    let first_provider = Provider::start().await;
    config(dir.path(), &first_provider.endpoint);
    let first = create_request(&client, dir.path()).await;
    let sid = first.session;
    client.submit(first).await.unwrap();
    idle(&backend).await;
    assert_eq!(first_provider.finish().await.len(), 1);
    let (before, resume_read) = arm(&backend.gates.capture_before);
    let mut view = client.watch(sid).await.unwrap();
    bounded(before).await.unwrap();
    let mut provider = Provider::start().await;
    provider.release.send_replace(false);
    config(dir.path(), &provider.endpoint);
    let input = raw_request(dir.path(), sid);
    client.submit(input.clone()).await.unwrap();
    bounded(&mut provider.arrived).await.unwrap();
    let (captured, resume_check) = arm(&backend.gates.capture);
    resume_read.send(()).unwrap();
    bounded(captured).await.unwrap();
    provider.release.send_replace(true);
    idle(&backend).await;
    resume_check.send(()).unwrap();
    assert!(matches!(
        view.next().await.unwrap().unwrap(),
        ViewUpdate::Head(SessionHead { owner: false, .. })
    ));
    let mut last = None;
    for _ in 0..4 {
        let ViewUpdate::Entry { entry } = view.next().await.unwrap().unwrap() else {
            panic!("entry")
        };
        last = Some(entry);
    }
    assert!(
        matches!(last.as_deref(), Some(ConversationEntry::Step { summary: Some(summary), .. }) if summary.progress.is_closed()),
        "无→有→无的 Owner 变化不能把运行态条目配上无 Owner 的头"
    );
    drop(view);
    assert_eq!(provider.finish().await.len(), 1);
    finish(&backend, task).await;
}
#[tokio::test]
async fn retried_capture_keeps_final_updates_for_entries_outside_the_recent_window() {
    let (dir, backend, client, task) = backend().await;
    let sid = new_session_id();
    let first = seed_round(&backend, sid, true).await;
    let sink = backend.observer(sid);
    sink.emit(crate::turn::TurnEvent::TurnStarted {
        turn: TurnRef {
            session_id: sid,
            turn_id: first,
        },
        user_node: NodeId(1),
    });
    let (waiting, resume_watch) = arm(&backend.gates.watch_wait);
    let mut view = client.watch(sid).await.unwrap();
    for _ in 0..3 {
        view.next().await.unwrap().unwrap();
    }
    bounded(waiting).await.unwrap();
    let (captured, resume_check) = arm(&backend.gates.capture);
    close_seed(&backend, sid, first).await;
    sink.release();
    resume_watch.send(()).unwrap();
    bounded(captured).await.unwrap();
    // 丢弃这一批之前，让旧条目移出最近十轮，并发生一次完整的 Owner 交接。
    for ordinal in 1..=11 {
        let id = seed_round(&backend, sid, false).await;
        sink.emit(crate::turn::TurnEvent::TurnStarted {
            turn: TurnRef {
                session_id: sid,
                turn_id: id,
            },
            user_node: NodeId(ordinal * 2 + 1),
        });
        close_seed(&backend, sid, id).await;
    }
    let rejected = raw_request(dir.path(), sid);
    assert_eq!(
        client
            .submit({
                invalid(dir.path());
                rejected
            })
            .await
            .unwrap_err()
            .kind,
        LocalErrorKind::BadRequest
    );
    resume_check.send(()).unwrap();
    let mut old_final = false;
    bounded(async {
        loop {
            if let ViewUpdate::Entry { entry } = view.next().await.unwrap().unwrap() {
                if entry.node() == NodeId(2) {
                    old_final = matches!(*entry, ConversationEntry::Step { summary: Some(ref summary), .. } if summary.progress.is_closed());
                }
                if entry.node() == NodeId(24) { break; }
            }
        }
    }).await;
    assert!(
        old_final,
        "重读不能丢掉已经展示、但已移出最近十轮的节点最终更新"
    );
    drop(view);
    finish(&backend, task).await;
}
#[tokio::test]
async fn accepted_read_failure_before_turn_started_republishes_position_after_retirement() {
    for has_prior_turn in [false, true] {
        let (dir, backend, client, task) = backend().await;
        config(dir.path(), "http://127.0.0.1:1/v1");
        let input = create_request(&client, dir.path()).await;
        let sid = input.session;
        let sink = backend.observer(sid);
        if has_prior_turn {
            let provider = Provider::start().await;
            config(dir.path(), &provider.endpoint);
            client.submit(raw_request(dir.path(), sid)).await.unwrap();
            idle(&backend).await;
            assert_eq!(provider.finish().await.len(), 1);
        }
        let position = NodeId(if has_prior_turn { 4 } else { 2 });
        let provider = Provider::start().await;
        let (accepted, fail_read) = arm(&sink.accepted_read_failure);
        let submitting = {
            let client = client.clone();
            let input = input.clone();
            config(dir.path(), &provider.endpoint);
            tokio::spawn(async move { client.submit(input).await })
        };
        bounded(accepted).await.unwrap();
        assert!(!sink.events().iter().any(|event| matches!(event, crate::turn::TurnEvent::TurnStarted { turn, .. } if turn.turn_id == input.turn_id)));
        let TurnLookup::Accepted { turn: detail } = client.turn(input.turn_id).await.unwrap()
        else {
            panic!("accepted")
        };
        let summary = detail.summary.clone();
        let (waiting, resume_watch) = arm(&backend.gates.watch_wait);
        let mut turn = client.follow_turn(detail);
        assert!(
            matches!(bounded(turn.next()).await, Some(LocalUpdate::Head(SessionHead { owner: true, position: node, .. })) if node == position)
        );
        loop {
            let Some(LocalUpdate::Entry { entry }) = bounded(turn.next()).await else {
                panic!("entry")
            };
            if entry.node() == position {
                assert!(
                    matches!(entry.as_ref(), ConversationEntry::Step { step, .. } if matches!(step.progress, StepProgress::Reserved))
                );
                break;
            }
        }
        bounded(waiting).await.unwrap();
        fail_read.send(()).unwrap();
        assert!(matches!(
            bounded(submitting).await.unwrap().unwrap(),
            Acceptance::ConfirmationUnknown { turn_id, session_id }
                if turn_id == input.turn_id && session_id == sid
        ));
        idle(&backend).await;
        // 退场已经完成，才开始下一次 capture；前后代次相同，不能借重试补齐条目。
        let (capturing, resume_capture) = arm(&backend.gates.capture_before);
        resume_watch.send(()).unwrap();
        bounded(capturing).await.unwrap();
        assert_eq!(backend.status().owners, 0);
        resume_capture.send(()).unwrap();
        assert!(
            matches!(bounded(turn.next()).await, Some(LocalUpdate::Head(SessionHead { owner: false, position: node, diagnostic: Some(_), .. })) if node == position)
        );
        let update = tokio::time::timeout(Duration::from_secs(1), turn.next()).await;
        assert!(
            matches!(update, Ok(Some(LocalUpdate::Entry { entry })) if entry.node() == position),
            "接受后读取失败且未发布 TurnStarted，Owner 退场后仍须补推真正的当前位置条目"
        );
        assert!(bounded(turn.next()).await.is_none());
        assert_eq!(turn.detach().await.summary, Some(summary));
        assert_eq!(provider.finish().await.len(), 0);
        drop(sink);
        finish(&backend, task).await;
    }
}
#[tokio::test]
async fn committed_rewind_publishes_replacement_before_any_new_position_head() {
    let (dir, backend, client, task) = backend().await;
    let sid = new_session_id();
    let first = seed_round(&backend, sid, true).await;
    close_seed(&backend, sid, first).await;
    let second = seed_round(&backend, sid, false).await;
    close_seed(&backend, sid, second).await;
    let (waiting, resume_watch) = arm(&backend.gates.watch_wait);
    let mut view = client.watch(sid).await.unwrap();
    for _ in 0..5 {
        view.next().await.unwrap().unwrap();
    }
    bounded(waiting).await.unwrap();
    let (committed, publish) = arm(&backend.gates.rewind_committed);
    let rewinding = {
        let client = client.clone();
        let path = dir.path().to_path_buf();
        tokio::spawn(async move { client.rewind(sid, NodeId(2), path).await })
    };
    bounded(committed).await.unwrap();
    let (checked, send) = arm(&backend.gates.capture_checked);
    backend.observer(sid).release();
    resume_watch.send(()).unwrap();
    bounded(checked).await.unwrap();
    // 判定已经完成但尚未写 socket。先完成 rewind 的发布，再让该批次写出。
    publish.send(()).unwrap();
    bounded(rewinding).await.unwrap().unwrap();
    send.send(()).unwrap();
    assert!(
        matches!(
            view.next().await.unwrap().unwrap(),
            ViewUpdate::Head(SessionHead {
                position: NodeId(2),
                path_replaced: true,
                ..
            })
        ),
        "rewind 后的第一条新位置会话头必须标记路径替换"
    );
    for node in [NodeId(1), NodeId(2)] {
        assert!(
            matches!(view.next().await.unwrap().unwrap(), ViewUpdate::Entry { entry } if entry.node() == node)
        );
    }
    drop(view);
    finish(&backend, task).await;
}
#[tokio::test]
async fn observer_cache_reclaims_missing_rejected_and_disconnected_sessions() {
    let (dir, backend, client, task) = backend().await;
    for _ in 0..12 {
        let sid = new_session_id();
        let mut view = client.watch(sid).await.unwrap();
        assert_eq!(
            view.next().await.unwrap_err().kind,
            LocalErrorKind::NotFound
        );
    }
    assert!(
        lock(&backend.observers).is_empty(),
        "不存在的会话不能留下观察缓存"
    );
    for _ in 0..12 {
        assert!(
            client
                .configure({
                    invalid(dir.path());
                    configure_new(dir.path(), new_session_id())
                })
                .await
                .is_err()
        );
        assert!(
            client
                .submit(raw_request(dir.path(), new_session_id()))
                .await
                .is_err()
        );
    }
    idle(&backend).await;
    assert!(
        lock(&backend.observers).is_empty(),
        "被拒绝的新会话不能留下观察缓存"
    );
    for _ in 0..12 {
        let sid = new_session_id();
        let id = seed_round(&backend, sid, true).await;
        close_seed(&backend, sid, id).await;
        let mut view = client.watch(sid).await.unwrap();
        for _ in 0..3 {
            view.next().await.unwrap().unwrap();
        }
        let (done, release) = arm(&backend.gates.watch_done);
        drop(view);
        bounded(done).await.unwrap();
        assert!(lock(&backend.observers).is_empty());
        release.send(()).unwrap();
    }
    assert!(
        lock(&backend.observers).is_empty(),
        "最后一个观察连接退出后必须回收缓存"
    );
    let shared_sid = new_session_id();
    let shared_turn = seed_round(&backend, shared_sid, true).await;
    close_seed(&backend, shared_sid, shared_turn).await;
    let mut views = Vec::new();
    for _ in 0..2 {
        let mut view = client.watch(shared_sid).await.unwrap();
        for _ in 0..3 {
            view.next().await.unwrap().unwrap();
        }
        views.push(view);
    }
    for remaining in [1, 0] {
        let (done, release) = arm(&backend.gates.watch_done);
        drop(views.pop().unwrap());
        bounded(done).await.unwrap();
        assert_eq!(
            lock(&backend.observers).len(),
            remaining,
            "只在最后一个观察者退出时移除共享缓存"
        );
        release.send(()).unwrap();
    }
    let mut provider = Provider::start().await;
    provider.release.send_replace(false);
    config(dir.path(), &provider.endpoint);
    let input = create_request(&client, dir.path()).await;
    client.submit(input.clone()).await.unwrap();
    bounded(&mut provider.arrived).await.unwrap();
    let mut view = client.watch(input.session).await.unwrap();
    // 初始批次结束后断开，Owner 仍须持有这一会话的观察状态。
    loop {
        if matches!(view.next().await.unwrap().unwrap(), ViewUpdate::Entry { entry } if entry.node() == NodeId(2))
        {
            break;
        }
    }
    let (done, release) = arm(&backend.gates.watch_done);
    drop(view);
    bounded(done).await.unwrap();
    assert_eq!(
        lock(&backend.observers).len(),
        1,
        "Owner 仍在时保留同一个观察状态"
    );
    release.send(()).unwrap();
    provider.release.send_replace(true);
    idle(&backend).await;
    assert_eq!(provider.finish().await.len(), 1);
    assert!(
        lock(&backend.observers).is_empty(),
        "Owner 也退出后必须回收缓存"
    );
    finish(&backend, task).await;
}
#[tokio::test]
async fn commit_fault_drains_real_reader_and_owner_before_reopening_the_store() {
    use lattice_session::store::*;
    for fault in [
        TestCommitFault::Poisoned,
        TestCommitFault::UnknownBefore,
        TestCommitFault::UnknownAfter,
    ] {
        let (dir, backend, client, task) = backend().await;
        let sid = new_session_id();
        let first = seed_round(&backend, sid, true).await;
        let mut provider = Provider::start().await;
        provider.release.send_replace(false);
        config(dir.path(), &provider.endpoint);
        let second = create_request(&client, dir.path()).await;
        client.submit(second.clone()).await.unwrap();
        bounded(&mut provider.arrived).await.unwrap();
        let owner = lock(&backend.admission).turns[&second.turn_id].clone();
        let (retiring, release_owner) = arm(&backend.gates.retire);
        let api = backend.open().await.unwrap();
        let shutdown = api.shutdown();
        let (entered_tx, entered) = oneshot::channel();
        let (release_read, wait) = std::sync::mpsc::channel();
        let reading = api.clone();
        let reader = tokio::spawn(async move {
            reading
                .read(move |r| {
                    entered_tx.send(()).unwrap();
                    wait.recv_timeout(Duration::from_secs(10)).unwrap();
                    r.list(None, 10)
                })
                .await
        });
        entered.await.unwrap();
        let error = api
            .blocking(move |api| {
                let guard = api.store.try_lock_session(sid)?;
                Ok(with_test_commit_fault(fault, || {
                    api.store.close_step(
                        &guard,
                        CloseStep::StopBetweenRequests {
                            turn_id: first,
                            cause: StopCause::Cancelled,
                        },
                        chrono::Utc::now(),
                    )
                })?)
            })
            .await
            .unwrap_err();
        assert!(
            matches!(error, crate::ApiError::Store(ref e) if matches!(e.as_ref(), StoreError::CommitFailed { .. }))
        );
        assert!(shutdown.load(Ordering::Acquire));
        assert!(!owner.execution_cancel.load(Ordering::Acquire));
        assert!(api.with_execution_start(|| ()).is_err());
        drop(api);
        bounded(retiring).await.unwrap();
        assert!(owner.done.borrow().is_none());
        release_read.send(()).unwrap();
        bounded(reader).await.unwrap().unwrap();
        assert_eq!(
            backend.stores.ready_generation(),
            None,
            "Owner 还在监督退休窗口，库代不能提前重开"
        );
        release_owner.send(()).unwrap();
        idle(&backend).await;
        let reopened = bounded(async {
            let mut changed = backend.stores.subscribe();
            loop {
                if let Ok(api) = backend.open().await {
                    break api;
                }
                changed.changed().await.unwrap();
            }
        })
        .await;
        assert_eq!(backend.stores.ready_generation(), Some(1));
        assert!(!reopened.shutdown().load(Ordering::Acquire));
        let first = reopened
            .read(move |r| r.turn_detail(sid, first))
            .await
            .unwrap();
        assert_eq!(
            first.summary.progress.is_closed(),
            matches!(fault, TestCommitFault::UnknownAfter)
        );
        let second_id = second.turn_id;
        let second_sid = second.session;
        let second = reopened
            .read(move |r| r.turn_detail(second_sid, second_id))
            .await
            .unwrap();
        assert!(matches!(second.steps[0].progress, StepProgress::Requesting));
        assert_eq!(
            lock(&backend.admission)
                .diagnostics
                .get(&second_id)
                .unwrap()
                .kind,
            LocalErrorKind::StoreUnavailable
        );
        drop(reopened);
        provider.release.send_replace(true);
        assert_eq!(provider.finish().await.len(), 1);
        finish(&backend, task).await;
    }
}
#[tokio::test]
async fn write_disabled_before_acceptance_is_rejected_not_unknown() {
    use lattice_session::store::*;
    // 分别停在准备前与接受事务之前；两处都尚未进入接受事务，禁写后必须确定性拒绝。
    for before_accept in [true, false] {
        let (dir, backend, client, task) = backend().await;
        let other = new_session_id();
        let seeded = seed_round(&backend, other, true).await;
        let provider = Provider::start().await;
        config(dir.path(), &provider.endpoint);
        let input = create_request(&client, dir.path()).await;
        let sink = backend.observer(input.session);
        let (paused, resume) = arm(if before_accept {
            &sink.before_accept
        } else {
            &backend.gates.prepare
        });
        let submitting = {
            let client = client.clone();
            let input = input.clone();
            tokio::spawn(async move { client.submit(input).await })
        };
        bounded(paused).await.unwrap();
        // 另一个会话的提交失败使库转为禁写；持有租约，恢复在本请求回复前不能重开。
        let api = backend.open().await.unwrap();
        let error = api
            .blocking(move |api| {
                let guard = api.store.try_lock_session(other)?;
                Ok(with_test_commit_fault(
                    TestCommitFault::UnknownBefore,
                    || {
                        api.store.close_step(
                            &guard,
                            CloseStep::StopBetweenRequests {
                                turn_id: seeded,
                                cause: StopCause::Cancelled,
                            },
                            chrono::Utc::now(),
                        )
                    },
                )?)
            })
            .await
            .unwrap_err();
        assert!(
            matches!(error, crate::ApiError::Store(ref e) if matches!(e.as_ref(), StoreError::CommitFailed { .. }))
        );
        resume.send(()).unwrap();
        let rejected = bounded(submitting).await.unwrap().unwrap_err();
        assert!(
            rejected
                .recovery
                .as_ref()
                .is_some_and(|r| r.cause == RecoveryCause::WriteDisabled),
            "事务前因禁写被拒，确定未被接受，不能回复结果不明：{rejected:?}"
        );
        drop(api);
        assert_eq!(provider.finish().await.len(), 0);
        finish(&backend, task).await;
    }
}
#[tokio::test]
async fn store_creation_failure_rejects_the_first_configure() {
    let (dir, backend, client, task) = backend().await;
    // 建库的父目录同步失败，库是否持久不明；尚未写入会话记录。
    *lock(&backend.stores.creation_sync_probe) = Some(Arc::new(|| true));
    config(dir.path(), "http://127.0.0.1:1/v1");
    let error = client
        .configure(configure_new(dir.path(), new_session_id()))
        .await
        .unwrap_err();
    assert!(
        error
            .recovery
            .as_ref()
            .is_some_and(|r| r.cause == RecoveryCause::CommitFailed),
        "{error:?}"
    );
    finish(&backend, task).await;
}
#[tokio::test]
async fn different_sessions_overlap_and_keep_their_frozen_configuration() {
    let (dir, backend, client, task) = backend().await;
    let mut first_provider = Provider::start().await;
    first_provider.release.send_replace(false);
    let mut second_provider = Provider::start().await;
    second_provider.release.send_replace(false);
    let profile_path = dir.path().join(".lattice/agents/local-test/agent.toml");
    config(dir.path(), &first_provider.endpoint);
    let source = std::fs::read_to_string(&profile_path).unwrap();
    std::fs::write(
        &profile_path,
        source.replace("system = 'test'", "system = 'first frozen prompt'"),
    )
    .unwrap();
    let first = create_request(&client, dir.path()).await;
    client.submit(first.clone()).await.unwrap();
    bounded(&mut first_provider.arrived).await.unwrap();
    config(dir.path(), &second_provider.endpoint);
    std::fs::write(
        &profile_path,
        source.replace("system = 'test'", "system = 'second frozen prompt'"),
    )
    .unwrap();
    let second = create_request(&client, dir.path()).await;
    client.submit(second.clone()).await.unwrap();
    bounded(&mut second_provider.arrived).await.unwrap();
    assert_eq!(
        client
            .list_sessions()
            .await
            .unwrap()
            .iter()
            .filter(|r| r.owner)
            .count(),
        2
    );
    first_provider.release.send_replace(true);
    second_provider.release.send_replace(true);
    idle(&backend).await;
    let api = backend.open().await.unwrap();
    let first_sid = first.session;
    let first_tid = first.turn_id;
    let first_config = api
        .read(move |r| r.turn_config(first_sid, first_tid))
        .await
        .unwrap();
    let second_sid = second.session;
    let second_tid = second.turn_id;
    let second_config = api
        .read(move |r| r.turn_config(second_sid, second_tid))
        .await
        .unwrap();
    assert_eq!(
        first_config.system_prompt.as_deref(),
        Some("first frozen prompt")
    );
    assert_eq!(
        second_config.system_prompt.as_deref(),
        Some("second frozen prompt")
    );
    drop(api);
    let first_requests = first_provider.finish().await;
    let second_requests = second_provider.finish().await;
    assert_eq!(first_requests.len(), 1);
    assert_eq!(second_requests.len(), 1);
    assert!(
        first_requests[0]
            .to_string()
            .contains("first frozen prompt")
    );
    assert!(
        !first_requests[0]
            .to_string()
            .contains("second frozen prompt")
    );
    assert!(
        second_requests[0]
            .to_string()
            .contains("second frozen prompt")
    );
    finish(&backend, task).await;
}
#[tokio::test]
async fn existing_orphan_and_root_use_session_settings_without_mutating_old_nodes() {
    let (dir, backend, client, task) = backend().await;
    let sid = new_session_id();
    let old = seed_round(&backend, sid, true).await;
    let before = client.turn(old).await.unwrap();
    let provider = Provider::start().await;
    let configure_source = || {
        config(dir.path(), &provider.endpoint);
        let path = dir.path().join(".lattice/agents/local-test/agent.toml");
        let source = std::fs::read_to_string(&path).unwrap();
        std::fs::write(
            path,
            source.replace("local/local-test", "local/source-only-model"),
        )
        .unwrap();
    };
    let next = raw_request(dir.path(), sid);
    assert!(matches!(
        client
            .submit({
                configure_source();
                next.clone()
            })
            .await
            .unwrap(),
        Acceptance::Accepted { .. }
    ));
    idle(&backend).await;
    assert_eq!(client.turn(old).await.unwrap(), before);
    client
        .rewind(sid, NodeId(0), dir.path().into())
        .await
        .unwrap();
    let at_root = client.session_head(sid).await.unwrap();
    assert_eq!(at_root.agent, "local-test");
    assert_eq!(at_root.model.as_ref().map(|m| m.as_str()), Some("test"));
    assert_eq!(at_root.turn_id, None);
    let mut root_request = next;
    root_request.turn_id = new_turn_id();
    client
        .submit({
            configure_source();
            root_request.clone()
        })
        .await
        .unwrap();
    idle(&backend).await;
    assert_eq!(client.session_head(sid).await.unwrap().agent, "local-test");
    configure_source();
    client
        .configure(ConfigureRequest {
            session: ConfigureSession::Existing(sid),
            workspace: dir.path().into(),
            agent: Some(AgentName::parse("local-test").unwrap()),
            model: None,
            effort: None,
        })
        .await
        .unwrap();
    root_request.turn_id = new_turn_id();
    client.submit(root_request).await.unwrap();
    idle(&backend).await;
    assert_eq!(client.turn(old).await.unwrap(), before);
    let captured = provider.finish().await;
    assert_eq!(captured.len(), 3);
    assert_eq!(captured[0]["model"], "test");
    assert_eq!(captured[1]["model"], "test");
    assert_eq!(captured[2]["model"], "source-only-model");
    finish(&backend, task).await;
}
#[tokio::test]
async fn preparation_disconnect_and_forwarding_panic_keep_independent_cancelled_completion() {
    for forwarding_panic in [false, true] {
        let (dir, backend, client, task) = backend().await;
        let mut provider = Provider::start().await;
        provider.release.send_replace(false);
        config(dir.path(), &provider.endpoint);
        let input = create_request(&client, dir.path()).await;
        let (entered, release) = arm(if forwarding_panic {
            &backend.gates.forward_panic
        } else {
            &backend.gates.prepare
        });
        let submit = {
            let client = client.clone();
            let input = input.clone();
            tokio::spawn(async move { client.submit(input).await })
        };
        let mut release = Some(release);
        bounded(entered).await.unwrap();
        if forwarding_panic {
            bounded(&mut provider.arrived).await.unwrap();
            release.take().unwrap().send(()).unwrap();
            assert!(matches!(
                bounded(submit).await.unwrap().unwrap(),
                Acceptance::ConfirmationUnknown { turn_id, session_id }
                    if turn_id == input.turn_id && session_id == input.session
            ));
        } else {
            submit.abort();
            assert!(submit.await.unwrap_err().is_cancelled());
        }
        let stop = {
            let client = client.clone();
            tokio::spawn(async move { client.stop().await })
        };
        bounded(async {
            while !backend.status().stopping {
                tokio::task::yield_now().await;
            }
        })
        .await;
        assert!(!stop.is_finished());
        assert!(client.cancel(input.turn_id).await.unwrap());
        if let Some(release) = release {
            release.send(()).unwrap();
        }
        let status = bounded(stop).await.unwrap().unwrap();
        assert_eq!(status.owners, 0);
        assert_eq!(status.unconfirmed_operations, usize::from(forwarding_panic));
        if forwarding_panic {
            let TurnLookup::Accepted { turn } = backend.lookup(input.turn_id).await.unwrap() else {
                panic!("accepted")
            };
            assert_eq!(turn.summary.outcome, PublicTurnOutcome::Cancelled);
            assert!(turn.summary.progress.is_closed());
        } else {
            assert_eq!(
                backend.lookup(input.turn_id).await.unwrap_err().kind,
                LocalErrorKind::NotFound
            );
        }
        provider.release.send_replace(true);
        assert_eq!(provider.finish().await.len(), usize::from(forwarding_panic));
        finish(&backend, task).await;
    }
}

#[tokio::test]
async fn session_head_and_cancel_do_not_need_observation_permits() {
    let (dir, backend, client, task) = backend().await;
    let mut provider = Provider::start().await;
    provider.release.send_replace(false);
    config(dir.path(), &provider.endpoint);
    let input = create_request(&client, dir.path()).await;
    client.submit(input.clone()).await.unwrap();
    bounded(&mut provider.arrived).await.unwrap();
    let permits = backend.subscriptions.acquire_many(32).await.unwrap();
    let head = client.session_head(input.session).await.unwrap();
    assert_eq!(head.turn_id, Some(input.turn_id));
    assert!(head.owner);
    assert!(!head.path_replaced);
    assert_eq!(
        client.cancel_session(input.session).await.unwrap(),
        (Some(input.turn_id), true)
    );
    assert_eq!(
        client
            .session_head(new_session_id())
            .await
            .unwrap_err()
            .kind,
        LocalErrorKind::NotFound
    );
    idle(&backend).await;
    assert!(!client.session_head(input.session).await.unwrap().owner);
    assert_eq!(
        client.cancel_session(input.session).await.unwrap(),
        (None, false)
    );
    drop(permits);
    provider.release.send_replace(true);
    assert_eq!(provider.finish().await.len(), 1);
    finish(&backend, task).await;
}

#[tokio::test]
async fn dispatch_submit_without_submitter_is_an_internal_error() {
    let (dir, backend, client, task) = backend().await;
    client.status().await.unwrap();
    let result = backend
        .dispatch(Request::Submit {
            request: raw_request(dir.path(), new_session_id()),
        })
        .await;
    assert!(matches!(
        result,
        Err(LocalError {
            kind: LocalErrorKind::Internal,
            ..
        })
    ));
    assert_eq!(backend.status().owners, 0);
    finish(&backend, task).await;
}

#[tokio::test]
async fn invalid_request_is_rejected_before_claiming_session() {
    let (dir, backend, client, task) = backend().await;
    config(dir.path(), "http://127.0.0.1:1/v1");
    let mut input = create_request(&client, dir.path()).await;
    input.input = "x".repeat(2 * 1024 * 1024 + 1);
    let sid = input.session;
    let (entered, release) = arm(&backend.gates.registered);
    let submitting = {
        let client = client.clone();
        invalid(dir.path());
        tokio::spawn(async move { client.submit(input).await })
    };
    tokio::select! {
        result = submitting => assert_eq!(result.unwrap().unwrap_err().kind, LocalErrorKind::BadRequest),
        _ = entered => {
            release.send(()).unwrap();
            panic!("无效输入占住了 Session 槽位");
        }
    }
    assert!(!lock(&backend.admission).sessions.contains_key(&sid));
    finish(&backend, task).await;
}

type GateSlot = fn(&Gates) -> &Mutex<Option<Gate>>;
type GatedServe = (
    tempfile::TempDir,
    StateLayout,
    LocalClient,
    oneshot::Receiver<()>,
    oneshot::Sender<()>,
    tokio::task::JoinHandle<Result<DrainOutcome>>,
);
struct ConfiguredServe {
    dir: tempfile::TempDir,
    layout: StateLayout,
    backend: Arc<Backend>,
    client: LocalClient,
    stop: tokio::sync::mpsc::Sender<()>,
    serving: tokio::task::JoinHandle<Result<DrainOutcome>>,
}

async fn serve_configured(
    drain_timeout: Duration,
    metadata: Option<Arc<super::super::metadata::MetadataRefresh>>,
    slot: Option<GateSlot>,
) -> (
    ConfiguredServe,
    Option<(oneshot::Receiver<()>, oneshot::Sender<()>)>,
) {
    let dir = tempfile::tempdir().unwrap();
    let layout = StateLayout::in_state_root(dir.path());
    let (stop_tx, stop_rx) = tokio::sync::mpsc::channel(4);
    let (ready_tx, ready_rx) = oneshot::channel();
    let (serving_layout, backend_layout, home) =
        (layout.clone(), layout.clone(), dir.path().to_path_buf());
    let serving = tokio::spawn(async move {
        let exit = serve_with_backend(serving_layout, drain_timeout, stop_rx, move || {
            let mut backend = Backend::with_tools(
                backend_layout,
                Some(home),
                crate::turn::default_tool_executor(),
            );
            Arc::get_mut(&mut backend)
                .expect("unshared test backend")
                .metadata =
                metadata.unwrap_or_else(super::super::metadata::MetadataRefresh::without_network);
            lock(&backend.admission).drain_timeout = drain_timeout;
            let armed = slot.map(|slot| arm(slot(&backend.gates)));
            let _ = ready_tx.send((backend.clone(), armed));
            backend
        })
        .await?;
        let ServeExit {
            drain,
            failure,
            instance,
            ..
        } = exit;
        drop(instance);
        match failure {
            Some(error) => Err(error),
            None => Ok(drain),
        }
    });
    let (backend, armed) = bounded(ready_rx).await.unwrap();
    let client = LocalClient::new(layout.clone());
    (
        ConfiguredServe {
            dir,
            layout,
            backend,
            client,
            stop: stop_tx,
            serving,
        },
        armed,
    )
}

/// 以正式 serve 收尾路径起后台，构造 Backend 时在指定退场窗口装 gate。
async fn serve_gated(slot: GateSlot) -> GatedServe {
    let (configured, armed) = serve_configured(Duration::from_secs(60), None, Some(slot)).await;
    let (entry, release) = armed.expect("armed gate");
    (
        configured.dir,
        configured.layout,
        configured.client,
        entry,
        release,
        configured.serving,
    )
}

/// 测试宿主在 `serve` 返回后继续持有 `InstanceLock`，直至收到放行信号再释放。
async fn serve_holding_lock() -> GatedServe {
    let dir = tempfile::tempdir().unwrap();
    let layout = StateLayout::in_state_root(dir.path());
    let (_stop_tx, stop_rx) = tokio::sync::mpsc::channel(1);
    let (entered_tx, entry) = oneshot::channel();
    let (release, resume) = oneshot::channel();
    let (ready_tx, ready_rx) = oneshot::channel();
    let (serving_layout, backend_layout, home) =
        (layout.clone(), layout.clone(), dir.path().to_path_buf());
    let serving = tokio::spawn(async move {
        let exit = serve_with_backend(
            serving_layout,
            Duration::from_secs(60),
            stop_rx,
            move || {
                let mut backend = Backend::with_tools(
                    backend_layout,
                    Some(home),
                    crate::turn::default_tool_executor(),
                );
                Arc::get_mut(&mut backend)
                    .expect("unshared test backend")
                    .metadata = super::super::metadata::MetadataRefresh::without_network();
                let _ = ready_tx.send(());
                backend
            },
        )
        .await?;
        let ServeExit {
            drain,
            failure,
            instance,
            ..
        } = exit;
        let _ = entered_tx.send(());
        bounded(resume).await.unwrap();
        drop(instance);
        match failure {
            Some(error) => Err(error),
            None => Ok(drain),
        }
    });
    bounded(ready_rx).await.unwrap();
    let client = LocalClient::new(layout.clone());
    (dir, layout, client, entry, release, serving)
}

/// 另开文件描述做非阻塞 flock；同进程内的后台锁同样会冲突。
fn owner_lock_free(layout: &lattice_session::StateLayout) -> bool {
    use rustix::fs::{FlockOperation, flock};
    let file = std::fs::File::open(layout.lattice_root().join("local/owner.lock")).unwrap();
    flock(&file, FlockOperation::NonBlockingLockExclusive).is_ok()
}
/// stop 回执之后后台停在退场窗口：stop 不能先返回，放行后返回时实例锁已释放、socket 已撤下。
async fn stop_waits_for_departure(gated: GatedServe) {
    let (_dir, layout, client, entry, release, serving) = gated;
    let stop = {
        let client = client.clone();
        tokio::spawn(async move { client.stop().await })
    };
    bounded(entry).await.unwrap();
    assert!(!owner_lock_free(&layout));
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!stop.is_finished(), "实例锁仍被持有时 stop 已返回");
    release.send(()).unwrap();
    let status = bounded(stop).await.unwrap().unwrap();
    assert_eq!(status.owners, 0);
    assert!(owner_lock_free(&layout), "stop 返回时实例锁尚未释放");
    assert!(!layout.lattice_root().join("local/api.sock").exists());
    assert_eq!(
        bounded(serving).await.unwrap().unwrap(),
        DrainOutcome::Completed
    );
}
#[tokio::test]
async fn stop_waits_while_listener_close_is_delayed() {
    stop_waits_for_departure(serve_gated(|gates| &gates.listener_close).await).await;
}
#[tokio::test]
async fn stop_waits_while_lock_release_is_delayed() {
    stop_waits_for_departure(serve_holding_lock().await).await;
}
#[tokio::test]
async fn stop_reports_timeout_when_lock_is_never_released() {
    let (_dir, layout, client, entry, release, serving) = serve_holding_lock().await;
    let started = std::time::Instant::now();
    let error = bounded(client.stop_within(Duration::from_millis(200)))
        .await
        .unwrap_err();
    assert_eq!(error.kind, LocalErrorKind::Stopping);
    assert!(error.message.contains("lattice daemon status"));
    assert!(started.elapsed() < Duration::from_secs(5));
    // 超时报错时测试宿主仍持有 ServeExit.instance，说明报错不是因为锁恰好放开。
    bounded(entry).await.unwrap();
    assert!(!layout.lattice_root().join("local/api.sock").exists());
    assert!(!owner_lock_free(&layout));
    release.send(()).unwrap();
    assert_eq!(
        bounded(serving).await.unwrap().unwrap(),
        DrainOutcome::Completed
    );
    assert!(owner_lock_free(&layout));
}
#[tokio::test]
async fn symlinked_workspace_shares_canonical_project_binding() {
    let (dir, backend, client, task) = backend().await;
    let provider = Provider::start().await;
    config(dir.path(), &provider.endpoint);
    let first = create_request(&client, dir.path()).await;
    let sid = first.session;
    client.submit(first).await.expect("在原始目录提交首轮");
    idle(&backend).await;

    let link_parent = tempfile::tempdir().expect("创建符号链接父目录");
    let symlink_ws = link_parent.path().join("project-symlink");
    std::os::unix::fs::symlink(dir.path(), &symlink_ws).expect("创建指向项目根的符号链接");
    let original_root = crate::LocalWorkspaceRoot::from_path(dir.path()).expect("解析原始工作区");
    let via_symlink =
        crate::LocalWorkspaceRoot::from_path(&symlink_ws).expect("经符号链接解析工作区");
    assert_eq!(
        via_symlink.project_path(),
        original_root.project_path(),
        "经符号链接解析得到的规范项目路径必须与原始项目一致"
    );

    let second = raw_request(via_symlink.canonical_path(), sid);
    client
        .submit(second)
        .await
        .expect("经符号链接续写同一会话应成功");
    idle(&backend).await;

    let rewound = client
        .rewind(sid, NodeId(0), via_symlink.canonical_path().to_path_buf())
        .await
        .expect("经符号链接在同一会话执行 rewind 应成功");
    assert_eq!(rewound.current, NodeId(0), "rewind 后当前节点应回到根");
    assert_eq!(
        rewound.project_path,
        original_root.project_path(),
        "rewind 返回的会话记录应保留同一规范项目路径"
    );
    assert_eq!(provider.finish().await.len(), 2);
    finish(&backend, task).await;
}

#[tokio::test]
async fn symlinked_workspace_path_is_rejected_without_side_effects() {
    fn assert_not_canonical<T: std::fmt::Debug>(
        result: std::result::Result<T, LocalError>,
        operation: &str,
    ) {
        let error = result.expect_err(&format!("{operation} 传入符号链接路径应被拒绝"));
        assert_eq!(
            error.kind,
            LocalErrorKind::BadRequest,
            "{operation} 应返回 BadRequest：{error}"
        );
        assert_eq!(
            error.message, "workspace must be canonical",
            "{operation} 应在规范路径核对处被拒绝：{error}"
        );
    }

    let (dir, backend, client, task) = backend().await;
    let provider = Provider::start().await;
    config(dir.path(), &provider.endpoint);
    let link_parent = tempfile::tempdir().expect("创建符号链接父目录");
    let symlink_ws = link_parent.path().join("project-symlink");
    std::os::unix::fs::symlink(dir.path(), &symlink_ws).expect("创建指向项目根的符号链接");

    let rejected = new_session_id();
    assert_not_canonical(
        client.configure(configure_new(&symlink_ws, rejected)).await,
        "configure(New)",
    );
    let missing = client
        .session_head(rejected)
        .await
        .expect_err("被拒绝的 configure(New) 不应创建草稿会话");
    assert_eq!(missing.kind, LocalErrorKind::NotFound, "{missing}");

    let first = create_request(&client, dir.path()).await;
    let sid = first.session;
    client.submit(first).await.expect("在规范路径提交首轮");
    idle(&backend).await;
    let before = client
        .session_head(sid)
        .await
        .expect("读取首轮之后的会话头");

    assert_not_canonical(
        client
            .configure(ConfigureRequest {
                session: ConfigureSession::Existing(sid),
                workspace: symlink_ws.clone(),
                agent: Some(AgentName::parse("local-test").unwrap()),
                model: None,
                effort: None,
            })
            .await,
        "configure(Existing)",
    );
    assert_not_canonical(client.submit(raw_request(&symlink_ws, sid)).await, "submit");
    assert_not_canonical(
        client.rewind(sid, NodeId(0), symlink_ws.clone()).await,
        "rewind",
    );

    let after = client
        .session_head(sid)
        .await
        .expect("读取被拒请求之后的会话头");
    assert_eq!(
        after.position, before.position,
        "被拒绝的 rewind 不应移动当前位置"
    );
    assert_eq!(
        after.settings_version, before.settings_version,
        "被拒绝的 configure 不应改动会话设置"
    );
    assert_eq!(
        after.turn_id, before.turn_id,
        "被拒绝的 submit 不应开始新的一轮"
    );
    assert_eq!(provider.finish().await.len(), 1, "只有首轮向模型发出请求");
    finish(&backend, task).await;
}

#[tokio::test]
async fn stop_request_queued_before_serve_drains_immediately_and_removes_socket() {
    let dir = tempfile::tempdir().unwrap();
    let layout = StateLayout::in_state_root(dir.path());
    let (stop_tx, stop_rx) = tokio::sync::mpsc::channel(1);
    stop_tx.send(()).await.expect("预先向停机通道写入请求");

    let exit = bounded(super::serve_for_test(
        layout.clone(),
        Some(dir.path().into()),
        Duration::from_secs(60),
        stop_rx,
    ))
    .await
    .expect("就绪前收到停机请求的 serve 应正常返回");

    assert_eq!(
        exit.drain,
        DrainOutcome::Completed,
        "无在途工作时应排空完成"
    );
    assert!(
        exit.failure.is_none(),
        "不应产生运行期失败: {:?}",
        exit.failure
    );
    assert!(
        !layout.lattice_root().join("local/api.sock").exists(),
        "serve 返回前 socket 应已撤下"
    );
    assert!(
        !owner_lock_free(&layout),
        "宿主释放 ServeExit.instance 前实例锁仍应持有"
    );
    drop(exit.instance);
    assert!(owner_lock_free(&layout), "释放实例持有对象后锁应可取得");
}

#[tokio::test]
async fn drain_timeout_disables_writes_before_inflight_turn_can_record_terminal_outcome() {
    let (configured, _) = serve_configured(Duration::from_millis(300), None, None).await;
    let ConfiguredServe {
        dir,
        layout,
        backend,
        client,
        stop,
        serving,
    } = configured;
    let mut provider = Provider::start().await;
    provider.release.send_replace(false);
    config(dir.path(), &provider.endpoint);

    let input = create_request(&client, dir.path()).await;
    client.submit(input.clone()).await.expect("提交在途轮次");
    bounded(&mut provider.arrived).await.unwrap();

    stop.send(()).await.expect("发送停机请求触发排空超时");
    let outcome = bounded(serving).await.unwrap().expect("serve 收尾完成");
    assert_eq!(outcome, DrainOutcome::TimedOut, "超过 300ms 应返回排空超时");
    assert_eq!(
        backend.status().owners,
        1,
        "放行模型前在途轮次的 Owner 仍应存活"
    );

    provider.release.send_replace(true);
    assert_eq!(provider.finish().await.len(), 1);
    idle(&backend).await;
    assert_eq!(backend.status().owners, 0, "在途轮次的 Owner 应实际退休");

    drop(backend);
    let reopened =
        lattice_session::store::SessionStore::open(&layout).expect("释放全部库句柄后重开会话库");
    let read = reopened.begin_read().expect("开启只读事务");
    let log = read.log(input.session).expect("读取会话日志");
    assert_eq!(
        log.unclosed_turn,
        Some(input.turn_id),
        "停写后实际退休的轮次在库中仍应保持未关闭"
    );
    let detail = read
        .turn_detail(input.session, input.turn_id)
        .expect("读取轮次详情");
    assert!(
        !detail.summary.progress.is_closed(),
        "停写后模型完成的轮次不应写入持久结局"
    );
}

#[tokio::test]
async fn ipc_stop_replies_after_drain_timeout_and_before_metadata_tail() {
    let (configured, armed) = serve_configured(
        Duration::from_millis(300),
        None,
        Some(|gates| &gates.metadata_drain),
    )
    .await;
    let (meta_entered, meta_release) = armed.expect("armed metadata_drain gate");
    let ConfiguredServe {
        dir,
        layout,
        backend,
        client,
        serving,
        ..
    } = configured;

    let mut provider = Provider::start().await;
    provider.release.send_replace(false);
    config(dir.path(), &provider.endpoint);
    let input = create_request(&client, dir.path()).await;
    client.submit(input).await.expect("提交阻塞轮次");
    bounded(&mut provider.arrived).await.unwrap();

    let (status, departure) = bounded(client.request_stop())
        .await
        .expect("排空超时后、元数据收尾完成前 IPC stop 回执应到达");
    bounded(meta_entered)
        .await
        .expect("后台应进入元数据收尾尾段并停在门控处");
    assert!(status.stopping, "stop 回执应带 stopping=true");
    assert!(
        status.unconfirmed_operations >= 1,
        "排空超时后 stop 回执的 unconfirmed_operations 应不小于 1"
    );
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(
        !serving.is_finished() && !owner_lock_free(&layout),
        "元数据收尾门控未放行时 serve 不应已退出，证明回执先于元数据收尾到达"
    );

    let (second_status, _) = tokio::time::timeout(Duration::from_secs(2), client.request_stop())
        .await
        .expect("排空超时进入元数据尾段后经真实 socket 的重复 stop 不应卡住")
        .expect("重复 IPC stop 请求应返回保存的回执");
    assert_eq!(
        second_status, status,
        "第 2 步结束后重复的 stop 应共用同一份 DaemonStatus"
    );

    meta_release.send(()).unwrap();
    bounded(departure.wait(Duration::from_secs(5)))
        .await
        .expect("放行元数据门控后实例锁应释放");
    assert_eq!(
        bounded(serving).await.unwrap().unwrap(),
        DrainOutcome::TimedOut
    );

    provider.release.send_replace(true);
    let _ = provider.finish().await;
    idle(&backend).await;
}

#[tokio::test]
async fn duplicate_ipc_stop_and_status_succeed_over_socket_during_metadata_tail() {
    let (_dir, _layout, client, meta_entered, meta_release, serving) =
        serve_gated(|gates| &gates.metadata_drain).await;

    let (first_status, departure) = bounded(client.request_stop())
        .await
        .expect("首次 IPC stop 应在排空完成后返回回执");
    bounded(meta_entered)
        .await
        .expect("后台应进入元数据收尾尾段并停在门控处");
    assert!(first_status.stopping, "首次 stop 回执应带 stopping=true");

    let (second_stop, status_reply) = tokio::time::timeout(Duration::from_secs(2), async {
        tokio::join!(client.request_stop(), client.status())
    })
    .await
    .expect("元数据收尾期间经真实 socket 的重复 stop 与 status 不应被挡在未消费连接队列中");
    let (second_status, _) = second_stop.expect("元数据尾段期间经真实 socket 的重复 stop 应成功");
    let current_status = status_reply.expect("元数据尾段期间经真实 socket 的 status 应成功");
    assert_eq!(
        second_status, first_status,
        "第二次 stop 的回执应与第一次保存的 DaemonStatus 相同"
    );
    assert!(
        current_status.stopping,
        "元数据尾段期间 status 回答应带 stopping=true"
    );

    meta_release.send(()).unwrap();
    bounded(departure.wait(Duration::from_secs(5)))
        .await
        .expect("放行元数据门控后 socket 应撤下且实例锁应释放");
    assert_eq!(
        bounded(serving).await.unwrap().expect("serve 应正常返回"),
        DrainOutcome::Completed
    );
}

#[tokio::test]
async fn final_release_and_socket_removal_share_the_shutdown_deadline() {
    let dir = tempfile::tempdir().unwrap();
    let layout = StateLayout::in_state_root(dir.path());
    let (stop_tx, stop_rx) = tokio::sync::mpsc::channel(4);
    let (ready_tx, ready_rx) = oneshot::channel();
    let (serving_layout, backend_layout, home) =
        (layout.clone(), layout.clone(), dir.path().to_path_buf());
    let serving = tokio::spawn(serve_with_backend(
        serving_layout,
        Duration::from_secs(60),
        stop_rx,
        move || {
            let mut backend = Backend::with_tools(
                backend_layout,
                Some(home),
                crate::turn::default_tool_executor(),
            );
            Arc::get_mut(&mut backend)
                .expect("unshared test backend")
                .metadata = super::super::metadata::MetadataRefresh::without_network();
            let armed = arm(&backend.gates.final_release);
            let _ = ready_tx.send(armed);
            backend
        },
    ));
    let (release_entered, release_resume) = bounded(ready_rx).await.unwrap();
    let client = LocalClient::new(layout.clone());
    config(dir.path(), "http://127.0.0.1:1/v1");
    let workspace = crate::LocalWorkspaceRoot::from_path(dir.path())
        .unwrap()
        .canonical_path()
        .to_path_buf();
    client
        .configure(configure_new(&workspace, new_session_id()))
        .await
        .expect("先 configure 会话以打开会话库");

    stop_tx.send(()).await.expect("发送停机信号");
    bounded(release_entered)
        .await
        .expect("最终释放任务应进入门控");
    // 释放被挡住时，serve 在第 6 步的 5 秒期限到达后返回，不随释放无限等待。
    let exit = tokio::time::timeout(Duration::from_secs(8), serving)
        .await
        .expect("最终释放被挡住时 serve 应在第 6 步的期限到达后返回")
        .unwrap()
        .expect("serve 应正常完成收尾");
    assert_eq!(exit.drain, DrainOutcome::Completed);
    assert!(
        exit.failure.is_none(),
        "最终释放超过期限不算运行错误: {:?}",
        exit.failure
    );
    assert!(
        exit.shutdown_deadline <= std::time::Instant::now(),
        "serve 应等到第 6 步的期限才放弃等待最终释放"
    );
    assert!(
        !layout.lattice_root().join("local/api.sock").exists(),
        "最终释放被挡住时 api.sock 仍应在期限内撤下"
    );
    assert!(
        lattice_session::store::SessionStore::open(&layout).is_err(),
        "最终释放被挡住时会话库仍由释放任务持有"
    );

    release_resume.send(()).unwrap();
    drop(exit.instance);
    // 放行后释放任务丢掉最后一个后台引用，会话库关闭，可以重新打开。
    let reopened = bounded(async {
        loop {
            match lattice_session::store::SessionStore::open(&layout) {
                Ok(store) => break store,
                Err(_) => tokio::time::sleep(Duration::from_millis(20)).await,
            }
        }
    })
    .await;
    drop(reopened);
}

#[tokio::test(start_paused = true)]
async fn tail_deadlines_bound_idle_connection_and_hung_metadata_at_five_seconds() {
    // 1. 不发请求的空闲连接占着 socket：同步到连接收尾尾段起点后，虚拟时间 5 秒起、10 秒读帧超时前返回。
    {
        let (configured, armed) = serve_configured(
            Duration::from_secs(60),
            None,
            Some(|gates| &gates.listener_close),
        )
        .await;
        let (close_entered, close_release) = armed.expect("armed listener_close gate");
        while configured.client.status().await.is_err() {
            tokio::task::yield_now().await;
        }
        let _idle_stream = lattice_local::test_support::connect(configured.layout.clone())
            .await
            .expect("建立不发请求的连接");
        configured
            .client
            .status()
            .await
            .expect("确认空闲连接已被 accept 循环接纳");

        configured.stop.send(()).await.unwrap();
        close_entered.await.expect("同步到连接收尾尾段起点");
        let started = tokio::time::Instant::now();
        close_release.send(()).unwrap();

        tokio::time::advance(Duration::from_millis(4_900)).await;
        tokio::task::yield_now().await;
        assert!(
            !configured.serving.is_finished(),
            "连接尾段 5 秒期限未到前 serve 不应提前中止空闲连接"
        );

        let outcome = tokio::time::timeout(Duration::from_secs(2), configured.serving)
            .await
            .expect("连接收尾应在 5 秒期限到达后立即返回，不等待 10 秒读帧超时")
            .unwrap()
            .unwrap();
        let elapsed = started.elapsed();
        assert!(
            elapsed >= Duration::from_secs(5) && elapsed < Duration::from_secs(10),
            "连接尾段耗时应在 [5s, 10s) 区间内，实际为 {elapsed:?}"
        );
        assert_eq!(outcome, DrainOutcome::Completed);
        assert!(
            !configured
                .layout
                .lattice_root()
                .join("local/api.sock")
                .exists()
        );
    }

    // 2. 元数据收尾 gate 永不放行：同步到元数据尾段起点后，虚拟时间 5 秒起继续收尾并返回。
    {
        let (configured, armed) = serve_configured(
            Duration::from_secs(60),
            None,
            Some(|gates| &gates.metadata_drain),
        )
        .await;
        let (meta_entered, _never_release) = armed.expect("armed metadata_drain gate");

        configured.stop.send(()).await.unwrap();
        meta_entered.await.expect("同步到元数据收尾尾段起点");
        let started = tokio::time::Instant::now();

        tokio::time::advance(Duration::from_millis(4_900)).await;
        tokio::task::yield_now().await;
        assert!(
            !configured.serving.is_finished(),
            "元数据尾段 5 秒期限未到前 serve 应继续等待"
        );

        let outcome = tokio::time::timeout(Duration::from_secs(2), configured.serving)
            .await
            .expect("元数据收尾应在虚拟时间 5 秒起不再等待")
            .unwrap()
            .unwrap();
        let elapsed = started.elapsed();
        assert!(
            elapsed >= Duration::from_secs(5) && elapsed < Duration::from_secs(10),
            "元数据尾段耗时应在 [5s, 10s) 区间内，实际为 {elapsed:?}"
        );
        assert_eq!(outcome, DrainOutcome::Completed, "尾段到期本身不算排空超时");
        assert!(
            !configured
                .layout
                .lattice_root()
                .join("local/api.sock")
                .exists()
        );
    }
}

#[tokio::test]
async fn drain_timeout_counts_each_active_operation_once_across_error_retirement() {
    // 1. 普通 Owner：排空超时计入 1 次，之后以 WriteDisabled 退休不再重复计数。
    {
        let (configured, _) = serve_configured(Duration::from_millis(100), None, None).await;
        let mut provider = Provider::start().await;
        provider.release.send_replace(false);
        config(configured.dir.path(), &provider.endpoint);
        let input = create_request(&configured.client, configured.dir.path()).await;
        configured.client.submit(input).await.unwrap();
        bounded(&mut provider.arrived).await.unwrap();

        configured.stop.send(()).await.unwrap();
        assert_eq!(
            bounded(configured.serving).await.unwrap().unwrap(),
            DrainOutcome::TimedOut
        );
        assert_eq!(configured.backend.status().unconfirmed_operations, 1);

        provider.release.send_replace(true);
        let _ = provider.finish().await;
        idle(&configured.backend).await;
        assert_eq!(
            configured.backend.status().unconfirmed_operations,
            1,
            "普通 Owner 在排空超时后以错误退休时不能重复计数"
        );
    }

    // 2. 草稿清理扫描阶段（cleanup_scanned gate）：计入 1 次，放行退出后仍为 1。
    {
        let (configured, _) = serve_configured(Duration::from_millis(100), None, None).await;
        let provider = Provider::start().await;
        config(configured.dir.path(), &provider.endpoint);
        let workspace = crate::LocalWorkspaceRoot::from_path(configured.dir.path())
            .unwrap()
            .canonical_path()
            .to_path_buf();
        let expired = new_session_id();
        super::configure_tests::seed_draft(
            &configured.backend,
            &workspace,
            expired,
            chrono::Utc::now() - chrono::Duration::hours(25),
        )
        .await;
        let (scanned_in, scanned_out) = arm(&configured.backend.gates.cleanup_scanned);
        configured
            .client
            .configure(configure_new(&workspace, new_session_id()))
            .await
            .unwrap();
        bounded(scanned_in).await.unwrap();

        configured.stop.send(()).await.unwrap();
        assert_eq!(
            bounded(configured.serving).await.unwrap().unwrap(),
            DrainOutcome::TimedOut
        );
        assert_eq!(configured.backend.status().unconfirmed_operations, 1);

        scanned_out.send(()).unwrap();
        idle(&configured.backend).await;
        assert_eq!(
            configured.backend.status().unconfirmed_operations,
            1,
            "扫描阶段的草稿清理在超时后结束不能改变计数"
        );
        let _ = provider.finish().await;
    }

    // 3. 草稿清理候选 Owner 阶段（cleanup_claimed gate）：清理与候选 Owner 共计 1 次，WriteDisabled 退休后仍为 1。
    {
        let (configured, _) = serve_configured(Duration::from_millis(100), None, None).await;
        let provider = Provider::start().await;
        config(configured.dir.path(), &provider.endpoint);
        let workspace = crate::LocalWorkspaceRoot::from_path(configured.dir.path())
            .unwrap()
            .canonical_path()
            .to_path_buf();
        let expired = new_session_id();
        super::configure_tests::seed_draft(
            &configured.backend,
            &workspace,
            expired,
            chrono::Utc::now() - chrono::Duration::hours(25),
        )
        .await;
        let (claimed_in, claimed_out) = arm(&configured.backend.gates.cleanup_claimed);
        configured
            .client
            .configure(configure_new(&workspace, new_session_id()))
            .await
            .unwrap();
        bounded(claimed_in).await.unwrap();
        assert_eq!(configured.backend.status().owners, 1);
        assert!(configured.backend.has_cleanups());

        configured.stop.send(()).await.unwrap();
        assert_eq!(
            bounded(configured.serving).await.unwrap().unwrap(),
            DrainOutcome::TimedOut
        );
        assert_eq!(
            configured.backend.status().unconfirmed_operations,
            1,
            "草稿清理与它的候选 Owner 只能按一次操作计入"
        );

        claimed_out.send(()).unwrap();
        idle(&configured.backend).await;
        assert_eq!(
            configured.backend.status().unconfirmed_operations,
            1,
            "候选 Owner 因停写报错退休后不能重复计数"
        );
        let _ = provider.finish().await;
    }
}

#[tokio::test]
async fn second_stop_signal_during_drain_triggers_immediate_drain_timeout() {
    for start_via_ipc in [false, true] {
        let (configured, _) = serve_configured(Duration::from_secs(60), None, None).await;
        let mut provider = Provider::start().await;
        provider.release.send_replace(false);
        config(configured.dir.path(), &provider.endpoint);
        let input = create_request(&configured.client, configured.dir.path()).await;
        configured.client.submit(input).await.unwrap();
        bounded(&mut provider.arrived).await.unwrap();

        let ipc_stop = if start_via_ipc {
            let client = configured.client.clone();
            Some(tokio::spawn(async move { client.stop().await }))
        } else {
            configured.stop.send(()).await.unwrap();
            None
        };
        bounded(async {
            while !configured.backend.status().stopping {
                tokio::task::yield_now().await;
            }
        })
        .await;

        // 排空期间重复的 IPC stop 不算信号，不能提前判为超时。
        let dup_client = configured.client.clone();
        let dup_ipc = tokio::spawn(async move { dup_client.stop().await });
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(
            !configured.serving.is_finished(),
            "60 秒排空期内且未收到信号时后台不应提前超时"
        );

        let signal_sent = std::time::Instant::now();
        configured.stop.send(()).await.unwrap();
        let outcome = bounded(configured.serving).await.unwrap().unwrap();
        assert_eq!(
            outcome,
            DrainOutcome::TimedOut,
            "排空期间收到信号应立即按排空超时处理"
        );
        assert!(
            signal_sent.elapsed() < Duration::from_secs(5),
            "收到信号后不应继续等待 60 秒排空期限"
        );
        let dup_status = bounded(dup_ipc).await.unwrap().unwrap();
        assert_eq!(dup_status.unconfirmed_operations, 1);
        if let Some(ipc_stop) = ipc_stop {
            let first_status = bounded(ipc_stop).await.unwrap().unwrap();
            assert_eq!(first_status, dup_status);
        }

        provider.release.send_replace(true);
        let _ = provider.finish().await;
        idle(&configured.backend).await;
    }
}
