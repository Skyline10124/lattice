// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

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
    writer.write_all(b"LTLOCAL2").await.unwrap();
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
    let loaded = crate::load_agent_profile_path(&path).unwrap();
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
    let create = || crate::CreateSessionRequest {
        label: None,
        active_agent: "local-test".into(),
        project_branch: "main".into(),
        project_store_key: root.store_key_hex(),
    };
    let a = client.create_session(create()).await.unwrap();
    let b = client.create_session(create()).await.unwrap();
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
    let request = |id: String| TurnRequest {
        session_id: id,
        turn_id: TurnId::parse(&uuid::Uuid::new_v4().to_string()).unwrap(),
        workspace: root.canonical_path().into(),
        input: "local prompt".into(),
    };
    let first = request(a.session_id.clone());
    let second = request(b.session_id.clone());
    assert_eq!(
        client.lookup_or_recover(first.clone()).await.unwrap(),
        Acceptance::NeedsConfiguration
    );
    let (accepted, replay) = tokio::join!(
        client.submit(first.clone(), Some(frozen(project.path(), &endpoint))),
        client.submit(first.clone(), Some(frozen(project.path(), &endpoint)))
    );
    assert!(matches!(accepted.unwrap(), Acceptance::Accepted { .. }));
    assert!(matches!(replay.unwrap(), Acceptance::Accepted { .. }));
    let busy = client
        .submit(request(a.session_id.clone()), None)
        .await
        .unwrap_err();
    assert_eq!(busy.kind, LocalErrorKind::Busy);
    let mut conflict = first.clone();
    conflict.input.push('!');
    assert_eq!(
        client.submit(conflict, None).await.unwrap_err().kind,
        LocalErrorKind::Conflict
    );
    assert!(matches!(
        client
            .submit(second.clone(), Some(frozen(project.path(), &endpoint)))
            .await
            .unwrap(),
        Acceptance::Accepted { .. }
    ));
    wait_until(|| arrived.load(Ordering::SeqCst) == 2).await;
    let view = client
        .query(&a.session_id, Some(first.turn_id))
        .await
        .unwrap();
    assert!(matches!(
        view.requested_turn,
        Some(TurnObservation::Pending(_))
    ));
    let status = client.session_status(&a.session_id).await.unwrap();
    assert_eq!(status.commit_count, 0);
    assert_eq!(
        client
            .write_session(
                &a.session_id,
                SessionWrite::SetAgent {
                    name: "other".into()
                }
            )
            .await
            .err()
            .unwrap()
            .kind,
        LocalErrorKind::Busy
    );
    let mut observation = client
        .subscribe(
            &a.session_id,
            first.turn_id,
            Some(EventCursor {
                epoch: "previous-process".into(),
                sequence: 0,
            }),
        )
        .await
        .unwrap();
    assert!(matches!(
        observation.next().await.unwrap(),
        Some(Observation::Gap { .. })
    ));
    let Some(Observation::Snapshot {
        view, owned: true, ..
    }) = observation.next().await.unwrap()
    else {
        panic!("pending snapshot")
    };
    let pending = view.active_turn.unwrap();
    let pending_messages = client
        .read_messages(&a.session_id, &pending.journal_oid)
        .await
        .unwrap();
    assert_eq!(pending_messages.len(), 1);
    assert!(
        matches!(&pending_messages[0].parts[0].semantic, MessageSemantic::Text { text } if text == "local prompt")
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
    assert_eq!(
        client
            .submit(request(b.session_id.clone()), None)
            .await
            .unwrap_err()
            .kind,
        LocalErrorKind::Busy
    );
    assert!(
        client
            .query(&a.session_id, Some(first.turn_id))
            .await
            .is_ok()
    );
    assert!(client.cancel(&b.session_id, second.turn_id).await.unwrap());
    client.cancel(&b.session_id, second.turn_id).await.unwrap();
    release.send_replace(true);
    let status = stop.await.unwrap().unwrap();
    assert_eq!(status.owners, 0);
    serving.await.unwrap().unwrap();
    shutdown.send(()).unwrap();
    provider.await.unwrap();
    assert_eq!(arrived.load(Ordering::SeqCst), 2);
    // 重启没有自动恢复或 provider 接线；只读结果与永久身份仍可找回。
    let serving = tokio::spawn(serve(layout));
    tokio::time::timeout(Duration::from_secs(10), async {
        while client.status().await.is_err() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let cancelled = client
        .query(&b.session_id, Some(second.turn_id))
        .await
        .unwrap();
    assert!(
        matches!(cancelled.requested_turn, Some(TurnObservation::Committed(turn)) if matches!(turn.outcome, crate::TurnOutcome::Cancelled { .. }))
    );
    let prior = client
        .submit_with(first.clone(), || async {
            panic!("existing Turn must not load configuration")
        })
        .await
        .unwrap();
    let Acceptance::Existing {
        turn: TurnObservation::Committed(commit),
    } = prior
    else {
        panic!("expected stored result")
    };
    let mut text = String::new();
    let mut offset = 0;
    loop {
        let page = client
            .read_transcript_page(&a.session_id, &commit.commit_id, offset, 17)
            .await
            .unwrap();
        text.push_str(&page.jsonl);
        if let Some(next) = page.next_offset {
            offset = next;
        } else {
            break;
        }
    }
    assert!(text.contains("后台结果"));
    assert_eq!(
        client
            .history_page(&a.session_id, &commit.commit_id, 0, 1)
            .await
            .unwrap(),
        vec![commit.commit_id.clone()]
    );
    assert!(
        client
            .history_page(&a.session_id, &commit.commit_id, 1, 1)
            .await
            .unwrap()
            .is_empty()
    );
    let messages = client
        .read_messages(&a.session_id, &commit.commit_id)
        .await
        .unwrap();
    assert_eq!(messages.len(), 2);
    // 后续提交与后台重启不改变已固定的 pending 正文，也不触发模型重做。
    assert_eq!(
        client
            .read_messages(&a.session_id, &pending.journal_oid)
            .await
            .unwrap(),
        pending_messages
    );
    assert_eq!(client.list_sessions().await.unwrap().len(), 2);
    assert_eq!(client.log(&a.session_id).await.unwrap().len(), 1);
    let diff = client
        .diff(&a.session_id, &commit.commit_id, &commit.commit_id)
        .await
        .unwrap();
    assert!(diff.only_in_from.is_empty() && diff.only_in_to.is_empty());
    assert!(matches!(
        client
            .write_session(
                &a.session_id,
                SessionWrite::Branch {
                    name: "review".into(),
                    from: Some(commit.commit_id.clone())
                }
            )
            .await
            .unwrap(),
        SessionWriteResult::Branch(_)
    ));
    assert!(matches!(
        client
            .write_session(
                &a.session_id,
                SessionWrite::Checkout {
                    target: "review".into()
                }
            )
            .await
            .unwrap(),
        SessionWriteResult::Checkout(_)
    ));
    assert!(matches!(
        client
            .write_session(
                &a.session_id,
                SessionWrite::Reset {
                    target: commit.commit_id.clone()
                }
            )
            .await
            .unwrap(),
        SessionWriteResult::Reset(_)
    ));
    assert!(matches!(
        client
            .write_session(
                &a.session_id,
                SessionWrite::SetAgent {
                    name: "other".into()
                }
            )
            .await
            .unwrap(),
        SessionWriteResult::SetAgent(_)
    ));
    assert!(matches!(
        client
            .rewind(
                &a.session_id,
                &commit.commit_id,
                root.canonical_path().into()
            )
            .await
            .unwrap(),
        LocalRewindResult::Success(_)
    ));
    assert!(!text.contains("local-test-secret"));
    client.stop().await.unwrap();
    serving.await.unwrap().unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn query_stays_read_only_and_probe_or_write_closes_without_configuration() {
    for operation in ["probe", "write"] {
        close_pending_through(operation).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn submit_with_closes_pending_before_loading_configuration_once() {
    close_pending_through("submit_with").await;
}

async fn close_pending_through(operation: &str) {
    use lattice_session::pending::{
        FrozenTurnConfig, ToolCatalogSnapshot, TurnExecutionLimits, TurnStartedFacts,
    };
    use lattice_session::{Blocking, LockMode, SessionGraphStore};
    let state = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let layout = crate::StateLayout::in_state_root(state.path());
    let root = crate::LocalWorkspaceRoot::from_path(workspace.path()).unwrap();
    let serving = tokio::spawn(serve(layout.clone()));
    let client = LocalClient::new(layout.clone());
    tokio::time::timeout(Duration::from_secs(10), async {
        while client.status().await.is_err() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let session = client
        .create_session(crate::CreateSessionRequest {
            label: None,
            active_agent: "test".into(),
            project_branch: "main".into(),
            project_store_key: root.store_key_hex(),
        })
        .await
        .unwrap();
    let store = lattice_session::GitSessionGraphStore::open_layout(&layout).unwrap();
    let old_id = TurnId::parse(&uuid::Uuid::new_v4().to_string()).unwrap();
    {
        let lock = store
            .lock_execution(
                &session.session_id,
                LockMode::Exclusive,
                Blocking::NonBlocking,
            )
            .unwrap();
        store
            .begin_pending_turn(
                TurnStartedFacts {
                    turn_id: old_id,
                    session_id: lancet_protocol::SessionId::parse(&session.session_id).unwrap(),
                    project_store_key: root.store_key(),
                    parent_conversation_tip: None,
                    user_message: lattice_conversation::NativeMessage::new(
                        lattice_conversation::MessageRole::User,
                        None,
                        vec![lattice_conversation::MessagePart::semantic_only(
                            lattice_conversation::MessageSemantic::Text {
                                text: "old prompt".into(),
                            },
                        )],
                    ),
                    config: FrozenTurnConfig {
                        agent_name: "test".into(),
                        model: "unconfigured-model".into(),
                        provider: "test".into(),
                        system_prompt: None,
                        profile_snapshot_json: "{}".into(),
                        policy_snapshot_json: "{}".into(),
                        catalog: ToolCatalogSnapshot::new("test".into(), vec![]).unwrap(),
                        limits: TurnExecutionLimits {
                            timeout_secs: 300,
                            max_model_steps: 32,
                            max_model_attempts: 1,
                        },
                    },
                    accepted_at: "2026-09-01T00:00:00Z".into(),
                },
                &lock,
            )
            .unwrap();
    }
    let before = client
        .query(&session.session_id, Some(old_id))
        .await
        .unwrap();
    assert!(!client.cancel(&session.session_id, old_id).await.unwrap());
    assert_eq!(
        before,
        client
            .query(&session.session_id, Some(old_id))
            .await
            .unwrap()
    );
    if operation == "submit_with" {
        let provider = super::server::Provider::start().await;
        let new_id = new_turn_id();
        let loads = std::cell::Cell::new(0);
        let acceptance = client.submit_with(
                TurnRequest {
                    session_id: session.session_id.clone(),
                    turn_id: new_id,
                    workspace: root.canonical_path().into(),
                    input: "关闭旧任务后接受新输入".into(),
                },
                || async {
                    loads.set(loads.get() + 1);
                    let closed = client.query(&session.session_id, Some(old_id)).await?;
                    assert!(closed.active_turn.is_none(), "加载配置前必须关闭旧 pending");
                    assert!(matches!(
                        closed.requested_turn,
                        Some(TurnObservation::Committed(ref turn))
                            if turn.turn_id == old_id && turn.origin == crate::ClosureOrigin::Reconciled
                    ));
                    Ok(frozen(workspace.path(), &provider.endpoint))
                },
            ).await.unwrap();
        assert_eq!(loads.get(), 1);
        assert_eq!(acceptance, Acceptance::Accepted { turn_id: new_id });
        let mut observation = client
            .subscribe(&session.session_id, new_id, None)
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(10), async {
                loop {
                    if let Observation::Finished { view, error, .. } = observation.next().await.unwrap().unwrap() {
                        assert!(error.is_none());
                        assert!(matches!(view.requested_turn, Some(TurnObservation::Committed(ref turn))
                            if turn.turn_id == new_id && matches!(turn.outcome, crate::TurnOutcome::Completed { .. })));
                        break;
                    }
                }
            }).await.unwrap();
        assert_eq!(
            provider.finish().await.len(),
            1,
            "只执行新任务，不重放旧任务"
        );
    } else if operation == "probe" {
        let new_id = TurnId::parse(&uuid::Uuid::new_v4().to_string()).unwrap();
        let result = client
            .lookup_or_recover(TurnRequest {
                session_id: session.session_id.clone(),
                turn_id: new_id,
                workspace: root.canonical_path().into(),
                input: "新的输入".into(),
            })
            .await
            .unwrap();
        assert_eq!(result, Acceptance::NeedsConfiguration);
        assert!(
            client
                .query(&session.session_id, Some(new_id))
                .await
                .unwrap()
                .requested_turn
                .is_none()
        );
    } else {
        client
            .write_session(
                &session.session_id,
                SessionWrite::SetAgent {
                    name: "新代理".into(),
                },
            )
            .await
            .unwrap();
    }
    let after = client
        .query(&session.session_id, Some(old_id))
        .await
        .unwrap();
    assert!(after.active_turn.is_none());
    assert!(
        matches!(after.requested_turn, Some(TurnObservation::Committed(turn)) if turn.origin == crate::ClosureOrigin::Reconciled)
    );
    client.stop().await.unwrap();
    serving.await.unwrap().unwrap();
}
