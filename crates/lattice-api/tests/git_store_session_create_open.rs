// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 原创建/重开身份风险改为首次接受原子建 Session；label/branch 已按 ADR 0006 删除。
use lattice_api::local::*;
use lattice_api::{RunTurnRequest, SessionApi, StateLayout, TurnAcceptance, TurnCompletion};
use lattice_session::store::{PublicTurnOutcome, SessionStore};
use std::{sync::Arc, time::Duration};

#[tokio::test]
async fn first_acceptance_reopen_preserves_identity_without_observers_or_configuration() {
    let state = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let layout = StateLayout::in_state_root(state.path());
    let store = Arc::new(SessionStore::create(&layout).unwrap());
    let api = SessionApi::from_store(store.clone());
    let profile = project.path().join("agent.toml");
    std::fs::write(
        &profile,
        "schema_version=1\n[agent]\nname='alpha'\nmodel='test'\n[prompt]\nsystem='frozen'\n",
    )
    .unwrap();
    let llm = Arc::new(
        lattice_agent::testing::FakeStreamingLlmClient::builder()
            .push_events(vec![
                lattice_llm::LlmEvent::Token {
                    content: "answer".into(),
                },
                lattice_llm::LlmEvent::Done {
                    finish_reason: "stop".into(),
                    usage: None,
                },
            ])
            .build(),
    );
    let request = RunTurnRequest {
        input: " original \n".into(),
        session_id: None,
        agent: Some("alpha".into()),
        loaded_profile: Some(lattice_profile::load_agent_profile(&profile).unwrap()),
        cancel_flag: None,
        test_llm: Some((llm.clone(), lattice_llm::ProviderKind::OpenAiCompatible)),
        test_project_root: None,
    };
    let root = lattice_api::LocalWorkspaceRoot::from_path(project.path()).unwrap();
    let id = new_turn_id();
    let execution = lattice_api::start_turn(
        api.clone(),
        layout.clone(),
        id,
        root.clone(),
        request.clone(),
        None,
    );
    let TurnAcceptance::Accepted { turn } = execution.acceptance.await.unwrap().unwrap() else {
        panic!("not accepted")
    };
    let session_id = turn.session_id;
    let TurnCompletion::Closed(completed) = execution.completion.await.unwrap().unwrap() else {
        panic!("not closed")
    };
    assert_eq!(
        completed.summary.as_ref().unwrap().outcome,
        PublicTurnOutcome::Completed
    );
    drop(execution.cancel_flag);
    drop(api);
    drop(store);
    std::fs::write(
        &profile,
        "schema_version=1\n[agent]\nname='alpha'\nmodel='changed'\n[prompt]\nsystem='changed'\n",
    )
    .unwrap();
    let store = Arc::new(SessionStore::open(&layout).unwrap());
    let api = SessionApi::from_store(store.clone());
    let mut retry = request;
    retry.loaded_profile = None;
    retry.agent = None;
    retry.test_llm = None;
    let execution = lattice_api::start_turn(api, layout, id, root, retry, None);
    let TurnAcceptance::Existing { turn } = execution.acceptance.await.unwrap().unwrap() else {
        panic!("not found")
    };
    assert_eq!(turn.session_id, session_id);
    assert_eq!(turn, *completed);
    assert!(matches!(
        execution.completion.await.unwrap().unwrap(),
        TurnCompletion::Closed(_)
    ));
    assert_eq!(llm.call_count(), 1);
    let read = store.begin_read().unwrap();
    assert_eq!(read.list(None, 10).unwrap().items.len(), 1);
    assert_eq!(read.log(session_id).unwrap().nodes.len(), 3);
    assert_eq!(
        read.turn_record(id).unwrap().unwrap().original_input,
        " original \n"
    );
    let record = read.turn_record(id).unwrap().unwrap();
    assert_eq!(record.config.model, "test");
    assert_eq!(record.config.system_prompt.as_deref(), Some("frozen"));
    let snapshot: serde_json::Value =
        serde_json::from_str(&record.config.profile_snapshot_json).unwrap();
    assert_eq!(snapshot["agent_profile"]["agent"]["name"], "alpha");
    assert_eq!(snapshot["agent_profile"]["agent"]["model"], "test");
    assert_eq!(snapshot["system_prompt"], "frozen");
}

#[tokio::test]
async fn first_probe_and_readonly_requests_never_initialize_missing_store() {
    let state = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let layout = StateLayout::in_state_root(state.path());
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

#[tokio::test]
async fn readonly_finished_delivers_full_history_and_rewind_only_moves_current() {
    use lancet_protocol::ModelAttemptKey;
    use lattice_conversation::{
        MessagePart, MessageRole, MessageSemantic, NativeMessage, ProviderOrigin,
    };
    use lattice_session::store::*;
    let state = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let layout = StateLayout::in_state_root(state.path());
    let session_id = SessionId::parse("00000000000000000000000000000001").unwrap();
    let id = new_turn_id();
    let root = lattice_api::LocalWorkspaceRoot::from_path(project.path()).unwrap();
    let history_path = layout
        .projects_dir()
        .join(root.store_key().to_string())
        .join("workspace-history.git");
    std::fs::create_dir_all(history_path.parent().unwrap()).unwrap();
    std::fs::write(&history_path, "deliberately invalid; closure must not read").unwrap();
    let store = SessionStore::create(&layout).unwrap();
    let guard = store.try_lock_session(session_id).unwrap();
    store
        .accept_turn(
            &guard,
            AcceptTurn {
                session: AcceptSession::New { id: session_id },
                turn_id: id,
                project_store_key: root.store_key(),
                input: "question".into(),
                accepted_at: chrono::Utc::now(),
                agent_selection: AgentSelection::Explicit,
                config: FrozenTurnConfig {
                    agent_name: "alpha".into(),
                    model: "test".into(),
                    provider: "openai_compatible".into(),
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
            },
        )
        .unwrap();
    let key = ModelAttemptKey {
        turn_id: id,
        model_step_ordinal: 0,
        attempt_ordinal: 0,
    };
    store
        .start_request(&guard, key, chrono::Utc::now())
        .unwrap();
    store
        .close_step(
            &guard,
            CloseStep::Finish(CompleteReply {
                key,
                origin: ProviderOrigin::OpenAiCompatible,
                usage: None,
                message: NativeMessage::new(
                    MessageRole::Assistant,
                    Some(ProviderOrigin::OpenAiCompatible),
                    vec![MessagePart::semantic_only(MessageSemantic::Text {
                        text: "answer 中".into(),
                    })],
                ),
            }),
            chrono::Utc::now(),
        )
        .unwrap();
    drop(guard);
    drop(store);
    let serving = tokio::spawn(serve(layout.clone()));
    let client = LocalClient::new(layout);
    tokio::time::timeout(Duration::from_secs(5), async {
        while client.status().await.is_err() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let target = TurnTarget {
        session_id: None,
        turn_id: id,
    };
    let mut subscription = client.subscribe(target, None).await.unwrap();
    let Some(Observation::Snapshot { view }) = subscription.next().await.unwrap() else {
        panic!("summary snapshot first")
    };
    assert_eq!(view.selected_turn.as_ref().unwrap().steps.len(), 1);
    assert_eq!(
        view.selected_turn
            .as_ref()
            .unwrap()
            .summary
            .as_ref()
            .unwrap()
            .outcome,
        PublicTurnOutcome::Completed
    );
    let Some(Observation::Finished { history, .. }) = subscription.next().await.unwrap() else {
        panic!("complete history second")
    };
    assert_eq!(history.entries.len(), 2);
    assert_eq!(history.entries[1].node, NodeId(2));
    assert!(
        matches!(&history.entries[1].content, Node::Step { content, .. } if content.assistant.parts.iter().any(|p| matches!(&p.semantic, MessageSemantic::Text { text } if text == "answer 中")))
    );
    let mut local = client.observe(None, Some(id));
    let mut saw_history = false;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match local.next().await.unwrap() {
                LocalUpdate::History {
                    result: Ok(history),
                    stale: false,
                    ..
                } => {
                    assert_eq!(history.entries.len(), 2);
                    saw_history = true;
                }
                LocalUpdate::Finished { .. } => {
                    assert!(saw_history);
                    break;
                }
                LocalUpdate::Connection(ConnectionStatus::Error(error)) => panic!("{error}"),
                _ => {}
            }
        }
    })
    .await
    .unwrap();
    local.detach().await;
    let before = client.turn(target).await.unwrap();
    let _workspace_lock = lattice_store::acquire_flock(
        &client_layout(state.path(), root.store_key()),
        lattice_store::LockMode::Exclusive,
        lattice_store::Blocking::NonBlocking,
    )
    .unwrap();
    assert_eq!(
        client
            .rewind(session_id, NodeId(0), root.canonical_path().into())
            .await
            .unwrap()
            .current,
        NodeId(0)
    );
    assert_eq!(client.turn(target).await.unwrap(), before);
    assert_eq!(
        std::fs::read_to_string(history_path).unwrap(),
        "deliberately invalid; closure must not read"
    );
    assert_eq!(client.log(session_id).await.unwrap().nodes.len(), 3);
    assert!(
        client
            .audit_replay(root.canonical_path().into(), session_id, id)
            .await
            .unwrap()
            .calls
            .is_empty()
    );
    client.stop().await.unwrap();
    serving.await.unwrap().unwrap();
}

fn client_layout(state: &std::path::Path, key: lattice_api::ProjectStoreKey) -> std::path::PathBuf {
    StateLayout::in_state_root(state).project_lock(&key, "workspace.lock")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn durable_identity_lookup_precedes_another_turns_busy_admission() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let state = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let layout = StateLayout::in_state_root(state.path());
    let root = lattice_api::LocalWorkspaceRoot::from_path(project.path()).unwrap();
    let profile = project.path().join("agent.toml");
    std::fs::write(
        &profile,
        "schema_version=1\n[agent]\nname='alpha'\nmodel='compat:test'\n[prompt]\nsystem='frozen'\n",
    )
    .unwrap();
    let store = Arc::new(SessionStore::create(&layout).unwrap());
    let llm = Arc::new(
        lattice_agent::testing::FakeStreamingLlmClient::builder()
            .push_events(vec![
                lattice_llm::LlmEvent::Token {
                    content: "first".into(),
                },
                lattice_llm::LlmEvent::Done {
                    finish_reason: "stop".into(),
                    usage: None,
                },
            ])
            .build(),
    );
    let id = new_turn_id();
    let execution = lattice_api::start_turn(
        SessionApi::from_store(store.clone()),
        layout.clone(),
        id,
        root.clone(),
        RunTurnRequest {
            input: "first".into(),
            session_id: None,
            agent: Some("alpha".into()),
            loaded_profile: Some(lattice_profile::load_agent_profile(&profile).unwrap()),
            cancel_flag: None,
            test_llm: Some((llm, lattice_llm::ProviderKind::OpenAiCompatible)),
            test_project_root: None,
        },
        None,
    );
    let TurnAcceptance::Accepted { turn } = execution.acceptance.await.unwrap().unwrap() else {
        panic!("not accepted")
    };
    execution.completion.await.unwrap().unwrap();
    drop(store);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
    let (arrived, arrival) = tokio::sync::oneshot::channel();
    let (release, wait) = tokio::sync::oneshot::channel();
    let provider = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut header = Vec::new();
        while !header.ends_with(b"\r\n\r\n") {
            header.push(socket.read_u8().await.unwrap());
            assert!(header.len() < 65536);
        }
        let header = String::from_utf8(header).unwrap();
        let length: usize = header
            .lines()
            .find_map(|l| {
                l.to_lowercase()
                    .strip_prefix("content-length:")
                    .map(|v| v.trim().parse().unwrap())
            })
            .unwrap();
        let mut body = vec![0; length];
        socket.read_exact(&mut body).await.unwrap();
        arrived.send(()).unwrap();
        wait.await.unwrap();
        let body = "data: {\"choices\":[{\"delta\":{\"content\":\"second\"},\"finish_reason\":null}]}\n\ndata: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n";
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        socket.write_all(response.as_bytes()).await.unwrap();
    });
    let serving = tokio::spawn(serve(layout.clone()));
    let client = LocalClient::new(layout);
    tokio::time::timeout(Duration::from_secs(5), async {
        while client.status().await.is_err() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let connection = lattice_llm::LlmConnection::new(
        lattice_llm::ResolvedModel {
            alias: "compat:test".into(),
            provider: lattice_llm::ProviderKind::OpenAiCompatible,
            provider_model: "test".into(),
            base_url: endpoint,
            api_key_env: String::new(),
            credential_status: lattice_llm::CredentialStatus::Present,
        },
        "fixture-key".into(),
        vec![],
    )
    .unwrap();
    let frozen = FrozenConfiguration::capture(
        lattice_profile::load_agent_profile(&profile).unwrap(),
        &root,
        connection,
    )
    .unwrap();
    let second = TurnRequest {
        session_id: Some(turn.session_id),
        turn_id: new_turn_id(),
        workspace: root.canonical_path().into(),
        input: "second".into(),
        agent: None,
    };
    let requests: Vec<_> = (0..4)
        .map(|_| TurnRequest {
            turn_id: new_turn_id(),
            ..second.clone()
        })
        .collect();
    let configuration = serde_json::to_value(frozen).unwrap();
    let submissions = futures::future::join_all(requests.iter().cloned().map(|request| {
        client.submit(
            request,
            Some(serde_json::from_value(configuration.clone()).unwrap()),
        )
    }))
    .await;
    let mut winners = Vec::new();
    let mut rejected = Vec::new();
    for (request, response) in requests.iter().zip(submissions) {
        match response {
            Ok(Acceptance::Accepted { turn }) => winners.push(turn),
            Err(error) if error.kind == LocalErrorKind::Busy => rejected.push(request.turn_id),
            other => panic!("unexpected concurrent acceptance: {other:?}"),
        }
    }
    assert_eq!(winners.len(), 1);
    assert_eq!(rejected.len(), 3);
    for turn_id in rejected {
        assert_eq!(
            client
                .turn(TurnTarget {
                    session_id: Some(turn.session_id),
                    turn_id
                })
                .await
                .unwrap_err()
                .kind,
            LocalErrorKind::NotFound
        );
    }
    assert_eq!(
        client
            .rewind(
                turn.session_id,
                lattice_session::store::NodeId(0),
                root.canonical_path().into()
            )
            .await
            .unwrap_err()
            .kind,
        LocalErrorKind::Busy
    );
    tokio::time::timeout(Duration::from_secs(5), arrival)
        .await
        .unwrap()
        .unwrap();
    let result = client
        .submit(
            TurnRequest {
                session_id: Some(turn.session_id),
                turn_id: id,
                workspace: root.canonical_path().into(),
                input: "first".into(),
                agent: None,
            },
            None,
        )
        .await;
    // 先排空自有后台，再报告断言，失败也不遗留在途 provider。
    release.send(()).unwrap();
    provider.await.unwrap();
    client.stop().await.unwrap();
    serving.await.unwrap().unwrap();
    let store = SessionStore::open(&StateLayout::in_state_root(state.path())).unwrap();
    let read = store.begin_read().unwrap();
    assert_eq!(read.log(turn.session_id).unwrap().nodes.len(), 5);
    assert_eq!(
        read.turn_detail(turn.session_id, winners[0].turn_id)
            .unwrap()
            .summary
            .unwrap()
            .outcome,
        PublicTurnOutcome::Completed
    );
    assert!(
        matches!(result, Ok(Acceptance::Existing { ref turn }) if turn.turn_id == id),
        "durable lookup returned {result:?}"
    );
}
