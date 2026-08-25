// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Behavioral tests for the live `stream_turn` path.
//!
//! These tests exercise the full live path: real SessionApi, one-attempt
//! [`ModelStepDriver`] over FakeStreamingLlmClient, and stream_turn_live.

use std::sync::Arc;

use super::types::{RunTurnRequest, TurnEvent, TurnEventStream};
use crate::session::SessionApi;
use futures::StreamExt;
use lattice_agent::ModelStepDriver;
use lattice_agent::testing::FakeStreamingLlmClient;
use lattice_llm::{LlmEvent, TokenUsage};

const TEST_SESSION_ID: &str = "0123456789abcdef0123456789abcdef";

fn make_session(dir: &std::path::Path) -> SessionApi {
    let layout = lattice_session::StateLayout::in_state_root(dir);
    let sessions_path = layout.lattice_root().join("sessions.git");
    let key = crate::root::LocalWorkspaceRoot::resolve()
        .expect("resolve cwd")
        .store_key();
    crate::stores::create_project_stores(&layout, key).expect("create project stores");
    let api =
        SessionApi::git_create_with_layout(sessions_path, &layout).expect("create session store");
    let _ = api
        .store()
        .create_session(TEST_SESSION_ID, None, "test", "main", &key);
    api
}

fn open_session(dir: &std::path::Path) -> SessionApi {
    let layout = lattice_session::StateLayout::in_state_root(dir);
    SessionApi::git_open_with_layout(layout.lattice_root().join("sessions.git"), &layout)
        .expect("open session store")
}

fn make_model_driver(client: FakeStreamingLlmClient) -> ModelStepDriver {
    ModelStepDriver::new(Arc::new(client))
}

fn begin_pending_turn(
    session: &SessionApi,
    project_store_key: lattice_session::ProjectStoreKey,
) -> lattice_session::PendingTurnTip {
    let store = session.git_store_for_coordinator().expect("turn store");
    let (_, lock) = session
        .lock_execution(TEST_SESSION_ID)
        .expect("session execution lock");
    store
        .begin_pending_turn(
            lattice_session::pending::TurnStartedFacts {
                turn_id: lancet_protocol::TurnId::parse("11111111-1111-4111-8111-111111111111")
                    .unwrap(),
                session_id: lancet_protocol::SessionId::parse(TEST_SESSION_ID).unwrap(),
                project_store_key,
                parent_conversation_tip: None,
                user_message: lattice_conversation::NativeMessage::new(
                    lattice_conversation::MessageRole::User,
                    None,
                    vec![lattice_conversation::MessagePart::semantic_only(
                        lattice_conversation::MessageSemantic::Text {
                            text: "recover me".to_string(),
                        },
                    )],
                ),
                config: lattice_session::pending::FrozenTurnConfig {
                    agent_name: "test".to_string(),
                    model: "sonnet".to_string(),
                    provider: "test".to_string(),
                    system_prompt: Some("System from profile.".to_string()),
                    profile_snapshot_json: "{}".to_string(),
                    policy_snapshot_json: "{}".to_string(),
                    catalog: lattice_session::pending::ToolCatalogSnapshot::new(
                        "test-build".to_string(),
                        Vec::new(),
                    )
                    .unwrap(),
                    limits: lattice_session::pending::TurnExecutionLimits {
                        timeout_secs: 300,
                        max_model_steps: lattice_session::pending::MAX_MODEL_STEPS,
                        max_model_attempts: 1,
                    },
                },
                accepted_at: "2026-08-24T00:00:00Z".to_string(),
            },
            &lock,
        )
        .expect("begin pending Turn")
}

async fn stream_turn_live_with_agent(
    session: SessionApi,
    model_driver: ModelStepDriver,
    request: RunTurnRequest,
    tool_definitions: Vec<lattice_llm::ToolDefinition>,
) -> crate::session::Result<TurnEventStream> {
    let (cep_client, _server) = lancet_native::CepTransport::pair(1);
    let tool_catalog: Vec<lancet_api::ToolDefinition> = tool_definitions
        .iter()
        .map(|tool| {
            lancet_api::ToolDefinition::new(
                tool.name.clone(),
                tool.description.clone(),
                tool.parameters.clone(),
                "0".repeat(64),
            )
        })
        .collect();
    let deps = super::cep::ProductionTurnDeps {
        workspace_history: Arc::new(lancet_api::WorkspaceHistory::disabled()),
        model_driver,
        tool_catalog,
        runtime: lancet_api::ServerLifecycle::noop(),
        cep_client,
        #[cfg(test)]
        build_metrics: super::cep::CepRuntimeBuildMetrics {
            client_instances: 0,
            transport_pairs: 0,
        },
    };

    super::live::stream_turn_live(session, request, deps).await
}

fn make_request(input: &str) -> RunTurnRequest {
    RunTurnRequest {
        input: input.to_string(),
        session_id: TEST_SESSION_ID.to_string(),
        loaded_profile: Some(make_loaded_profile("sonnet", "System from profile.")),
        cancel_flag: None,
        test_llm: None,
        test_project_root: None,
    }
}

fn make_request_with_session(input: &str, session_id: &str) -> RunTurnRequest {
    RunTurnRequest {
        input: input.to_string(),
        session_id: session_id.to_string(),
        loaded_profile: Some(make_loaded_profile("sonnet", "System from profile.")),
        cancel_flag: None,
        test_llm: None,
        test_project_root: None,
    }
}

fn make_loaded_profile(model: &str, system_prompt: &str) -> lattice_profile::LoadedAgentProfile {
    lattice_profile::LoadedAgentProfile {
        path: std::path::PathBuf::from("/tmp/test/agent.toml"),
        package_dir: std::env::current_dir().unwrap_or_else(|_| "/tmp".into()),
        profile: lattice_profile::AgentProfile {
            schema_version: 1,
            agent: lattice_profile::AgentSection {
                name: "test".to_string(),
                model: model.to_string(),
                description: None,
            },
            prompt: lattice_profile::PromptSection {
                system: Some(system_prompt.to_string()),
                system_path: None,
            },
            tools: lattice_profile::ToolsSection { enabled: vec![] },
            llm: lattice_profile::LlmSection::default(),
            lancet: lattice_profile::LancetSection::default(),
        },
        system_prompt: system_prompt.to_string(),
    }
}

fn happy_events() -> Vec<LlmEvent> {
    vec![
        LlmEvent::Token {
            content: "Hello, world!".to_string(),
        },
        LlmEvent::Done {
            finish_reason: "stop".to_string(),
            usage: Some(TokenUsage {
                prompt_tokens: 10,
                completion_tokens: 5,
                total_tokens: 15,
            }),
        },
    ]
}

#[tokio::test]
async fn recovery_rejects_the_wrong_workspace_before_any_side_effect() {
    let dir = tempfile::tempdir().unwrap();
    let session = make_session(dir.path());
    let layout = session.state_layout().unwrap();
    let workspace = crate::root::LocalWorkspaceRoot::resolve().unwrap();
    let project_key = workspace.store_key();
    let pending_before = begin_pending_turn(&session, project_key);
    let ledger = lancet_audit::invocation_store::InvocationStore::open(&layout, &project_key)
        .expect("invocation store");
    let ledger_before = ledger.pinned_view().unwrap().commit_oid;
    let session_before = session
        .open_session(crate::session::OpenSessionRequest {
            id: TEST_SESSION_ID.to_string(),
        })
        .unwrap();
    let history = lancet_api::WorkspaceHistory::open(lancet_api::WorkspaceHistoryBacking::for_key(
        layout,
        project_key,
    ))
    .unwrap();
    let history_before = history.current().unwrap();

    let other_workspace = tempfile::tempdir().unwrap();
    let sentinel = other_workspace.path().join("sentinel.txt");
    std::fs::write(&sentinel, "unchanged").unwrap();
    let client = Arc::new(
        FakeStreamingLlmClient::builder()
            .push_events(happy_events())
            .build(),
    );
    let mut request = make_request("wrong workspace");
    request.test_llm = Some(client.clone());
    request.test_project_root = Some(other_workspace.path().to_string_lossy().into_owned());

    let err = match super::execute::stream_turn(session.clone(), request).await {
        Ok(_) => panic!("a session bound to another workspace must fail closed"),
        Err(err) => err,
    };
    assert!(
        matches!(err, crate::session::ApiError::Conflict(_)),
        "got {err:?}"
    );
    assert_eq!(client.call_count(), 0, "provider must not be requested");
    assert_eq!(ledger.pinned_view().unwrap().commit_oid, ledger_before);
    assert_eq!(history.current().unwrap(), history_before);
    assert_eq!(std::fs::read_to_string(sentinel).unwrap(), "unchanged");
    assert_eq!(
        session
            .git_store_for_coordinator()
            .unwrap()
            .read_active_turn(TEST_SESSION_ID)
            .unwrap()
            .unwrap()
            .oid,
        pending_before.oid
    );
    assert_eq!(
        session
            .open_session(crate::session::OpenSessionRequest {
                id: TEST_SESSION_ID.to_string(),
            })
            .unwrap(),
        session_before
    );
}

#[tokio::test]
async fn recovery_strict_open_failure_does_not_advance_the_session_or_ledger() {
    let dir = tempfile::tempdir().unwrap();
    let session = make_session(dir.path());
    let layout = session.state_layout().unwrap();
    let workspace = crate::root::LocalWorkspaceRoot::resolve().unwrap();
    let project_key = workspace.store_key();
    let pending_before = begin_pending_turn(&session, project_key);
    let ledger = lancet_audit::invocation_store::InvocationStore::open(&layout, &project_key)
        .expect("invocation store");
    let ledger_before = ledger.pinned_view().unwrap().commit_oid;
    let history_backing = lancet_api::WorkspaceHistoryBacking::for_key(layout, project_key);
    std::fs::remove_dir_all(history_backing.store_path().unwrap()).unwrap();
    let client = Arc::new(
        FakeStreamingLlmClient::builder()
            .push_events(happy_events())
            .build(),
    );
    let mut request = make_request("strict reopen");
    request.test_llm = Some(client.clone());
    request.test_project_root = Some(workspace.canonical_path().to_string_lossy().into_owned());

    let err = match super::execute::stream_turn(session.clone(), request).await {
        Ok(_) => panic!("missing WorkspaceHistory must fail closed"),
        Err(err) => err,
    };
    assert!(err.to_string().contains("workspace history setup"), "{err}");
    assert_eq!(client.call_count(), 0, "provider must not be requested");
    assert_eq!(ledger.pinned_view().unwrap().commit_oid, ledger_before);
    assert_eq!(
        session
            .git_store_for_coordinator()
            .unwrap()
            .read_active_turn(TEST_SESSION_ID)
            .unwrap()
            .unwrap()
            .oid,
        pending_before.oid
    );
}

#[tokio::test]
async fn valid_recovery_closes_the_pending_turn_then_runs_the_new_turn() {
    let dir = tempfile::tempdir().unwrap();
    let session = make_session(dir.path());
    let workspace = crate::root::LocalWorkspaceRoot::resolve().unwrap();
    begin_pending_turn(&session, workspace.store_key());
    let client = Arc::new(
        FakeStreamingLlmClient::builder()
            .push_events(happy_events())
            .build(),
    );
    let mut request = make_request("after recovery");
    request.test_llm = Some(client.clone());
    request.test_project_root = Some(workspace.canonical_path().to_string_lossy().into_owned());

    let events: Vec<_> = super::execute::stream_turn(session.clone(), request)
        .await
        .expect("valid recovery starts the new Turn")
        .collect()
        .await;
    assert!(
        events
            .iter()
            .any(|event| matches!(event, TurnEvent::TurnCommitted { .. }))
    );
    assert_eq!(client.call_count(), 1);
    assert!(
        session
            .git_store_for_coordinator()
            .unwrap()
            .read_active_turn(TEST_SESSION_ID)
            .unwrap()
            .is_none()
    );
    let log = session.log(TEST_SESSION_ID).unwrap();
    assert_eq!(log.len(), 2, "reconciled Turn plus new live Turn");
    assert_eq!(log[0].origin, lattice_session::ClosureOrigin::Reconciled);
    assert_eq!(log[1].origin, lattice_session::ClosureOrigin::Live);
}

#[tokio::test]
async fn turn_uses_the_injected_non_default_session_repository() {
    let dir = tempfile::tempdir().unwrap();
    let layout = lattice_session::StateLayout::in_state_root(dir.path());
    let workspace = crate::root::LocalWorkspaceRoot::resolve().unwrap();
    let project_key = workspace.store_key();
    crate::stores::create_project_stores(&layout, project_key).unwrap();
    let custom_repo = dir.path().join("injected").join("authoritative.git");
    let session = SessionApi::git_create_with_layout(&custom_repo, &layout).unwrap();
    session
        .store()
        .create_session(TEST_SESSION_ID, None, "test", "main", &project_key)
        .unwrap();
    let client = Arc::new(
        FakeStreamingLlmClient::builder()
            .push_events(happy_events())
            .build(),
    );
    let mut request = make_request("custom repository");
    request.test_llm = Some(client.clone());
    request.test_project_root = Some(workspace.canonical_path().to_string_lossy().into_owned());

    let events: Vec<_> = super::execute::stream_turn(session, request)
        .await
        .expect("custom repository Turn starts")
        .collect()
        .await;
    assert!(
        events
            .iter()
            .any(|event| matches!(event, TurnEvent::TurnCommitted { .. }))
    );
    assert_eq!(client.call_count(), 1);
    let reopened = SessionApi::git_open_with_layout(&custom_repo, &layout).unwrap();
    assert_eq!(reopened.log(TEST_SESSION_ID).unwrap().len(), 1);
    assert!(
        !layout.lattice_root().join("sessions.git").exists(),
        "TurnCoordinator must not reopen the layout-default session repository"
    );
}

#[tokio::test]
async fn stream_turn_live_with_agent_startup_failure_no_stream() {
    let dir = tempfile::tempdir().unwrap();
    let session = make_session(dir.path());
    let client = FakeStreamingLlmClient::builder()
        .push_events(happy_events())
        .build();
    let driver = make_model_driver(client);

    let request = make_request_with_session("hello", "non-existent-session-id");
    let result = stream_turn_live_with_agent(session, driver, request, vec![]).await;
    assert!(
        result.is_err(),
        "startup failure should return Err, not Ok with error events in stream"
    );
}

#[tokio::test]
async fn stream_turn_live_with_agent_commit_stores_delta_only() {
    let dir = tempfile::tempdir().unwrap();
    let session = make_session(dir.path());
    let client = FakeStreamingLlmClient::builder()
        .push_events(happy_events())
        .build();
    let driver = make_model_driver(client);
    let request = make_request("What is 2+2?");

    let stream = stream_turn_live_with_agent(session, driver, request, vec![])
        .await
        .expect("stream_turn_live_with_agent should succeed");
    let events: Vec<TurnEvent> = stream.collect().await;

    let (session_id, commit_id) = events
        .iter()
        .find_map(|e| {
            if let TurnEvent::TurnCommitted {
                session_id,
                commit_id,
                ..
            } = e
            {
                Some((session_id.clone(), commit_id.clone()))
            } else {
                None
            }
        })
        .expect("should have TurnCommitted");

    let session2 = make_session(dir.path());
    let log = session2.log(&session_id).expect("should read log");
    assert_eq!(log.len(), 1, "should have exactly one commit");
    assert_eq!(log[0].commit_id, commit_id);

    let transcript = session2
        .head_transcript(&session_id)
        .expect("should read transcript");
    let committed_messages = &transcript.messages;

    assert_eq!(
        committed_messages[0].role,
        lattice_conversation::MessageRole::User
    );
    assert_eq!(
        lattice_session::test_helpers::text(&committed_messages[0]),
        Some("What is 2+2?")
    );
    assert!(
        committed_messages.len() >= 2,
        "should have at least user + assistant messages"
    );
    assert_eq!(
        committed_messages[1].role,
        lattice_conversation::MessageRole::Assistant
    );

    let user_count = committed_messages
        .iter()
        .filter(|m| m.role == lattice_conversation::MessageRole::User)
        .count();
    assert_eq!(
        user_count, 1,
        "commit should contain exactly one user message (delta only)"
    );
    assert!(
        log[0].commit_message.len() <= 72,
        "commit message should be <= 72 chars, got: {} ('{}')",
        log[0].commit_message.len(),
        log[0].commit_message
    );

    let session3 = make_session(dir.path());
    let client2 = FakeStreamingLlmClient::builder()
        .push_events(happy_events())
        .build();
    let driver2 = make_model_driver(client2);
    let request2 = make_request_with_session("follow-up question", &session_id);

    let stream2 = stream_turn_live_with_agent(session3, driver2, request2, vec![])
        .await
        .expect("second turn should succeed");
    let events2: Vec<TurnEvent> = stream2.collect().await;

    let commit_id2 = events2
        .iter()
        .find_map(|e| {
            if let TurnEvent::TurnCommitted { commit_id, .. } = e {
                Some(commit_id.clone())
            } else {
                None
            }
        })
        .unwrap_or_else(|| panic!("second turn should have TurnCommitted: {events2:#?}"));

    let session4 = make_session(dir.path());
    let log2 = session4.log(&session_id).expect("should read log");
    assert_eq!(log2.len(), 2, "should have two commits after two turns");
    assert_eq!(log2[1].commit_id, commit_id2);

    let transcript2 = session4
        .head_transcript(&session_id)
        .expect("should read transcript after second turn");
    let all_messages = &transcript2.messages;
    assert_eq!(
        all_messages.len(),
        4,
        "HEAD transcript should have 4 messages (2 user + 2 assistant), got {}",
        all_messages.len()
    );
    assert_eq!(
        all_messages[2].role,
        lattice_conversation::MessageRole::User
    );
    assert_eq!(
        lattice_session::test_helpers::text(&all_messages[2]),
        Some("follow-up question")
    );
    assert_eq!(
        all_messages[3].role,
        lattice_conversation::MessageRole::Assistant
    );
    assert_eq!(
        lattice_session::test_helpers::text(&all_messages[0]),
        Some("What is 2+2?")
    );
}

#[tokio::test]
async fn stream_turn_live_with_agent_receiver_drop_does_not_cancel() {
    let dir = tempfile::tempdir().unwrap();
    let session = make_session(dir.path());
    let client = FakeStreamingLlmClient::builder()
        .push_events(happy_events())
        .build();
    let driver = make_model_driver(client);
    let request = make_request("receiver drop test");

    let session_id = {
        let mut stream = stream_turn_live_with_agent(session, driver, request, vec![])
            .await
            .expect("stream_turn_live_with_agent should succeed");
        let first_event = stream.next().await.expect("should get TurnStarted");
        if let TurnEvent::TurnStarted { session_id, .. } = first_event {
            session_id
        } else {
            panic!("first event should be TurnStarted");
        }
    };

    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let session = open_session(dir.path());
            if session.log(&session_id).map(|log| log.len()).unwrap_or(0) == 1 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("background task should commit after receiver drop");
}

fn flood_token_events(count: usize) -> Vec<LlmEvent> {
    let mut events: Vec<LlmEvent> = (0..count)
        .map(|i| LlmEvent::Token {
            content: format!("tok{i}"),
        })
        .collect();
    events.push(LlmEvent::Done {
        finish_reason: "stop".to_string(),
        usage: Some(TokenUsage {
            prompt_tokens: 1,
            completion_tokens: count as u64,
            total_tokens: count as u64 + 1,
        }),
    });
    events
}

#[tokio::test]
async fn stream_turn_live_with_agent_slow_observer_still_commits() {
    let dir = tempfile::tempdir().unwrap();
    let session = make_session(dir.path());
    let client = FakeStreamingLlmClient::builder()
        .push_events(flood_token_events(400))
        .build();
    let driver = make_model_driver(client);
    let request = make_request("slow observer");

    let session_id = {
        let mut stream = stream_turn_live_with_agent(session, driver, request, vec![])
            .await
            .expect("stream_turn_live_with_agent should succeed");
        let first = stream.next().await.expect("TurnStarted");
        match first {
            TurnEvent::TurnStarted { session_id, .. } => session_id,
            other => panic!("expected TurnStarted, got {other:?}"),
        }
    };
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            let session = open_session(dir.path());
            if session.log(&session_id).map(|log| log.len()).unwrap_or(0) == 1 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("turn must commit despite slow observer");
}
