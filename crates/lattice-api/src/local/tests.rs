// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use super::client::SubmissionReply;
use super::*;
use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::{oneshot, watch},
};

async fn wait_until(mut ready: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(10), async {
        while !ready() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn private_socket_rejects_second_owner_and_oversized_frame() {
    let directory = tempfile::tempdir().unwrap();
    let layout = crate::StateLayout::in_state_root(directory.path());
    let (instance, listener) = ipc::Instance::bind(&layout).unwrap();
    assert!(matches!(
        ipc::Instance::bind(&layout),
        Err(LocalError {
            kind: LocalErrorKind::AlreadyRunning,
            ..
        })
    ));
    let path = ipc::endpoint(&layout).unwrap();
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let (mut writer, mut reader) = tokio::net::UnixStream::pair().unwrap();
    ipc::check_peer(&writer).unwrap();
    writer.write_all(b"LTLOCAL3").await.unwrap();
    writer.write_u32((ipc::MAX_FRAME + 1) as u32).await.unwrap();
    let result = ipc::read::<serde_json::Value>(&mut reader).await;
    assert!(matches!(
        result,
        Err(LocalError {
            kind: LocalErrorKind::Protocol,
            ..
        })
    ));
    drop(listener);
    instance.cleanup().unwrap();
    std::os::unix::fs::symlink("/dev/null", path).unwrap();
    assert!(matches!(
        ipc::Instance::bind(&layout),
        Err(LocalError {
            kind: LocalErrorKind::Permission,
            ..
        })
    ));
}

pub(super) fn frozen(project: &std::path::Path, endpoint: &str) -> FrozenConfiguration {
    let path = project.join("agent.toml");
    std::fs::write(&path, "schema_version = 1\n[agent]\nname = 'local-test'\nmodel = 'compat:local-test'\n[prompt]\nsystem = 'test'\n").unwrap();
    let loaded = lattice_profile::load_agent_profile(&path).unwrap();
    let connection = lattice_llm::LlmConnection::new(
        lattice_llm::ResolvedModel {
            alias: "compat:local-test".into(),
            provider: lattice_llm::ProviderKind::OpenAiCompatible,
            provider_model: "local-test".into(),
            base_url: endpoint.into(),
            api_key_env: String::new(),
            credential_status: lattice_llm::CredentialStatus::Present,
        },
        "local-test-secret".into(),
        vec![],
    )
    .unwrap();
    FrozenConfiguration::capture(
        loaded,
        &crate::LocalWorkspaceRoot::from_path(project).unwrap(),
        connection,
    )
    .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn detached_owners_share_acceptance_overlap_and_drain_with_fixed_reload() {
    let state = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let layout = crate::StateLayout::in_state_root(state.path());
    let serving = tokio::spawn(serve(layout.clone()));
    let client = LocalClient::new(layout.clone());
    tokio::time::timeout(Duration::from_secs(10), async {
        while client.status().await.is_err() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let root = crate::LocalWorkspaceRoot::from_path(project.path()).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
    let arrived = Arc::new(AtomicUsize::new(0));
    let (release, gate) = watch::channel(false);
    let (shutdown, mut stopping) = oneshot::channel();
    let count = arrived.clone();
    let provider = tokio::spawn(async move {
        let mut tasks = tokio::task::JoinSet::new();
        loop {
            tokio::select! {
                accepted = listener.accept() => {
                    let (mut socket, _) = accepted.unwrap();
                    let mut gate = gate.clone();
                    let count = count.clone();
                    tasks.spawn(async move {
                        let mut bytes = Vec::new();
                        loop {
                            let byte = socket.read_u8().await.unwrap(); bytes.push(byte);
                            assert!(bytes.len() <= 65_536);
                            if bytes.ends_with(b"\r\n\r\n") { break; }
                        }
                        let headers = String::from_utf8(bytes).unwrap();
                        let length: usize = headers.lines().find_map(|line| line.to_ascii_lowercase().strip_prefix("content-length:").map(|s| s.trim().parse().unwrap())).unwrap();
                        assert!(length < 1_048_576);
                        let mut body = vec![0; length]; socket.read_exact(&mut body).await.unwrap();
                        count.fetch_add(1, Ordering::SeqCst);
                        while !*gate.borrow_and_update() { gate.changed().await.unwrap(); }
                        let body = "data: {\"choices\":[{\"delta\":{\"content\":\"后台结果\"},\"finish_reason\":null}]}\n\ndata: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n";
                        let response = format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
                        // 取消测试允许客户端在 gate 释放前关闭响应连接；第一条持久正文仍单独断言。
                        let _ = socket.write_all(response.as_bytes()).await;
                    });
                }
                _ = &mut stopping => break,
            }
        }
        while let Some(result) = tasks.join_next().await {
            result.unwrap();
        }
    });
    let request = |session_id| TurnRequest {
        session_id,
        turn_id: new_turn_id(),
        workspace: root.canonical_path().into(),
        input: "local prompt".into(),
        agent: Some("local-test".into()),
    };
    let first = request(None);
    let second = request(None);
    assert_eq!(
        client.submit(first.clone(), None).await.unwrap(),
        Acceptance::NeedsConfiguration {
            agent_name: "local-test".into()
        }
    );
    let (accepted, replay) = tokio::join!(
        client.submit(first.clone(), Some(frozen(project.path(), &endpoint))),
        client.submit(first.clone(), Some(frozen(project.path(), &endpoint)))
    );
    let Acceptance::Accepted { turn: a } = accepted.unwrap() else {
        panic!("first acceptance")
    };
    assert!(
        matches!(replay.unwrap(), Acceptance::Accepted { turn } | Acceptance::Existing { turn } if turn.turn_id == first.turn_id && turn.session_id == a.session_id)
    );
    assert_eq!(
        client
            .submit(request(Some(a.session_id)), None)
            .await
            .unwrap_err()
            .kind,
        LocalErrorKind::Busy
    );
    let mut conflict = first.clone();
    conflict.input.push('!');
    assert_eq!(
        client.submit(conflict, None).await.unwrap_err().kind,
        LocalErrorKind::Conflict
    );
    let Acceptance::Accepted { turn: b } = client
        .submit(second.clone(), Some(frozen(project.path(), &endpoint)))
        .await
        .unwrap()
    else {
        panic!("second acceptance")
    };
    assert_ne!(a.session_id, b.session_id);
    wait_until(|| arrived.load(Ordering::SeqCst) == 2).await;
    let view = client
        .query(Some(a.session_id), Some(first.turn_id))
        .await
        .unwrap();
    assert!(
        !view
            .selected_turn
            .as_ref()
            .unwrap()
            .summary
            .progress
            .is_closed()
    );
    let log = client.log(a.session_id).await.unwrap();
    assert_eq!(log.nodes.len(), 3);
    assert_eq!(log.unclosed_turn, Some(first.turn_id));
    assert_eq!(
        client
            .list_sessions()
            .await
            .unwrap()
            .iter()
            .find(|row| row.session_id == a.session_id)
            .unwrap()
            .unclosed_turn,
        Some(first.turn_id)
    );
    assert_eq!(
        client
            .rewind(a.session_id, NodeId(0), root.canonical_path().into())
            .await
            .unwrap_err()
            .kind,
        LocalErrorKind::Busy
    );
    let target = TurnTarget {
        session_id: Some(a.session_id),
        turn_id: first.turn_id,
    };
    let mut observation = client
        .subscribe(
            target,
            Some(EventCursor {
                epoch: "previous-process".into(),
                sequence: 0,
            }),
        )
        .await
        .unwrap();
    let Some(Observation::Snapshot { view }) = observation.next().await.unwrap() else {
        panic!("snapshot, not Gap")
    };
    assert!(view.owned);
    let pending_anchor = view.history.clone();
    let pending_history = client
        .read_history_with_progress(&view, || {})
        .await
        .unwrap();
    assert_eq!(pending_history.entries.len(), 1);
    assert_eq!(
        pending_history.entries[0].summary,
        view.selected_turn.as_ref().unwrap().summary,
        "历史携带读取时的进度与结论"
    );
    assert!(
        matches!(&pending_history.entries[0].content, Node::UserMessage { message, .. }
        if matches!(&message.parts[0].semantic, MessageSemantic::Text { text } if text == "local prompt"))
    );
    drop(observation);
    let stopping_client = client.clone();
    let stop = tokio::spawn(async move { stopping_client.stop().await });
    tokio::time::timeout(Duration::from_secs(5), async {
        while !client.status().await.unwrap().stopping {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(!stop.is_finished());
    assert_eq!(
        client
            .submit(request(Some(b.session_id)), None)
            .await
            .unwrap_err()
            .kind,
        LocalErrorKind::Busy
    );
    assert!(
        client
            .query(Some(a.session_id), Some(first.turn_id))
            .await
            .is_ok()
    );
    let cancel_target = TurnTarget {
        session_id: Some(b.session_id),
        turn_id: second.turn_id,
    };
    assert!(client.cancel(cancel_target).await.unwrap());
    // 等持久取消而不是与 provider release 竞争；控制回执不等于已关闭。
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if client
                .turn(cancel_target)
                .await
                .unwrap()
                .summary
                .progress
                .is_closed()
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    release.send_replace(true);
    let status = stop.await.unwrap().unwrap();
    assert_eq!(status.owners, 0);
    serving.await.unwrap().unwrap();
    shutdown.send(()).unwrap();
    provider.await.unwrap();
    assert_eq!(arrived.load(Ordering::SeqCst), 2);
    let serving = tokio::spawn(serve(layout));
    tokio::time::timeout(Duration::from_secs(10), async {
        while client.status().await.is_err() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        client.turn(cancel_target).await.unwrap().summary.outcome,
        lattice_session::store::PublicTurnOutcome::Cancelled
    );
    let SubmissionReply::Accepted(prior) = client
        .submit_handshake(
            first.clone(),
            |_| async { panic!("existing Turn must not load configuration") },
            None,
        )
        .await
        .unwrap()
    else {
        panic!("existing Turn must be accepted")
    };
    assert!(
        matches!(prior, Acceptance::Existing { turn } if turn.turn_id == first.turn_id && turn.summary.progress.is_closed())
    );
    let view = client
        .query(Some(a.session_id), Some(first.turn_id))
        .await
        .unwrap();
    let history = client
        .read_history_with_progress(&view, || {})
        .await
        .unwrap();
    assert_eq!(history.entries.len(), 2);
    // 历史为每个节点带回所属 Turn 的结局，与 Turn 详情投影一致。
    let closed = view.selected_turn.as_ref().unwrap().summary.clone();
    assert!(closed.progress.is_closed());
    assert!(history.entries.iter().all(|entry| entry.summary == closed));
    let encoded = serde_json::to_string(&history).unwrap();
    assert!(encoded.contains("后台结果"));
    assert!(!encoded.contains("local-test-secret"));
    let mut offset = None;
    let mut fragments = String::new();
    loop {
        let page = client
            .history_page(pending_anchor.clone(), offset, 17)
            .await
            .unwrap();
        for entry in page.fragments {
            fragments.push_str(&entry.fragment.json_fragment);
        }
        offset = page.next;
        if offset.is_none() {
            break;
        }
    }
    let saved: Node = serde_json::from_str(&fragments).unwrap();
    assert_eq!(saved, pending_history.entries[0].content);
    assert_eq!(client.list_sessions().await.unwrap().len(), 2);
    assert_eq!(client.log(a.session_id).await.unwrap().nodes.len(), 3);
    assert_eq!(
        client
            .rewind(a.session_id, NodeId(0), root.canonical_path().into())
            .await
            .unwrap()
            .current,
        NodeId(0)
    );
    client.stop().await.unwrap();
    serving.await.unwrap().unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn query_probe_and_rewind_preserve_orphan_without_configuration() {
    for operation in ["probe", "write"] {
        preserve_orphan_through(operation).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn submit_handshake_preserves_orphan_and_loads_configuration_once() {
    preserve_orphan_through("submit_handshake").await;
}

async fn preserve_orphan_through(operation: &str) {
    use lattice_session::store::*;
    let state = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let layout = crate::StateLayout::in_state_root(state.path());
    let root = crate::LocalWorkspaceRoot::from_path(workspace.path()).unwrap();
    let store = SessionStore::create(&layout).unwrap();
    let session_id = SessionId::parse(&uuid::Uuid::new_v4().simple().to_string()).unwrap();
    let old_id = new_turn_id();
    let guard = store.try_lock_session(session_id).unwrap();
    store
        .accept_turn(
            &guard,
            AcceptTurn {
                session: AcceptSession::New { id: session_id },
                turn_id: old_id,
                project_store_key: root.store_key(),
                input: "old prompt".into(),
                accepted_at: chrono::Utc::now(),
                agent_selection: AgentSelection::Explicit,
                config: FrozenTurnConfig {
                    agent_name: "local-test".into(),
                    model: "unconfigured-model".into(),
                    provider: "openai_compatible".into(),
                    system_prompt: None,
                    catalog: ToolCatalogSnapshot::new("test".into(), vec![]).unwrap(),
                    limits: TurnExecutionLimits {
                        timeout_secs: 300,
                        max_model_steps: 32,
                        max_model_attempts: 1,
                    },
                },
            },
        )
        .unwrap();
    drop(guard);
    drop(store);
    let serving = tokio::spawn(serve(layout.clone()));
    let client = LocalClient::new(layout.clone());
    tokio::time::timeout(Duration::from_secs(10), async {
        while client.status().await.is_err() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let target = TurnTarget {
        session_id: Some(session_id),
        turn_id: old_id,
    };
    let before = client.query(Some(session_id), Some(old_id)).await.unwrap();
    assert!(
        !before
            .selected_turn
            .as_ref()
            .unwrap()
            .summary
            .progress
            .is_closed()
    );
    assert!(!client.cancel(target).await.unwrap());
    assert_eq!(
        before,
        client.query(Some(session_id), Some(old_id)).await.unwrap()
    );
    let new_id = new_turn_id();
    let request = TurnRequest {
        session_id: Some(session_id),
        turn_id: new_id,
        workspace: root.canonical_path().into(),
        input: "保留旧任务并接受新输入".into(),
        agent: None,
    };
    if operation == "submit_handshake" {
        // 新执行要求历史存在；探测与配置加载均不改旧节点。
        lancet_api::WorkspaceHistory::create(lancet_api::WorkspaceHistoryBacking::for_key(
            layout,
            root.store_key(),
        ))
        .unwrap();
        let provider = super::server::Provider::start().await;
        let loads = std::cell::Cell::new(0);
        let SubmissionReply::Accepted(acceptance) = client
            .submit_handshake(
                request,
                |agent| {
                    assert_eq!(agent, "local-test");
                    async {
                        loads.set(loads.get() + 1);
                        let old = client.turn(target).await?;
                        assert_eq!(&old, before.selected_turn.as_ref().unwrap());
                        Ok(frozen(workspace.path(), &provider.endpoint))
                    }
                },
                None,
            )
            .await
            .unwrap()
        else {
            panic!("new Turn must be accepted")
        };
        assert_eq!(loads.get(), 1);
        assert!(matches!(acceptance, Acceptance::Accepted { turn } if turn.turn_id == new_id));
        let mut observation = client
            .subscribe(
                TurnTarget {
                    session_id: Some(session_id),
                    turn_id: new_id,
                },
                None,
            )
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(10), async {
            let mut summary_seen = false;
            loop {
                match observation.next().await.unwrap().unwrap() {
                    Observation::Snapshot { view } => {
                        if view
                            .selected_turn
                            .as_ref()
                            .is_some_and(|t| t.summary.progress.is_closed())
                        {
                            assert_eq!(
                                view.selected_turn.as_ref().unwrap().summary.outcome,
                                PublicTurnOutcome::Completed
                            );
                            summary_seen = true;
                        }
                    }
                    Observation::Finished { history, .. } => {
                        assert!(summary_seen);
                        assert_eq!(history.entries.len(), 4); // 两轮各有 User 与 Step
                        break;
                    }
                    Observation::Event { .. } => {}
                    Observation::Diagnostic { error } => {
                        panic!("unexpected execution diagnostic: {error}")
                    }
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(provider.finish().await.len(), 1);
    } else if operation == "probe" {
        assert_eq!(
            client.submit(request, None).await.unwrap(),
            Acceptance::NeedsConfiguration {
                agent_name: "local-test".into()
            }
        );
        assert_eq!(
            client
                .turn(TurnTarget {
                    session_id: Some(session_id),
                    turn_id: new_id
                })
                .await
                .unwrap_err()
                .kind,
            LocalErrorKind::NotFound
        );
    } else {
        client
            .rewind(session_id, NodeId(0), root.canonical_path().into())
            .await
            .unwrap();
    }
    let after = client.query(Some(session_id), Some(old_id)).await.unwrap();
    assert_eq!(after.selected_turn, before.selected_turn);
    client.stop().await.unwrap();
    serving.await.unwrap().unwrap();
}

#[tokio::test]
async fn first_probe_and_readonly_requests_never_initialize_missing_store() {
    let state = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let layout = crate::StateLayout::in_state_root(state.path());
    let serving = tokio::spawn(serve(layout.clone()));
    let client = LocalClient::new(layout.clone());
    tokio::time::timeout(Duration::from_secs(5), async {
        while client.status().await.is_err() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let request = TurnRequest {
        session_id: None,
        turn_id: new_turn_id(),
        workspace: project.path().canonicalize().unwrap(),
        input: " original ".into(),
        agent: Some("alpha".into()),
    };
    assert_eq!(
        client.submit(request.clone(), None).await.unwrap(),
        Acceptance::NeedsConfiguration {
            agent_name: "alpha".into()
        }
    );
    let mut invalid = request;
    invalid.agent = None;
    assert_eq!(
        client.submit(invalid, None).await.unwrap_err().kind,
        LocalErrorKind::BadRequest
    );
    let error = client.list_sessions().await.unwrap_err();
    assert!(matches!(
        error.store,
        Some(StoreDiagnostic::NotInitialized { .. })
    ));
    assert!(!layout.lattice_root().join("lattice.redb").exists());
    client.stop().await.unwrap();
    serving.await.unwrap().unwrap();
}
