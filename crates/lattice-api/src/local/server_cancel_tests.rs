// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0
use super::*;
use lattice_session::store::{ChangeSeq, PublicTurnOutcome, TurnDetail};

type Gate = (oneshot::Sender<()>, oneshot::Receiver<()>);
type StoreGate = (oneshot::Sender<()>, std::sync::mpsc::Receiver<()>);

// 只控制实际操作的调度，不注入 Owner、取消载体或完成结果。
#[derive(Default)]
pub(super) struct Gates {
    pub(super) probe: Mutex<Option<Gate>>,
    pub(super) prepare: Mutex<Option<Gate>>,
    pub(super) retire: Mutex<Option<Gate>>,
    pub(super) retry: Mutex<Option<Gate>>,
    pub(super) forward_panic: Mutex<Option<Gate>>,
    pub(super) supervise: Mutex<Option<Gate>>,
    pub(super) snapshot: Mutex<Option<Gate>>,
    pub(super) accepted: Mutex<Option<Gate>>,
    pub(super) history: Mutex<Option<Gate>>,
    cancel_store: Mutex<Option<StoreGate>>,
}
impl Gates {
    pub(super) fn pause_cancel_store(&self) {
        if let Some((entered, release)) = lock(&self.cancel_store).take() {
            entered.send(()).unwrap();
            release.recv_timeout(Duration::from_secs(10)).unwrap();
        }
    }
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
        entered.send(()).unwrap();
        bounded(release).await.unwrap();
    }
}
pub(super) async fn fail_forwarding(slot: &Mutex<Option<Gate>>) {
    let gate = lock(slot).take();
    if let Some((entered, release)) = gate {
        entered.send(()).unwrap();
        bounded(release).await.unwrap();
        panic!("注入接受 continuation panic，核心收尾仍归监督者");
    }
}
async fn bounded<T>(future: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(10), future)
        .await
        .expect("局部交接超时")
}
pub(super) async fn setup() -> (tempfile::TempDir, Arc<Backend>, TurnRequest) {
    let (dir, backend, mut request) = super::tests::pending_backend().await;
    super::tests::close(&backend, &request).await;
    let root = crate::LocalWorkspaceRoot::from_path(dir.path()).unwrap();
    let backing =
        lancet_api::WorkspaceHistoryBacking::for_key(backend.layout.clone(), root.store_key());
    tokio::task::spawn_blocking(move || lancet_api::WorkspaceHistory::create(backing))
        .await
        .unwrap()
        .unwrap();
    request.turn_id = super::super::new_turn_id();
    request.input = "取消交接".into();
    (dir, backend, request)
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
        target: TurnTarget {
            session_id: request.session_id,
            turn_id: request.turn_id,
        },
    }))
    .await
    .unwrap() else {
        panic!("cancel response")
    };
    owned
}
async fn absent(backend: &Backend, request: &TurnRequest) {
    let id = request.turn_id;
    assert!(
        backend
            .open()
            .await
            .unwrap()
            .read(move |r| r.turn_node(id))
            .await
            .unwrap()
            .is_none()
    );
}
async fn idle(backend: &Backend) {
    bounded(async {
        let mut changed = backend.changed.subscribe();
        while backend.status().owners != 0 {
            changed.changed().await.unwrap();
        }
    })
    .await;
}
async fn committed(backend: &Backend, request: &TurnRequest) -> TurnDetail {
    idle(backend).await;
    let view = backend
        .query(request.session_id, Some(request.turn_id))
        .await
        .unwrap();
    assert!(view.active.is_none());
    let turn = view.selected_turn.unwrap();
    assert!(turn.summary.progress.is_closed());
    turn
}
fn outcome(turn: &TurnDetail) -> PublicTurnOutcome {
    turn.summary.outcome.clone()
}
async fn change_seq(backend: &Backend, request: &TurnRequest) -> ChangeSeq {
    let key = crate::LocalWorkspaceRoot::from_path(&request.workspace)
        .unwrap()
        .store_key();
    backend
        .open()
        .await
        .unwrap()
        .read(move |r| Ok(r.operator_status(&key)?.latest_change))
        .await
        .unwrap()
}
async fn wait_for_replies(backend: &Backend, request: &TurnRequest, count: usize) {
    bounded(async {
        loop {
            if lock(&backend.admission)
                .turns
                .get(&request.turn_id)
                .is_some_and(|o| o.reply.receiver_count() >= count)
            {
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
fn invalid_configuration(directory: &std::path::Path) -> super::super::FrozenConfiguration {
    let configuration = super::super::tests::frozen(directory, "http://127.0.0.1:1/v1");
    let mut value = serde_json::to_value(configuration).unwrap();
    value["profile_toml"] = serde_json::json!("不是 TOML");
    serde_json::from_value(value).unwrap()
}
fn accepted(response: Response, id: TurnId) {
    assert!(
        matches!(response, Response::Acceptance(Acceptance::Accepted { turn }) if turn.turn_id == id)
    );
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
                    assert!(length < 1_048_576);
                    let mut body = vec![0; length]; socket.read_exact(&mut body).await.unwrap();
                    requests.push(serde_json::from_slice(&body).unwrap());
                    if let Some(arrival) = arrival.take() { let _ = arrival.send(()); }
                    while !*gate.borrow_and_update() { gate.changed().await.unwrap(); }
                    let body = "data: {\"choices\":[{\"delta\":{\"content\":\"完成\"},\"finish_reason\":null}]}\n\ndata: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n";
                    // 取消允许先断连接，正向返回由持久 outcome 验证。
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
    pub(in crate::local) async fn finish(self) -> Vec<serde_json::Value> {
        self.release.send_replace(true);
        self.shutdown.send(()).unwrap();
        bounded(self.task).await.unwrap()
    }
}

#[tokio::test]
async fn c2_probe_does_not_take_cancellation_or_wait_for_future_configuration() {
    let (_dir, backend, request) = setup().await;
    let (entry, release) = arm(&backend.gates.probe);
    let task = submit(&backend, &request, None);
    bounded(entry).await.unwrap();
    assert!(!cancel(&backend, &request).await);
    release.send(()).unwrap();
    assert!(matches!(
        bounded(task).await.unwrap().unwrap(),
        Response::Acceptance(Acceptance::NeedsConfiguration { .. })
    ));
    absent(&backend, &request).await;
    assert_eq!(backend.status().owners, 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn c3_cancel_store_and_supervisor_removal_share_admission_lock() {
    let (dir, backend, request) = setup().await;
    let (prepared, release_prepare) = arm(&backend.gates.prepare);
    let (retiring, release_retire) = arm(&backend.gates.retire);
    let task = submit(&backend, &request, Some(invalid_configuration(dir.path())));
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
    let protected = backend.admission.try_lock().is_err();
    release_store.send(()).unwrap();
    assert!(bounded(cancellation).await.unwrap());
    release_retire.send(()).unwrap();
    assert!(matches!(
        bounded(task).await.unwrap().unwrap(),
        Response::Error(LocalError {
            kind: LocalErrorKind::BadRequest,
            ..
        })
    ));
    assert!(!cancel(&backend, &request).await);
    assert_eq!(backend.status().owners, 0);
    assert!(protected, "取消查找与置位不得在监督移除锁之外");
}

#[tokio::test]
async fn c1_preparation_cancel_survives_all_frontend_disconnects() {
    for without_sid in [false, true] {
        let (dir, backend, mut request) = setup().await;
        if without_sid {
            request.session_id = None;
        }
        let provider = Provider::start().await;
        let before = change_seq(&backend, &request).await;
        let (prepared, release) = arm(&backend.gates.prepare);
        let (writer, mut frontend) = UnixStream::pair().unwrap();
        let task = tokio::spawn(connection(backend.clone(), writer));
        ipc::write(
            &mut frontend,
            &Request::Submit {
                request: request.clone(),
                configuration: Some(Box::new(super::super::tests::frozen(
                    dir.path(),
                    &provider.endpoint,
                ))),
            },
        )
        .await
        .unwrap();
        bounded(prepared).await.unwrap();
        absent(&backend, &request).await;
        assert!(cancel(&backend, &request).await);
        drop(frontend);
        release.send(()).unwrap();
        assert!(bounded(task).await.unwrap().is_err());
        assert_eq!(
            outcome(&committed(&backend, &request).await),
            PublicTurnOutcome::Cancelled
        );
        assert_eq!(change_seq(&backend, &request).await, before);
        assert_eq!(provider.finish().await.len(), 0);
        assert!(!cancel(&backend, &request).await);
    }
}

#[tokio::test]
async fn c2_concurrent_configurations_share_probe_then_one_cancellable_execution() {
    let (dir, backend, request) = setup().await;
    let provider = Provider::start().await;
    let before = change_seq(&backend, &request).await;
    let (entered, release_probe) = arm(&backend.gates.probe);
    let probe = submit(&backend, &request, None);
    bounded(entered).await.unwrap();
    let first = submit(
        &backend,
        &request,
        Some(configuration_named(
            dir.path(),
            &provider.endpoint,
            "第一份",
        )),
    );
    let second = submit(
        &backend,
        &request,
        Some(configuration_named(
            dir.path(),
            &provider.endpoint,
            "第二份",
        )),
    );
    wait_for_replies(&backend, &request, 3).await;
    assert!(!cancel(&backend, &request).await);
    let (prepared, release) = arm(&backend.gates.prepare);
    release_probe.send(()).unwrap();
    assert!(matches!(
        bounded(probe).await.unwrap().unwrap(),
        Response::Acceptance(Acceptance::NeedsConfiguration { .. })
    ));
    bounded(prepared).await.unwrap();
    wait_for_replies(&backend, &request, 2).await;
    assert!(cancel(&backend, &request).await);
    release.send(()).unwrap();
    for task in [first, second] {
        accepted(bounded(task).await.unwrap().unwrap(), request.turn_id);
    }
    assert_eq!(
        outcome(&committed(&backend, &request).await),
        PublicTurnOutcome::Cancelled
    );
    let id = request.turn_id;
    let session = request.session_id.unwrap();
    let config = backend
        .open()
        .await
        .unwrap()
        .read(move |r| r.turn_config(session, id))
        .await
        .unwrap();
    assert!(matches!(
        config.system_prompt.as_deref(),
        Some("第一份" | "第二份")
    ));
    assert_eq!(change_seq(&backend, &request).await, before);
    assert_eq!(provider.finish().await.len(), 0);
}

#[tokio::test]
async fn c2_configuration_after_probe_retirement_does_not_inherit_false_cancel() {
    let (dir, backend, request) = setup().await;
    let provider = Provider::start().await;
    let (entry, release) = arm(&backend.gates.probe);
    let probe = submit(&backend, &request, None);
    bounded(entry).await.unwrap();
    assert!(!cancel(&backend, &request).await);
    release.send(()).unwrap();
    assert!(matches!(
        bounded(probe).await.unwrap().unwrap(),
        Response::Acceptance(Acceptance::NeedsConfiguration { .. })
    ));
    assert_eq!(backend.status().owners, 0);
    accepted(
        bounded(submit(
            &backend,
            &request,
            Some(configuration_named(
                dir.path(),
                &provider.endpoint,
                "晚到配置",
            )),
        ))
        .await
        .unwrap()
        .unwrap(),
        request.turn_id,
    );
    assert_eq!(
        outcome(&committed(&backend, &request).await),
        PublicTurnOutcome::Completed
    );
    let requests = provider.finish().await;
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0]["messages"][0]["content"], "晚到配置");
}

#[tokio::test]
async fn c3_unconfigured_replay_shares_execution_and_late_cancel_preserves_outcome() {
    let (dir, backend, request) = setup().await;
    let provider = Provider::start().await;
    let (retiring, release) = arm(&backend.gates.retire);
    accepted(
        bounded(submit(
            &backend,
            &request,
            Some(configuration_named(
                dir.path(),
                &provider.endpoint,
                "原执行",
            )),
        ))
        .await
        .unwrap()
        .unwrap(),
        request.turn_id,
    );
    bounded(retiring).await.unwrap();
    assert!(matches!(
        bounded(submit(&backend, &request, None)).await.unwrap().unwrap(),
        Response::Acceptance(Acceptance::Existing { turn }) if turn.turn_id == request.turn_id
    ));
    assert!(cancel(&backend, &request).await);
    release.send(()).unwrap();
    let original = committed(&backend, &request).await;
    assert_eq!(outcome(&original), PublicTurnOutcome::Completed);
    assert!(!cancel(&backend, &request).await);
    let mut next = request.clone();
    next.turn_id = super::super::new_turn_id();
    accepted(
        bounded(submit(
            &backend,
            &next,
            Some(configuration_named(
                dir.path(),
                &provider.endpoint,
                "后续执行",
            )),
        ))
        .await
        .unwrap()
        .unwrap(),
        next.turn_id,
    );
    assert_eq!(
        outcome(&committed(&backend, &next).await),
        PublicTurnOutcome::Completed
    );
    assert_eq!(committed(&backend, &request).await, original);
    assert_eq!(provider.finish().await.len(), 2);
}

async fn rejected_cancellation_then_retry(waited: bool) {
    let (dir, backend, request) = setup().await;
    let provider = Provider::start().await;
    let waiting = if waited {
        let (entry, release) = arm(&backend.gates.probe);
        let probe = submit(&backend, &request, None);
        bounded(entry).await.unwrap();
        let (retrying, release_retry) = arm(&backend.gates.retry);
        let configured = submit(
            &backend,
            &request,
            Some(configuration_named(
                dir.path(),
                &provider.endpoint,
                "有效配置",
            )),
        );
        wait_for_replies(&backend, &request, 2).await;
        release.send(()).unwrap();
        assert!(matches!(
            bounded(probe).await.unwrap().unwrap(),
            Response::Acceptance(Acceptance::NeedsConfiguration { .. })
        ));
        bounded(retrying).await.unwrap();
        Some((configured, release_retry))
    } else {
        None
    };
    let (entry, release) = arm(&backend.gates.prepare);
    let rejected = submit(&backend, &request, Some(invalid_configuration(dir.path())));
    bounded(entry).await.unwrap();
    assert!(cancel(&backend, &request).await);
    release.send(()).unwrap();
    assert!(matches!(
        bounded(rejected).await.unwrap().unwrap(),
        Response::Error(LocalError {
            kind: LocalErrorKind::BadRequest,
            ..
        })
    ));
    absent(&backend, &request).await;
    assert_eq!(backend.status().owners, 0);
    let valid = if let Some((task, release)) = waiting {
        release.send(()).unwrap();
        task
    } else {
        submit(
            &backend,
            &request,
            Some(configuration_named(
                dir.path(),
                &provider.endpoint,
                "有效配置",
            )),
        )
    };
    accepted(bounded(valid).await.unwrap().unwrap(), request.turn_id);
    assert_eq!(
        outcome(&committed(&backend, &request).await),
        PublicTurnOutcome::Completed
    );
    let requests = provider.finish().await;
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0]["messages"][0]["content"], "有效配置");
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
async fn c5_existing_turn_precedes_configuration_and_new_send_preserves_orphan() {
    let (dir, backend, request) = super::tests::pending_backend().await;
    let provider = Provider::start().await;
    let before = change_seq(&backend, &request).await;
    let pending = backend
        .query(request.session_id, Some(request.turn_id))
        .await
        .unwrap();
    for config in [None, Some(invalid_configuration(dir.path()))] {
        assert!(!cancel(&backend, &request).await);
        assert!(
            matches!(bounded(submit(&backend, &request, config)).await.unwrap().unwrap(), Response::Acceptance(Acceptance::Existing { turn }) if turn.summary.is_none())
        );
        assert_eq!(
            backend
                .query(request.session_id, Some(request.turn_id))
                .await
                .unwrap(),
            pending
        );
    }
    let root = crate::LocalWorkspaceRoot::from_path(dir.path()).unwrap();
    let backing =
        lancet_api::WorkspaceHistoryBacking::for_key(backend.layout.clone(), root.store_key());
    tokio::task::spawn_blocking(move || lancet_api::WorkspaceHistory::create(backing))
        .await
        .unwrap()
        .unwrap();
    let mut new = request.clone();
    new.turn_id = super::super::new_turn_id();
    new.input = "新输入".into();
    let (entry, release) = arm(&backend.gates.prepare);
    let task = submit(
        &backend,
        &new,
        Some(configuration_named(
            dir.path(),
            &provider.endpoint,
            "新配置",
        )),
    );
    bounded(entry).await.unwrap();
    assert!(cancel(&backend, &new).await);
    assert!(!cancel(&backend, &request).await);
    release.send(()).unwrap();
    accepted(bounded(task).await.unwrap().unwrap(), new.turn_id);
    idle(&backend).await;
    let session = request.session_id.unwrap();
    let turn_id = request.turn_id;
    let old = backend
        .open()
        .await
        .unwrap()
        .read(move |r| r.turn_detail(session, turn_id))
        .await
        .unwrap();
    assert_eq!(&old, pending.selected_turn.as_ref().unwrap());
    assert_eq!(outcome(&old), PublicTurnOutcome::Failed);
    assert_eq!(
        old.summary.progress,
        lattice_session::store::StepProgress::Reserved
    );
    assert_eq!(
        outcome(&committed(&backend, &new).await),
        PublicTurnOutcome::Cancelled
    );
    assert!(
        matches!(bounded(submit(&backend, &request, Some(invalid_configuration(dir.path())))).await.unwrap().unwrap(), Response::Acceptance(Acceptance::Existing { turn }) if turn == TurnView::from(&old))
    );
    assert_eq!(change_seq(&backend, &request).await, before);
    assert_eq!(provider.finish().await.len(), 0);
}

#[tokio::test]
async fn c5_exact_identity_and_stop_preserve_the_owned_preparation() {
    let (dir, backend, request) = setup().await;
    let provider = Provider::start().await;
    let other_workspace = tempfile::tempdir().unwrap();
    let (entry, release) = arm(&backend.gates.prepare);
    let submission = submit(
        &backend,
        &request,
        Some(configuration_named(
            dir.path(),
            &provider.endpoint,
            "应取消",
        )),
    );
    bounded(entry).await.unwrap();
    for workspace in [false, true] {
        let mut conflict = request.clone();
        if workspace {
            conflict.workspace = other_workspace.path().into();
        } else {
            conflict.input.push('!');
        }
        assert_eq!(
            bounded(submit(&backend, &conflict, None))
                .await
                .unwrap()
                .err()
                .unwrap()
                .kind,
            LocalErrorKind::Conflict
        );
    }
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
    other.session_id = None;
    assert_eq!(
        bounded(submit(&backend, &other, None))
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
        accepted(bounded(task).await.unwrap().unwrap(), request.turn_id);
    }
    let Response::Status(status) = bounded(stop).await.unwrap().unwrap() else {
        panic!("stop")
    };
    assert!(status.stopping && status.owners == 0);
    assert_eq!(
        outcome(&committed(&backend, &request).await),
        PublicTurnOutcome::Cancelled
    );
    assert_eq!(provider.finish().await.len(), 0);
}

#[tokio::test]
async fn c5_forwarding_panic_keeps_core_completion_until_stop_drains() {
    let (dir, backend, request) = setup().await;
    let mut provider = Provider::start().await;
    provider.release.send_replace(false);
    let before = change_seq(&backend, &request).await;
    let (forwarding, fail) = arm(&backend.gates.forward_panic);
    let (supervising, resume) = arm(&backend.gates.supervise);
    let (retiring, release_retire) = arm(&backend.gates.retire);
    let (writer, mut frontend) = UnixStream::pair().unwrap();
    let task = tokio::spawn(connection(backend.clone(), writer));
    ipc::write(
        &mut frontend,
        &Request::Submit {
            request: request.clone(),
            configuration: Some(Box::new(configuration_named(
                dir.path(),
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
    resume.send(()).unwrap();
    let server = backend.clone();
    let stop = tokio::spawn(async move { server.dispatch(Request::Stop).await });
    bounded(async {
        while !backend.status().stopping {
            tokio::task::yield_now().await;
        }
    })
    .await;
    assert!(!stop.is_finished());
    assert_eq!(backend.status().owners, 1);
    assert!(cancel(&backend, &request).await);
    bounded(retiring).await.unwrap();
    assert!(!stop.is_finished());
    release_retire.send(()).unwrap();
    let Response::Status(status) = bounded(stop).await.unwrap().unwrap() else {
        panic!("stop")
    };
    assert_eq!(status.owners, 0);
    assert_eq!(status.unconfirmed_operations, 1);
    assert!(bounded(task).await.unwrap().is_err());
    assert_eq!(
        outcome(&committed(&backend, &request).await),
        PublicTurnOutcome::Cancelled
    );
    assert_eq!(change_seq(&backend, &request).await, before);
    assert_eq!(provider.finish().await.len(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn new_session_reserves_admission_before_accepted_identity_is_visible() {
    let (dir, backend, mut request) = setup().await;
    request.session_id = None;
    let provider = Provider::start().await;
    provider.release.send_replace(false);
    let (at_accept, resume_accept) = arm(&backend.gates.accepted);
    let original = submit(
        &backend,
        &request,
        Some(super::super::tests::frozen(dir.path(), &provider.endpoint)),
    );
    bounded(at_accept).await.unwrap();
    let view = backend.query(None, Some(request.turn_id)).await.unwrap();
    assert!(view.owned);
    let (at_probe, resume_probe) = arm(&backend.gates.probe);
    let mut other = request.clone();
    other.turn_id = super::super::new_turn_id();
    other.session_id = Some(view.session.session_id);
    other.input = "competing request during acceptance handoff".into();
    let mut competing = submit(&backend, &other, None);
    let early = bounded(async {
        tokio::select! {
            result = &mut competing => Some(result.unwrap()),
            result = at_probe => { result.unwrap(); None }
        }
    })
    .await;
    resume_accept.send(()).unwrap();
    provider.release.send_replace(true);
    let response = bounded(original).await.unwrap();
    let _ = resume_probe.send(());
    let second = match early {
        Some(response) => response,
        None => bounded(competing).await.unwrap(),
    };
    idle(&backend).await;
    let durable = backend.query(None, Some(request.turn_id)).await.unwrap();
    assert!(durable.selected_turn.unwrap().summary.is_some());
    assert_eq!(provider.finish().await.len(), 1);
    assert!(
        matches!(
            response,
            Ok(Response::Acceptance(Acceptance::Accepted { .. }))
        ),
        "already accepted original must receive Accepted: {}",
        serde_json::to_string(&response).unwrap()
    );
    assert!(matches!(
        second,
        Err(LocalError {
            kind: LocalErrorKind::Busy,
            ..
        }) | Ok(Response::Error(LocalError {
            kind: LocalErrorKind::Busy,
            ..
        }))
    ));
}
