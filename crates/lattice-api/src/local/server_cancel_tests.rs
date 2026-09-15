// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use super::*;

type Gate = (oneshot::Sender<()>, oneshot::Receiver<()>);
type StoreGate = (oneshot::Sender<()>, std::sync::mpsc::Receiver<()>);

// 只控制真实准备、置位和监督退场的时序，不提供 Owner 或取消载体注入。
#[derive(Default)]
pub(super) struct Gates {
    pub(super) prepare: Mutex<Option<Gate>>,
    pub(super) retire: Mutex<Option<Gate>>,
    pub(super) retry: Mutex<Option<Gate>>,
    pub(super) forward_panic: Mutex<Option<Gate>>,
    pub(super) supervise: Mutex<Option<Gate>>,
    cancel_store: Mutex<Option<StoreGate>>,
}

impl Gates {
    pub(super) fn pause_cancel_store(&self) {
        let gate = lock(&self.cancel_store).take();
        if let Some((entered, release)) = gate {
            let _ = entered.send(());
            release.recv_timeout(Duration::from_secs(10)).unwrap();
        }
    }
}

fn arm(slot: &Mutex<Option<Gate>>) -> (oneshot::Receiver<()>, oneshot::Sender<()>) {
    let (entered, entry) = oneshot::channel();
    let (release, resume) = oneshot::channel();
    assert!(lock(slot).replace((entered, resume)).is_none());
    (entry, release)
}

pub(super) async fn pause(slot: &Mutex<Option<Gate>>) {
    let gate = lock(slot).take();
    if let Some((entered, release)) = gate {
        let _ = entered.send(());
        let _ = bounded(release).await;
    }
}

pub(super) async fn fail_forwarding(slot: &Mutex<Option<Gate>>) {
    let gate = lock(slot).take();
    if let Some((entered, release)) = gate {
        let _ = entered.send(());
        let _ = bounded(release).await;
        panic!("注入观察转发 panic，核心收尾仍归监督者");
    }
}

async fn bounded<T>(future: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(10), future)
        .await
        .expect("局部交接必须在测试预算内完成")
}

async fn setup() -> (tempfile::TempDir, Arc<Backend>, TurnRequest) {
    let directory = tempfile::tempdir().unwrap();
    let backend = Backend::new(crate::StateLayout::in_state_root(directory.path()));
    let root = crate::LocalWorkspaceRoot::from_path(directory.path()).unwrap();
    let Response::Session(session) = bounded(backend.dispatch(Request::Create {
        request: crate::CreateSessionRequest {
            label: None,
            active_agent: "local-test".into(),
            project_branch: "main".into(),
            project_store_key: root.store_key_hex(),
        },
    }))
    .await
    .unwrap() else {
        panic!("创建会话应返回身份")
    };
    let request = TurnRequest {
        session_id: session.session_id,
        turn_id: super::super::new_turn_id(),
        workspace: root.canonical_path().into(),
        input: "取消交接".into(),
    };
    (directory, backend, request)
}

fn submit(
    backend: &Arc<Backend>,
    request: &TurnRequest,
    configuration: Option<super::super::FrozenConfiguration>,
) -> tokio::task::JoinHandle<Result<Response>> {
    let backend = backend.clone();
    let request = request.clone();
    tokio::spawn(async move {
        backend
            .dispatch(Request::Submit {
                request,
                configuration: configuration.map(Box::new),
            })
            .await
    })
}

async fn cancel(backend: &Arc<Backend>, request: &TurnRequest) -> bool {
    let Response::Cancelled { owned } = bounded(backend.dispatch(Request::Cancel {
        session_id: request.session_id.clone(),
        turn_id: request.turn_id,
    }))
    .await
    .unwrap() else {
        panic!("取消应返回是否接管")
    };
    owned
}

#[tokio::test]
async fn c2_probe_does_not_take_cancellation_or_wait_for_future_configuration() {
    let (_directory, backend, request) = setup().await;
    let (entered, entry) = oneshot::channel();
    let (release, resume) = oneshot::channel();
    *lock(&backend.snapshot_gate) = Some((entered, resume));
    let probe = submit(&backend, &request, None);
    bounded(entry).await.unwrap();
    let owned = cancel(&backend, &request).await;
    release.send(()).unwrap();
    assert!(matches!(
        bounded(probe).await.unwrap().unwrap(),
        Response::Acceptance(Acceptance::NeedsConfiguration)
    ));
    let Response::Status(status) = bounded(backend.dispatch(Request::Stop)).await.unwrap() else {
        panic!("停止应返回状态")
    };
    assert_eq!(status.owners, 0);
    let view = backend
        .query(request.session_id, Some(request.turn_id))
        .await
        .unwrap();
    assert!(view.requested_turn.is_none() && view.active_turn.is_none());
    assert!(!owned, "独立无配置探测不接管未来执行的取消");
}

fn invalid_configuration(project: &std::path::Path) -> super::super::FrozenConfiguration {
    let configuration = super::super::tests::frozen(project, "http://127.0.0.1:1/v1");
    let mut value = serde_json::to_value(configuration).unwrap();
    value["profile_toml"] = serde_json::json!("不是 TOML");
    serde_json::from_value(value).unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn c3_cancel_store_and_supervisor_removal_share_admission_lock() {
    let (directory, backend, request) = setup().await;
    let (prepared, release_prepare) = arm(&backend.gates.prepare);
    let (retiring, release_retire) = arm(&backend.gates.retire);
    let submission = submit(
        &backend,
        &request,
        Some(invalid_configuration(directory.path())),
    );
    bounded(prepared).await.unwrap();
    let (entered, entry) = oneshot::channel();
    let (release_store, resume) = std::sync::mpsc::channel();
    *lock(&backend.gates.cancel_store) = Some((entered, resume));
    let server = backend.clone();
    let target = request.clone();
    let cancellation = tokio::spawn(async move { cancel(&server, &target).await });
    bounded(entry).await.unwrap();
    release_prepare.send(()).unwrap();
    bounded(retiring).await.unwrap();
    // 在置位前的真实临界区观察锁；旧的锁外 store 会让监督者提前移除。
    let protected = backend.admission.try_lock().is_err();
    release_store.send(()).unwrap();
    assert!(bounded(cancellation).await.unwrap());
    release_retire.send(()).unwrap();
    assert!(matches!(
        bounded(submission).await.unwrap().unwrap(),
        Response::Error(LocalError {
            kind: LocalErrorKind::BadRequest,
            ..
        })
    ));
    assert!(!cancel(&backend, &request).await);
    assert_eq!(backend.status().owners, 0);
    assert!(protected, "查找、资格和置位必须与监督移除同锁");
}

struct Provider {
    endpoint: String,
    arrived: oneshot::Receiver<()>,
    release: watch::Sender<bool>,
    shutdown: oneshot::Sender<()>,
    task: tokio::task::JoinHandle<Vec<serde_json::Value>>,
}

impl Provider {
    async fn start() -> Self {
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
                let (mut socket, _) = tokio::select! {
                    accepted = listener.accept() => accepted.unwrap(),
                    _ = &mut stopping => break,
                };
                bounded(async {
                    let mut headers = Vec::new();
                    while !headers.ends_with(b"\r\n\r\n") {
                        headers.push(socket.read_u8().await.unwrap());
                        assert!(headers.len() <= 65_536);
                    }
                    let headers = String::from_utf8(headers).unwrap();
                    let length: usize = headers.lines().find_map(|line| {
                        line.to_ascii_lowercase().strip_prefix("content-length:")
                            .map(|value| value.trim().parse().unwrap())
                    }).unwrap();
                    assert!(length < 1_048_576);
                    let mut body = vec![0; length];
                    socket.read_exact(&mut body).await.unwrap();
                    requests.push(serde_json::from_slice(&body).unwrap());
                    if let Some(arrival) = arrival.take() { let _ = arrival.send(()); }
                    while !*gate.borrow_and_update() { gate.changed().await.unwrap(); }
                    let body = "data: {\"choices\":[{\"delta\":{\"content\":\"完成\"},\"finish_reason\":null}]}\n\ndata: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n";
                    // 取消可先关闭响应连接；正向执行另以持久 Completed 断言交付成功。
                    let _ = socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await;
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

    async fn finish(self) -> Vec<serde_json::Value> {
        self.release.send_replace(true);
        self.shutdown.send(()).unwrap();
        bounded(self.task).await.unwrap()
    }
}

async fn idle(backend: &Backend) {
    bounded(async {
        while backend.status().owners != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await;
}

async fn ledger_tip(backend: &Backend, request: &TurnRequest) -> String {
    let layout = backend.layout.clone();
    let workspace = request.workspace.clone();
    bounded(tokio::task::spawn_blocking(move || {
        let root = crate::LocalWorkspaceRoot::from_path(&workspace).unwrap();
        lancet_audit::invocation_store::InvocationStore::open(&layout, &root.store_key())
            .unwrap()
            .pinned_view()
            .unwrap()
            .commit_oid
            .to_string()
    }))
    .await
    .unwrap()
}

async fn committed(backend: &Backend, request: &TurnRequest) -> CommittedTurnObservation {
    idle(backend).await;
    let view = backend
        .query(request.session_id.clone(), Some(request.turn_id))
        .await
        .unwrap();
    assert!(view.active_turn.is_none());
    let Some(TurnObservation::Committed(turn)) = view.requested_turn else {
        panic!("监督退出后应保留固定 Turn 结果")
    };
    *turn
}

#[tokio::test]
async fn c1_preparation_cancel_survives_all_frontend_disconnects() {
    let (directory, backend, request) = setup().await;
    let provider = Provider::start().await;
    let before = ledger_tip(&backend, &request).await;
    let configuration = super::super::tests::frozen(directory.path(), &provider.endpoint);
    let (prepared, release) = arm(&backend.gates.prepare);
    let (writer, mut frontend) = UnixStream::pair().unwrap();
    let connection = tokio::spawn(connection(backend.clone(), writer));
    ipc::write(
        &mut frontend,
        &Request::Submit {
            request: request.clone(),
            configuration: Some(Box::new(configuration)),
        },
    )
    .await
    .unwrap();
    bounded(prepared).await.unwrap();
    let view = backend
        .query(request.session_id.clone(), Some(request.turn_id))
        .await
        .unwrap();
    assert!(view.requested_turn.is_none() && view.active_turn.is_none());
    assert!(cancel(&backend, &request).await);
    drop(frontend);
    release.send(()).unwrap();
    assert!(
        bounded(connection).await.unwrap().is_err(),
        "回执连接已断开"
    );
    let turn = committed(&backend, &request).await;
    assert_eq!(turn.turn_id, request.turn_id);
    assert!(matches!(turn.outcome, crate::TurnOutcome::Cancelled { .. }));
    assert_eq!(
        ledger_tip(&backend, &request).await,
        before,
        "不得分发 Tool"
    );
    assert_eq!(provider.finish().await.len(), 0, "不得请求 provider");
    assert!(!cancel(&backend, &request).await);
    bounded(backend.dispatch(Request::Stop)).await.unwrap();
}

async fn wait_for_replies(backend: &Backend, request: &TurnRequest, count: usize) {
    bounded(async {
        loop {
            let waiting = lock(&backend.admission)
                .owners
                .get(&request.session_id)
                .is_some_and(|owner| owner.reply.receiver_count() >= count);
            if waiting {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await;
}

fn configuration_named(
    directory: &std::path::Path,
    endpoint: &str,
    prompt: &str,
) -> super::super::FrozenConfiguration {
    let configuration = super::super::tests::frozen(directory, endpoint);
    let mut value = serde_json::to_value(configuration).unwrap();
    value["system_prompt"] = serde_json::json!(prompt);
    serde_json::from_value(value).unwrap()
}

#[tokio::test]
async fn c2_concurrent_configurations_share_probe_then_one_cancellable_execution() {
    let (directory, backend, request) = setup().await;
    let provider = Provider::start().await;
    let before = ledger_tip(&backend, &request).await;
    let (queried, release_probe) = arm(&backend.snapshot_gate);
    let probe = submit(&backend, &request, None);
    bounded(queried).await.unwrap();
    let first = submit(
        &backend,
        &request,
        Some(configuration_named(
            directory.path(),
            &provider.endpoint,
            "第一份",
        )),
    );
    let second = submit(
        &backend,
        &request,
        Some(configuration_named(
            directory.path(),
            &provider.endpoint,
            "第二份",
        )),
    );
    wait_for_replies(&backend, &request, 3).await;
    let probe_owned = cancel(&backend, &request).await;
    let (prepared, release_execution) = arm(&backend.gates.prepare);
    release_probe.send(()).unwrap();
    assert!(matches!(
        bounded(probe).await.unwrap().unwrap(),
        Response::Acceptance(Acceptance::NeedsConfiguration)
    ));
    bounded(prepared).await.unwrap();
    wait_for_replies(&backend, &request, 2).await;
    assert!(cancel(&backend, &request).await);
    release_execution.send(()).unwrap();
    for submission in [first, second] {
        assert!(matches!(bounded(submission).await.unwrap().unwrap(),
            Response::Acceptance(Acceptance::Accepted { turn_id }) if turn_id == request.turn_id));
    }
    let turn = committed(&backend, &request).await;
    assert!(matches!(turn.outcome, crate::TurnOutcome::Cancelled { .. }));
    let view = backend
        .open()
        .await
        .unwrap()
        .read_view(&request.session_id, Some(request.turn_id))
        .await
        .unwrap();
    let stored = view.requested_turn.unwrap();
    assert!(matches!(
        stored.started().config.system_prompt.as_deref(),
        Some("第一份" | "第二份")
    ));
    assert_eq!(ledger_tip(&backend, &request).await, before);
    assert_eq!(provider.finish().await.len(), 0);
    assert!(!probe_owned, "连接中的配置尚未由后台接管");
}

#[tokio::test]
async fn c2_configuration_after_probe_retirement_does_not_inherit_false_cancel() {
    let (directory, backend, request) = setup().await;
    let provider = Provider::start().await;
    let (queried, release) = arm(&backend.snapshot_gate);
    let probe = submit(&backend, &request, None);
    bounded(queried).await.unwrap();
    assert!(!cancel(&backend, &request).await);
    release.send(()).unwrap();
    assert!(matches!(
        bounded(probe).await.unwrap().unwrap(),
        Response::Acceptance(Acceptance::NeedsConfiguration)
    ));
    assert_eq!(backend.status().owners, 0);
    let result = submit(
        &backend,
        &request,
        Some(configuration_named(
            directory.path(),
            &provider.endpoint,
            "晚到配置",
        )),
    );
    assert!(matches!(
        bounded(result).await.unwrap().unwrap(),
        Response::Acceptance(Acceptance::Accepted { .. })
    ));
    assert!(matches!(
        committed(&backend, &request).await.outcome,
        crate::TurnOutcome::Completed { .. }
    ));
    let requests = provider.finish().await;
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0]["messages"][0]["content"], "晚到配置");
}

#[tokio::test]
async fn c3_unconfigured_replay_shares_execution_and_late_cancel_preserves_outcome() {
    let (directory, backend, request) = setup().await;
    let provider = Provider::start().await;
    let (retiring, release) = arm(&backend.gates.retire);
    let submission = submit(
        &backend,
        &request,
        Some(configuration_named(
            directory.path(),
            &provider.endpoint,
            "原执行",
        )),
    );
    assert!(matches!(
        bounded(submission).await.unwrap().unwrap(),
        Response::Acceptance(Acceptance::Accepted { .. })
    ));
    bounded(retiring).await.unwrap();
    let replay = submit(&backend, &request, None);
    assert!(
        matches!(bounded(replay).await.unwrap().unwrap(), Response::Acceptance(Acceptance::Accepted { turn_id }) if turn_id == request.turn_id)
    );
    assert!(cancel(&backend, &request).await);
    release.send(()).unwrap();
    let original = committed(&backend, &request).await;
    assert!(matches!(
        original.outcome,
        crate::TurnOutcome::Completed { .. }
    ));
    assert!(!cancel(&backend, &request).await);
    let mut next = request.clone();
    next.turn_id = super::super::new_turn_id();
    let submission = submit(
        &backend,
        &next,
        Some(configuration_named(
            directory.path(),
            &provider.endpoint,
            "后续执行",
        )),
    );
    assert!(matches!(
        bounded(submission).await.unwrap().unwrap(),
        Response::Acceptance(Acceptance::Accepted { .. })
    ));
    assert!(matches!(
        committed(&backend, &next).await.outcome,
        crate::TurnOutcome::Completed { .. }
    ));
    assert_eq!(committed(&backend, &request).await, original);
    assert_eq!(provider.finish().await.len(), 2);
}

async fn rejected_cancellation_then_retry(waited_on_probe: bool) {
    let (directory, backend, request) = setup().await;
    let provider = Provider::start().await;
    let mut waiting = None;
    if waited_on_probe {
        let (queried, release_probe) = arm(&backend.snapshot_gate);
        let probe = submit(&backend, &request, None);
        bounded(queried).await.unwrap();
        let (retrying, release_retry) = arm(&backend.gates.retry);
        let configured = submit(
            &backend,
            &request,
            Some(configuration_named(
                directory.path(),
                &provider.endpoint,
                "等待中的有效配置",
            )),
        );
        wait_for_replies(&backend, &request, 2).await;
        release_probe.send(()).unwrap();
        assert!(matches!(
            bounded(probe).await.unwrap().unwrap(),
            Response::Acceptance(Acceptance::NeedsConfiguration)
        ));
        bounded(retrying).await.unwrap();
        waiting = Some((configured, release_retry));
    }
    let (prepared, release_prepare) = arm(&backend.gates.prepare);
    let rejected = submit(
        &backend,
        &request,
        Some(invalid_configuration(directory.path())),
    );
    bounded(prepared).await.unwrap();
    assert!(cancel(&backend, &request).await);
    release_prepare.send(()).unwrap();
    assert!(matches!(
        bounded(rejected).await.unwrap().unwrap(),
        Response::Error(LocalError {
            kind: LocalErrorKind::BadRequest,
            ..
        })
    ));
    assert_eq!(backend.status().owners, 0);
    let view = backend
        .query(request.session_id.clone(), Some(request.turn_id))
        .await
        .unwrap();
    assert!(
        view.requested_turn.is_none() && view.active_turn.is_none(),
        "只使用可证明未建立 Turn 的拒绝"
    );
    let valid = if let Some((configured, release_retry)) = waiting {
        release_retry.send(()).unwrap();
        configured
    } else {
        submit(
            &backend,
            &request,
            Some(configuration_named(
                directory.path(),
                &provider.endpoint,
                "晚到的有效配置",
            )),
        )
    };
    assert!(
        matches!(bounded(valid).await.unwrap().unwrap(), Response::Acceptance(Acceptance::Accepted { turn_id }) if turn_id == request.turn_id)
    );
    assert!(matches!(
        committed(&backend, &request).await.outcome,
        crate::TurnOutcome::Completed { .. }
    ));
    let requests = provider.finish().await;
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0]["messages"][0]["content"],
        if waited_on_probe {
            "等待中的有效配置"
        } else {
            "晚到的有效配置"
        }
    );
}

#[tokio::test]
async fn c4_rejected_operation_does_not_cancel_a_later_retry() {
    rejected_cancellation_then_retry(false).await;
}

#[tokio::test]
async fn c4_rejected_operation_does_not_cancel_configuration_already_waiting_on_probe() {
    rejected_cancellation_then_retry(true).await;
}

#[tokio::test]
async fn c5_existing_pending_and_pin_precede_new_configuration_and_recovery_is_closure_only() {
    let (directory, backend, request) = super::tests::pending_backend().await;
    let provider = Provider::start().await;
    let before = ledger_tip(&backend, &request).await;
    let pending = backend
        .query(request.session_id.clone(), Some(request.turn_id))
        .await
        .unwrap();
    assert!(!cancel(&backend, &request).await);
    let (prepared, release) = arm(&backend.gates.prepare);
    let replay = submit(&backend, &request, None);
    bounded(prepared).await.unwrap();
    assert!(
        !cancel(&backend, &request).await,
        "确认 Owner 不接管新执行取消"
    );
    release.send(()).unwrap();
    assert!(matches!(
        bounded(replay).await.unwrap().unwrap(),
        Response::Acceptance(Acceptance::Existing {
            turn: TurnObservation::Pending(_)
        })
    ));
    idle(&backend).await;
    assert_eq!(
        backend
            .query(request.session_id.clone(), Some(request.turn_id))
            .await
            .unwrap(),
        pending,
        "同请求确认不能隐式关闭 pending"
    );
    let invalid = || {
        let configuration = configuration_named(directory.path(), &provider.endpoint, "不应加载");
        let mut value = serde_json::to_value(configuration).unwrap();
        value["profile_toml"] = serde_json::json!("不是 TOML");
        serde_json::from_value(value).unwrap()
    };
    let replay = submit(&backend, &request, Some(invalid()));
    assert!(matches!(
        bounded(replay).await.unwrap().unwrap(),
        Response::Acceptance(Acceptance::Existing {
            turn: TurnObservation::Pending(_)
        })
    ));
    idle(&backend).await;
    assert_eq!(
        backend
            .query(request.session_id.clone(), Some(request.turn_id))
            .await
            .unwrap(),
        pending
    );
    let mut new = request.clone();
    new.turn_id = super::super::new_turn_id();
    new.input = "不接受的新输入".into();
    let (prepared, release) = arm(&backend.gates.prepare);
    let recovery = submit(&backend, &new, Some(invalid()));
    bounded(prepared).await.unwrap();
    assert!(cancel(&backend, &new).await);
    assert!(
        !cancel(&backend, &request).await,
        "不把新请求取消转给旧 Turn"
    );
    release.send(()).unwrap();
    assert!(
        matches!(bounded(recovery).await.unwrap().unwrap(), Response::Acceptance(Acceptance::Recovered { turn }) if turn.turn_id == request.turn_id)
    );
    let old = committed(&backend, &request).await;
    assert_eq!(old.origin, crate::ClosureOrigin::Reconciled);
    assert!(matches!(
        old.outcome,
        crate::TurnOutcome::Interrupted { .. }
    ));
    assert!(
        backend
            .query(new.session_id, Some(new.turn_id))
            .await
            .unwrap()
            .requested_turn
            .is_none()
    );
    assert!(!cancel(&backend, &request).await);
    let replay = submit(&backend, &request, Some(invalid()));
    assert!(
        matches!(bounded(replay).await.unwrap().unwrap(), Response::Acceptance(Acceptance::Existing { turn: TurnObservation::Committed(turn) }) if *turn == old)
    );
    idle(&backend).await;
    assert_eq!(ledger_tip(&backend, &request).await, before);
    assert_eq!(provider.finish().await.len(), 0);
}

#[tokio::test]
async fn c5_exact_identity_and_stop_preserve_the_owned_preparation() {
    let (directory, backend, request) = setup().await;
    let provider = Provider::start().await;
    let other_workspace = tempfile::tempdir().unwrap();
    let (prepared, release) = arm(&backend.gates.prepare);
    let submission = submit(
        &backend,
        &request,
        Some(configuration_named(
            directory.path(),
            &provider.endpoint,
            "应取消",
        )),
    );
    bounded(prepared).await.unwrap();
    let mut conflict = request.clone();
    conflict.input.push('！');
    assert_eq!(
        bounded(submit(&backend, &conflict, None))
            .await
            .unwrap()
            .err()
            .unwrap()
            .kind,
        LocalErrorKind::Conflict
    );
    conflict = request.clone();
    conflict.workspace = other_workspace.path().into();
    assert_eq!(
        bounded(submit(&backend, &conflict, None))
            .await
            .unwrap()
            .err()
            .unwrap()
            .kind,
        LocalErrorKind::Conflict
    );
    let mut other = request.clone();
    other.turn_id = super::super::new_turn_id();
    assert_eq!(
        bounded(submit(&backend, &other, None))
            .await
            .unwrap()
            .err()
            .unwrap()
            .kind,
        LocalErrorKind::Busy
    );
    assert!(!cancel(&backend, &other).await);
    let server = backend.clone();
    let stop = tokio::spawn(async move { server.dispatch(Request::Stop).await });
    bounded(async {
        while !backend.status().stopping {
            tokio::task::yield_now().await;
        }
    })
    .await;
    assert!(!stop.is_finished());
    let mut free_session = other.clone();
    free_session.session_id = "0".repeat(32);
    assert_eq!(
        bounded(submit(&backend, &free_session, None))
            .await
            .unwrap()
            .err()
            .unwrap()
            .kind,
        LocalErrorKind::Stopping
    );
    let replay = submit(&backend, &request, None);
    wait_for_replies(&backend, &request, 2).await;
    assert!(cancel(&backend, &request).await);
    release.send(()).unwrap();
    for task in [submission, replay] {
        assert!(matches!(
            bounded(task).await.unwrap().unwrap(),
            Response::Acceptance(Acceptance::Accepted { .. })
        ));
    }
    let Response::Status(status) = bounded(stop).await.unwrap().unwrap() else {
        panic!("排空返回状态")
    };
    assert!(status.stopping && status.owners == 0);
    assert!(matches!(
        committed(&backend, &request).await.outcome,
        crate::TurnOutcome::Cancelled { .. }
    ));
    assert_eq!(provider.finish().await.len(), 0);
}

#[tokio::test]
async fn c5_forwarding_panic_keeps_core_completion_until_stop_drains() {
    let (directory, backend, request) = setup().await;
    let mut provider = Provider::start().await;
    provider.release.send_replace(false);
    let before = ledger_tip(&backend, &request).await;
    let (forwarding, fail) = arm(&backend.gates.forward_panic);
    let (supervising, release_supervisor) = arm(&backend.gates.supervise);
    let (retiring, release_retire) = arm(&backend.gates.retire);
    let (writer, mut frontend) = UnixStream::pair().unwrap();
    let connection = tokio::spawn(connection(backend.clone(), writer));
    ipc::write(
        &mut frontend,
        &Request::Submit {
            request: request.clone(),
            configuration: Some(Box::new(configuration_named(
                directory.path(),
                &provider.endpoint,
                "监督排空",
            ))),
        },
    )
    .await
    .unwrap();
    bounded(forwarding).await.unwrap();
    bounded(&mut provider.arrived).await.unwrap();
    drop(frontend);
    fail.send(()).unwrap();
    bounded(supervising).await.unwrap();
    release_supervisor.send(()).unwrap();
    let server = backend.clone();
    let stop = tokio::spawn(async move { server.dispatch(Request::Stop).await });
    bounded(async {
        while !backend.status().stopping {
            tokio::task::yield_now().await;
        }
    })
    .await;
    assert!(!stop.is_finished());
    assert_eq!(backend.status().owners, 1, "转发退出不得丢弃核心完成接收器");
    assert!(cancel(&backend, &request).await);
    bounded(retiring).await.unwrap();
    assert!(!stop.is_finished(), "监督退场前 stop 仍须等待");
    release_retire.send(()).unwrap();
    let Response::Status(status) = bounded(stop).await.unwrap().unwrap() else {
        panic!("排空返回状态")
    };
    assert_eq!(status.owners, 0);
    assert_eq!(status.unconfirmed_operations, 1, "保留转发诊断");
    assert!(bounded(connection).await.unwrap().is_err());
    assert!(matches!(
        committed(&backend, &request).await.outcome,
        crate::TurnOutcome::Cancelled { .. }
    ));
    assert_eq!(ledger_tip(&backend, &request).await, before);
    assert_eq!(provider.finish().await.len(), 1);
}
