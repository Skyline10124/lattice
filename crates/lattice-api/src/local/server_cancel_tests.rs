// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0
use super::*;
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
                    assert!(length < 1_048_576);
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

async fn backend() -> (
    tempfile::TempDir,
    Arc<Backend>,
    super::super::LocalClient,
    tokio::task::JoinHandle<()>,
) {
    let dir = tempfile::tempdir().unwrap();
    let layout = crate::StateLayout::in_state_root(dir.path());
    let (instance, listener) = ipc::Instance::bind(&layout).unwrap();
    let listener = UnixListener::from_std(listener).unwrap();
    let backend = Backend::new(layout.clone());
    let server = backend.clone();
    let task = tokio::spawn(async move {
        let mut connections = JoinSet::new();
        let mut closed = server.closed.subscribe();
        loop {
            tokio::select! {
                accepted = listener.accept() => { let (socket, _) = accepted.unwrap(); connections.spawn(connection(server.clone(), socket)); }
                _ = connections.join_next(), if !connections.is_empty() => {},
                _ = closed.changed() => break,
            }
        }
        connections.abort_all();
        while connections.join_next().await.is_some() {}
        instance.cleanup().unwrap();
    });
    (dir, backend, super::super::LocalClient::new(layout), task)
}
fn request(dir: &std::path::Path) -> TurnRequest {
    TurnRequest {
        turn_id: super::super::new_turn_id(),
        session: SubmitSession::New(super::super::new_session_id()),
        workspace: dir.into(),
        input: "测试".into(),
        agent: Some("local-test".into()),
    }
}
fn config(dir: &std::path::Path, endpoint: &str) -> super::super::FrozenConfiguration {
    super::super::tests::frozen(dir, endpoint)
}
fn invalid(dir: &std::path::Path) -> super::super::FrozenConfiguration {
    let mut config = serde_json::to_value(config(dir, "http://127.0.0.1:1/v1")).unwrap();
    config["profile_toml"] = serde_json::json!("invalid toml");
    serde_json::from_value(config).unwrap()
}
async fn idle(backend: &Backend) {
    let mut changed = backend.changed.subscribe();
    bounded(async {
        while backend.status().owners != 0 {
            changed.changed().await.unwrap();
        }
    })
    .await;
}
async fn finish(backend: &Backend, task: tokio::task::JoinHandle<()>) {
    idle(backend).await;
    backend.closed.send_replace(true);
    bounded(task).await.unwrap();
}
#[tokio::test]
async fn legacy_store_rejection_before_execution_does_not_leave_unconfirmed_cleanup() {
    let (dir, backend, client, task) = backend().await;
    let legacy = backend.layout.lattice_root().join("sessions.git");
    std::fs::create_dir_all(&legacy).unwrap();
    let error = client
        .submit(
            request(dir.path()),
            config(dir.path(), "http://127.0.0.1:1/v1"),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(error.store.as_deref(), Some(StoreDiagnostic::LegacyStores { paths }) if paths == &[legacy])
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
async fn admission_identity_conflicts_busy_and_departed_submitter_never_creates_session() {
    let (dir, backend, client, task) = backend().await;
    let input = request(dir.path());
    let sid = input.session.id();
    let (entered, release) = arm(&backend.gates.registered);
    let mut wire = ipc::connect(backend.layout.clone()).await.unwrap();
    ipc::write(
        &mut wire,
        &Request::Submit {
            request: input.clone(),
            configuration: Box::new(config(dir.path(), "http://127.0.0.1:1/v1")),
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
        .submit(input.clone(), invalid(dir.path()))
        .await
        .unwrap_err();
    assert_eq!(
        (conflict.kind, conflict.conflict),
        (LocalErrorKind::Conflict, Some(ConflictReason::TurnIdUsed))
    );
    let mut race = input.clone();
    race.turn_id = super::super::new_turn_id();
    assert_eq!(
        client
            .submit(race.clone(), invalid(dir.path()))
            .await
            .unwrap_err()
            .kind,
        LocalErrorKind::Busy
    );
    assert!(client.status().await.unwrap().owners >= 1);
    let mut view = client.watch(sid).await.unwrap();
    assert_eq!(
        view.next().await.unwrap_err().kind,
        LocalErrorKind::NotFound
    );
    drop(view);
    drop(wire);
    release.send(()).unwrap();
    idle(&backend).await;
    assert_eq!(
        client.turn(input.turn_id).await.unwrap_err().kind,
        LocalErrorKind::NotFound
    );
    assert!(backend.optional_store().await.unwrap().is_none());
    // 被拒绝的 New(S) 没有建立，释放后同一个 S 可以由新 TurnId 使用。
    let provider = Provider::start().await;
    assert!(
        matches!(client.submit(race.clone(), config(dir.path(), &provider.endpoint)).await.unwrap(), Acceptance::Accepted { turn } if turn.session_id == sid)
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
async fn identity_table_prioritizes_used_turn_binding_new_collision_and_busy() {
    let (dir, backend, client, task) = backend().await;
    let mut provider = Provider::start().await;
    provider.release.send_replace(false);
    let input = request(dir.path());
    let sid = input.session.id();
    client
        .submit(input.clone(), config(dir.path(), &provider.endpoint))
        .await
        .unwrap();
    bounded(&mut provider.arrived).await.unwrap();
    let wrong = tempfile::tempdir().unwrap();
    let mut reused = input.clone();
    reused.session = SubmitSession::Existing(super::super::new_session_id());
    reused.workspace = wrong.path().into();
    let error = client
        .submit(reused, invalid(dir.path()))
        .await
        .unwrap_err();
    assert_eq!(error.conflict, Some(ConflictReason::TurnIdUsed));
    let mut missing = input.clone();
    missing.turn_id = super::super::new_turn_id();
    missing.session = SubmitSession::Existing(super::super::new_session_id());
    assert_eq!(
        client
            .submit(missing, invalid(dir.path()))
            .await
            .unwrap_err()
            .kind,
        LocalErrorKind::NotFound
    );
    let mut appended = input.clone();
    appended.turn_id = super::super::new_turn_id();
    appended.session = SubmitSession::Existing(sid);
    appended.workspace = wrong.path().into();
    assert_eq!(
        client
            .submit(appended.clone(), invalid(dir.path()))
            .await
            .unwrap_err()
            .conflict,
        Some(ConflictReason::BindingMismatch)
    );
    appended.workspace = input.workspace.clone();
    assert_eq!(
        client
            .submit(appended, invalid(dir.path()))
            .await
            .unwrap_err()
            .kind,
        LocalErrorKind::Busy
    );
    let mut collision = input.clone();
    collision.turn_id = super::super::new_turn_id();
    assert_eq!(
        client
            .submit(collision, invalid(dir.path()))
            .await
            .unwrap_err()
            .conflict,
        Some(ConflictReason::SessionExists)
    );
    assert!(client.list_sessions().await.unwrap()[0].owner);
    assert!(!client.cancel(super::super::new_turn_id()).await.unwrap());
    assert!(client.cancel(input.turn_id).await.unwrap());
    assert!(client.cancel(input.turn_id).await.unwrap());
    provider.release.send_replace(true);
    idle(&backend).await;
    assert!(!client.cancel(input.turn_id).await.unwrap());
    assert!(!client.list_sessions().await.unwrap()[0].owner);
    let error = client
        .submit(input.clone(), invalid(dir.path()))
        .await
        .unwrap_err();
    assert_eq!(error.conflict, Some(ConflictReason::TurnIdUsed));
    assert_eq!(provider.finish().await.len(), 1);
    finish(&backend, task).await;
}
#[tokio::test]
async fn preparation_cancel_is_owned_and_rejected_request_does_not_poison_retry() {
    let (dir, backend, client, task) = backend().await;
    let input = request(dir.path());
    let (entered, release) = arm(&backend.gates.prepare);
    let submit = {
        let client = client.clone();
        let input = input.clone();
        let config = invalid(dir.path());
        tokio::spawn(async move { client.submit(input, config).await })
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
        .submit(input.clone(), config(dir.path(), &provider.endpoint))
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
    let input = request(dir.path());
    client
        .submit(input.clone(), config(dir.path(), &provider.endpoint))
        .await
        .unwrap();
    bounded(&mut provider.arrived).await.unwrap();
    let mut view = client.watch(input.session.id()).await.unwrap();
    assert!(matches!(
        view.next().await.unwrap().unwrap(),
        ViewUpdate::Head(SessionHead { owner: true, .. })
    ));
    let stop = {
        let client = client.clone();
        tokio::spawn(async move { client.stop().await })
    };
    let mut changed = backend.changed.subscribe();
    bounded(async {
        while !backend.status().stopping {
            tokio::task::yield_now().await;
        }
    })
    .await;
    assert!(!stop.is_finished());
    assert_eq!(
        client
            .submit(request(dir.path()), invalid(dir.path()))
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
    assert_eq!(bounded(stop).await.unwrap().unwrap().owners, 0);
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
    assert_eq!(provider.finish().await.len(), 1);
    finish(&backend, task).await;
}
async fn seed_round(backend: &Backend, sid: SessionId, fresh: bool) -> TurnId {
    use lattice_session::store::*;
    let api = backend.stores.acquire(true).await.unwrap();
    let id = super::super::new_turn_id();
    let project =
        crate::LocalWorkspaceRoot::from_path(backend.layout.lattice_root().parent().unwrap())
            .unwrap()
            .store_key();
    api.blocking(move |api| {
        let guard = api.store.try_lock_session(sid)?;
        api.store.accept_turn(
            &guard,
            AcceptTurn {
                session: if fresh {
                    AcceptSession::New { id: sid }
                } else {
                    AcceptSession::Existing { id: sid }
                },
                turn_id: id,
                project_store_key: project,
                input: "seed".into(),
                accepted_at: chrono::Utc::now(),
                agent_selection: AgentSelection::Explicit,
                config: FrozenTurnConfig {
                    agent_name: "local-test".into(),
                    model: "test".into(),
                    provider: "openai-compatible".into(),
                    system_prompt: None,
                    catalog: ToolCatalogSnapshot::new("test".into(), vec![]).unwrap(),
                    limits: TurnExecutionLimits {
                        timeout_secs: 300,
                        max_model_steps: 32,
                        max_model_attempts: 2,
                    },
                },
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
    let sid = super::super::new_session_id();
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
    let input = request(dir.path());
    let sid = input.session.id();
    client
        .submit(input.clone(), config(dir.path(), &provider.endpoint))
        .await
        .unwrap();
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
    // CEP 通知只唤醒推送，不标记任何节点；不能借节点变化掩盖缺失的补推。
    sink.emit(crate::TurnEvent::CepCallStarted {
        invocation_id: lancet_protocol::InvocationId::parse("00000000-0000-4000-8000-000000000001")
            .unwrap(),
        method: "test".into(),
        semantic_stage: "test".into(),
        guard_stage: "test".into(),
    });
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
    let sid = super::super::new_session_id();
    let turn_id = seed_round(&backend, sid, true).await;
    let sink = backend.observer(sid);
    sink.emit(crate::TurnEvent::TurnStarted {
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
    let origin = lattice_conversation::ProviderOrigin::OpenAiCompatible;
    let attempt_origin = AttemptOrigin {
        protocol: AttemptProtocol::ChatCompletions,
        provider_id: "openai-compatible".into(),
    };
    io.start_request(key, attempt_origin.clone()).await.unwrap();
    sink.emit(crate::TurnEvent::ModelStarted {
        key,
        provider: origin,
        model: "test".into(),
    });
    sink.emit(crate::TurnEvent::TokenDelta {
        key,
        content: "你".into(),
    });
    sink.emit(crate::TurnEvent::TokenDelta {
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
    sink.emit(crate::TurnEvent::TokenDelta {
        key,
        content: "！".into(),
    });
    let batch = capture(fast.clone()).await;
    assert!(
        matches!(batch.as_slice(), [ViewUpdate::Text(TextUpdate { offset: 6, segments, .. })] if segments == &[TextSegment::Text { text: "！".into() }])
    );
    io.stop_attempt(AttemptStopped {
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
    sink.emit(crate::TurnEvent::LlmRetry {
        key,
        delay_ms: 1,
        notice: Notice {
            level: NoticeLevel::Warn,
            text: "retry".into(),
        },
    });
    key.attempt_ordinal = 1;
    io.start_request(key, attempt_origin.clone()).await.unwrap();
    sink.emit(crate::TurnEvent::ModelStarted {
        key,
        provider: origin,
        model: "test".into(),
    });
    sink.emit(crate::TurnEvent::ReasoningDelta {
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
    sink.emit(crate::TurnEvent::TokenDelta {
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
    let input = request(dir.path());
    client
        .submit(input.clone(), config(dir.path(), &provider.endpoint))
        .await
        .unwrap();
    bounded(&mut provider.arrived).await.unwrap();
    let (entered, release) = arm(&backend.gates.capture);
    let mut view = client.watch(input.session.id()).await.unwrap();
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
                session_id: input.session.id(),
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
    let sid = super::super::new_session_id();
    let first_provider = Provider::start().await;
    let mut first = request(dir.path());
    first.session = SubmitSession::New(sid);
    client
        .submit(first, config(dir.path(), &first_provider.endpoint))
        .await
        .unwrap();
    idle(&backend).await;
    assert_eq!(first_provider.finish().await.len(), 1);
    let (before, resume_read) = arm(&backend.gates.capture_before);
    let mut view = client.watch(sid).await.unwrap();
    bounded(before).await.unwrap();
    let mut provider = Provider::start().await;
    provider.release.send_replace(false);
    let mut input = request(dir.path());
    input.session = SubmitSession::Existing(sid);
    client
        .submit(input.clone(), config(dir.path(), &provider.endpoint))
        .await
        .unwrap();
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
    let sid = super::super::new_session_id();
    let first = seed_round(&backend, sid, true).await;
    let sink = backend.observer(sid);
    sink.emit(crate::TurnEvent::TurnStarted {
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
        sink.emit(crate::TurnEvent::TurnStarted {
            turn: TurnRef {
                session_id: sid,
                turn_id: id,
            },
            user_node: NodeId(ordinal * 2 + 1),
        });
        close_seed(&backend, sid, id).await;
    }
    let mut rejected = request(dir.path());
    rejected.session = SubmitSession::Existing(sid);
    assert_eq!(
        client
            .submit(rejected, invalid(dir.path()))
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
    for existing in [false, true] {
        let (dir, backend, client, task) = backend().await;
        let mut input = request(dir.path());
        let sid = input.session.id();
        let sink = backend.observer(sid);
        if existing {
            let provider = Provider::start().await;
            let mut first = request(dir.path());
            first.session = SubmitSession::New(sid);
            client
                .submit(first, config(dir.path(), &provider.endpoint))
                .await
                .unwrap();
            idle(&backend).await;
            assert_eq!(provider.finish().await.len(), 1);
            input.session = SubmitSession::Existing(sid);
        }
        let position = NodeId(if existing { 4 } else { 2 });
        let provider = Provider::start().await;
        let (accepted, fail_read) = arm(&sink.accepted_read_failure);
        let submitting = {
            let client = client.clone();
            let input = input.clone();
            let configuration = config(dir.path(), &provider.endpoint);
            tokio::spawn(async move { client.submit(input, configuration).await })
        };
        bounded(accepted).await.unwrap();
        assert!(!sink.events().iter().any(|event| matches!(event, crate::TurnEvent::TurnStarted { turn, .. } if turn.turn_id == input.turn_id)));
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
    let sid = super::super::new_session_id();
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
        let sid = super::super::new_session_id();
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
                .submit(request(dir.path()), invalid(dir.path()))
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
        let sid = super::super::new_session_id();
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
    let shared_sid = super::super::new_session_id();
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
    let input = request(dir.path());
    client
        .submit(input.clone(), config(dir.path(), &provider.endpoint))
        .await
        .unwrap();
    bounded(&mut provider.arrived).await.unwrap();
    let mut view = client.watch(input.session.id()).await.unwrap();
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
        let sid = super::super::new_session_id();
        let first = seed_round(&backend, sid, true).await;
        let mut provider = Provider::start().await;
        provider.release.send_replace(false);
        let second = request(dir.path());
        client
            .submit(second.clone(), config(dir.path(), &provider.endpoint))
            .await
            .unwrap();
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
        let second_sid = second.session.id();
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
    // 已开库时停在接受事务之前，未建库时停在建库之前；两处都已通过停机检查。
    for store_open in [true, false] {
        let (dir, backend, client, task) = backend().await;
        let other = super::super::new_session_id();
        let seeded = if store_open {
            Some(seed_round(&backend, other, true).await)
        } else {
            None
        };
        let provider = Provider::start().await;
        let input = request(dir.path());
        let sink = backend.observer(input.session.id());
        let (paused, resume) = arm(if store_open {
            &sink.before_accept
        } else {
            &backend.gates.prepare
        });
        let submitting = {
            let client = client.clone();
            let input = input.clone();
            let configuration = config(dir.path(), &provider.endpoint);
            tokio::spawn(async move { client.submit(input, configuration).await })
        };
        bounded(paused).await.unwrap();
        let seeded = match seeded {
            Some(id) => id,
            None => seed_round(&backend, other, true).await,
        };
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
async fn store_creation_failure_rejects_the_first_submission() {
    let (dir, backend, client, task) = backend().await;
    // 建库的父目录同步失败，库是否持久不明；这一轮尚未进入接受事务。
    *lock(&backend.stores.creation_sync_probe) = Some(Arc::new(|| true));
    let error = client
        .submit(
            request(dir.path()),
            config(dir.path(), "http://127.0.0.1:1/v1"),
        )
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
    let first = request(dir.path());
    let second = request(dir.path());
    let mut first_config =
        serde_json::to_value(config(dir.path(), &first_provider.endpoint)).unwrap();
    first_config["system_prompt"] = serde_json::json!("first frozen prompt");
    let first_config = serde_json::from_value(first_config).unwrap();
    client.submit(first.clone(), first_config).await.unwrap();
    bounded(&mut first_provider.arrived).await.unwrap();
    let mut second_config =
        serde_json::to_value(config(dir.path(), &second_provider.endpoint)).unwrap();
    second_config["system_prompt"] = serde_json::json!("second frozen prompt");
    client
        .submit(
            second.clone(),
            serde_json::from_value(second_config).unwrap(),
        )
        .await
        .unwrap();
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
    let first_sid = first.session.id();
    let first_tid = first.turn_id;
    let first_config = api
        .read(move |r| r.turn_config(first_sid, first_tid))
        .await
        .unwrap();
    let second_sid = second.session.id();
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
async fn existing_orphan_accepts_new_input_without_mutating_old_nodes_and_root_requires_agent() {
    let (dir, backend, client, task) = backend().await;
    let sid = super::super::new_session_id();
    let old = seed_round(&backend, sid, true).await;
    let before = client.turn(old).await.unwrap();
    let root = crate::LocalWorkspaceRoot::from_path(dir.path()).unwrap();
    let backing =
        lancet_api::WorkspaceHistoryBacking::for_key(backend.layout.clone(), root.store_key());
    tokio::task::spawn_blocking(move || lancet_api::WorkspaceHistory::create(backing))
        .await
        .unwrap()
        .unwrap();
    let provider = Provider::start().await;
    let mut next = request(dir.path());
    next.session = SubmitSession::Existing(sid);
    next.agent = None;
    assert!(matches!(
        client
            .submit(next.clone(), config(dir.path(), &provider.endpoint))
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
    let mut root_request = next;
    root_request.turn_id = super::super::new_turn_id();
    assert_eq!(
        client
            .submit(root_request.clone(), config(dir.path(), &provider.endpoint))
            .await
            .unwrap_err()
            .kind,
        LocalErrorKind::BadRequest
    );
    assert_eq!(
        client.turn(root_request.turn_id).await.unwrap_err().kind,
        LocalErrorKind::NotFound
    );
    root_request.agent = Some("local-test".into());
    client
        .submit(root_request, config(dir.path(), &provider.endpoint))
        .await
        .unwrap();
    idle(&backend).await;
    assert_eq!(client.turn(old).await.unwrap(), before);
    assert_eq!(provider.finish().await.len(), 2);
    finish(&backend, task).await;
}
#[tokio::test]
async fn preparation_disconnect_and_forwarding_panic_keep_independent_cancelled_completion() {
    for forwarding_panic in [false, true] {
        let (dir, backend, client, task) = backend().await;
        let mut provider = Provider::start().await;
        provider.release.send_replace(false);
        let input = request(dir.path());
        let (entered, release) = arm(if forwarding_panic {
            &backend.gates.forward_panic
        } else {
            &backend.gates.prepare
        });
        let submit = {
            let client = client.clone();
            let input = input.clone();
            let config = config(dir.path(), &provider.endpoint);
            tokio::spawn(async move { client.submit(input, config).await })
        };
        let mut release = Some(release);
        bounded(entered).await.unwrap();
        if forwarding_panic {
            bounded(&mut provider.arrived).await.unwrap();
            release.take().unwrap().send(()).unwrap();
            assert!(matches!(
                bounded(submit).await.unwrap().unwrap(),
                Acceptance::ConfirmationUnknown { turn_id, session_id }
                    if turn_id == input.turn_id && session_id == input.session.id()
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
            let TurnLookup::Accepted { turn } = client.turn(input.turn_id).await.unwrap() else {
                panic!("accepted")
            };
            assert_eq!(turn.summary.outcome, PublicTurnOutcome::Cancelled);
            assert!(turn.summary.progress.is_closed());
        } else {
            assert_eq!(
                client.turn(input.turn_id).await.unwrap_err().kind,
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
    let input = request(dir.path());
    client
        .submit(input.clone(), config(dir.path(), &provider.endpoint))
        .await
        .unwrap();
    bounded(&mut provider.arrived).await.unwrap();
    let permits = backend.subscriptions.acquire_many(32).await.unwrap();
    let head = client.session_head(input.session.id()).await.unwrap();
    assert_eq!(head.turn_id, Some(input.turn_id));
    assert!(head.owner);
    assert!(!head.path_replaced);
    assert_eq!(
        client.cancel_session(input.session.id()).await.unwrap(),
        (Some(input.turn_id), true)
    );
    assert_eq!(
        client
            .session_head(super::super::new_session_id())
            .await
            .unwrap_err()
            .kind,
        LocalErrorKind::NotFound
    );
    idle(&backend).await;
    assert!(!client.session_head(input.session.id()).await.unwrap().owner);
    assert_eq!(
        client.cancel_session(input.session.id()).await.unwrap(),
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
            request: request(dir.path()),
            configuration: Box::new(invalid(dir.path())),
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
async fn invalid_request_is_rejected_before_claiming_a_new_session() {
    let (dir, backend, client, task) = backend().await;
    let mut input = request(dir.path());
    input.input = "x".repeat(2 * 1024 * 1024 + 1);
    let sid = input.session.id();
    let (entered, release) = arm(&backend.gates.registered);
    let submitting = {
        let client = client.clone();
        let configuration = invalid(dir.path());
        tokio::spawn(async move { client.submit(input, configuration).await })
    };
    tokio::select! {
        result = submitting => assert_eq!(result.unwrap().unwrap_err().kind, LocalErrorKind::BadRequest),
        _ = entered => {
            release.send(()).unwrap();
            panic!("无效输入占住了 New(SessionId)");
        }
    }
    assert!(!lock(&backend.admission).sessions.contains_key(&sid));
    finish(&backend, task).await;
}
