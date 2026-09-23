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
    assert!(tool_definitions.is_empty());
    let layout = session.state_layout().expect("test StateLayout");
    let root = crate::root::LocalWorkspaceRoot::resolve().expect("resolve cwd");
    let backing = lancet_api::WorkspaceHistoryBacking::for_key(layout, root.store_key());
    let mut runtime = lancet_api::LancetRuntime::build(lancet_api::LancetRuntimeConfig {
        local_workspace_root: root.canonical_path().to_path_buf(),
        enabled_tools: Vec::new(),
        policy: lancet_api::PolicyConfig::defaults_for_project_root(
            root.canonical_path().to_path_buf(),
        ),
        capability_admission: Default::default(),
        tls_roots_der: Vec::new(),
        invocations: backing.clone(),
        workspace_history: backing,
    })
    .expect("build direct Component runtime");
    let tool_catalog = runtime.tool_definitions().to_vec();
    let workspace_history = Arc::clone(runtime.workspace_history());
    let events = runtime.take_events();
    let deps = super::cep::ProductionTurnDeps {
        workspace_history,
        model_driver,
        tool_catalog,
        runtime,
        events,
    };

    super::live::stream_turn_live(session, request, deps, lattice_llm::ProviderKind::Anthropic)
        .await
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
    request.test_llm = Some((client.clone(), lattice_llm::ProviderKind::Anthropic));
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
    request.test_llm = Some((client.clone(), lattice_llm::ProviderKind::Anthropic));
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
async fn valid_recovery_reports_only_the_pending_turn_without_a_profile() {
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
    request.loaded_profile = None;
    request.test_llm = Some((client.clone(), lattice_llm::ProviderKind::Anthropic));
    request.test_project_root = Some(workspace.canonical_path().to_string_lossy().into_owned());
    let events: Vec<_> = super::execute::stream_turn(session.clone(), request)
        .await
        .expect("report the recovered Turn")
        .collect()
        .await;
    assert!(
        events
            .iter()
            .any(|event| matches!(event, TurnEvent::TurnCommitted { .. }))
    );
    assert_eq!(client.call_count(), 0);
    assert!(
        session
            .git_store_for_coordinator()
            .unwrap()
            .read_active_turn(TEST_SESSION_ID)
            .unwrap()
            .is_none()
    );
    let log = session.log(TEST_SESSION_ID).unwrap();
    assert_eq!(log.len(), 1, "recovery must not accept the new prompt");
    assert_eq!(log[0].origin, lattice_session::ClosureOrigin::Reconciled);
    assert_eq!(
        log[0].workspace_history_checkpoint,
        lattice_session::WorkspaceHistoryCheckpoint::Empty
    );
}

#[tokio::test]
async fn accepted_turn_finishes_without_observers_and_replays_without_a_profile() {
    let dir = tempfile::tempdir().unwrap();
    let session = make_session(dir.path());
    let client = Arc::new(
        FakeStreamingLlmClient::builder()
            .push_events(happy_events())
            .push_events(happy_events())
            .build(),
    );
    let id = lancet_protocol::TurnId::parse("22222222-2222-4222-8222-222222222222").unwrap();
    let mut request = make_request("keep running");
    request.test_llm = Some((client.clone(), lattice_llm::ProviderKind::Anthropic));
    let execution = super::execute::start_turn(
        session.clone(),
        id,
        crate::root::LocalWorkspaceRoot::resolve().unwrap(),
        request.clone(),
        None,
    );
    assert!(matches!(
        execution.acceptance.await.unwrap().unwrap(),
        super::types::TurnAcceptance::Accepted { .. }
    ));
    drop(execution.events);
    let completed = execution.completion.await.unwrap().unwrap();
    let super::types::TurnCompletion::Committed(commit) = completed else {
        panic!("expected committed")
    };
    request.loaded_profile = None;
    let replay = super::execute::start_turn(
        session.clone(),
        id,
        crate::root::LocalWorkspaceRoot::resolve().unwrap(),
        request.clone(),
        None,
    );
    assert!(matches!(
        replay.acceptance.await.unwrap().unwrap(),
        super::types::TurnAcceptance::Existing(_)
    ));
    let super::types::TurnCompletion::Committed(again) = replay.completion.await.unwrap().unwrap()
    else {
        panic!("expected replay")
    };
    assert_eq!(commit.commit_id, again.commit_id);
    assert_eq!(client.call_count(), 1);
    assert_eq!(session.log(TEST_SESSION_ID).unwrap().len(), 1);
    let mut conflicting = request.clone();
    conflicting.input = "different input".into();
    let conflict = super::execute::start_turn(
        session.clone(),
        id,
        crate::root::LocalWorkspaceRoot::resolve().unwrap(),
        conflicting,
        None,
    );
    assert!(matches!(
        conflict.acceptance.await.unwrap(),
        Err(crate::session::ApiError::Conflict(_))
    ));
    assert!(conflict.completion.await.unwrap().is_err());
    request.loaded_profile = Some(make_loaded_profile("sonnet", "System from profile."));
    let new_id = lancet_protocol::TurnId::parse("33333333-3333-4333-8333-333333333333").unwrap();
    let independent = super::execute::start_turn(
        session.clone(),
        new_id,
        crate::root::LocalWorkspaceRoot::resolve().unwrap(),
        request,
        None,
    );
    assert!(matches!(
        independent.acceptance.await.unwrap().unwrap(),
        super::types::TurnAcceptance::Accepted { .. }
    ));
    drop(independent.events);
    assert!(matches!(
        independent.completion.await.unwrap().unwrap(),
        super::types::TurnCompletion::Committed(_)
    ));
    assert_eq!(
        client.call_count(),
        2,
        "same text with a new identity is independent"
    );
}

#[tokio::test]
async fn independent_recovery_and_read_views_do_not_require_execution_configuration() {
    let dir = tempfile::tempdir().unwrap();
    let session = make_session(dir.path());
    let workspace = crate::root::LocalWorkspaceRoot::resolve().unwrap();
    let pending = begin_pending_turn(&session, workspace.store_key());
    let (_, lock) = session.try_lock_execution(TEST_SESSION_ID).unwrap();
    let view = session
        .read_view(TEST_SESSION_ID, Some(pending.snapshot.turn_id))
        .await
        .unwrap();
    assert_eq!(view.active_turn.unwrap().oid, pending.oid);
    assert!(matches!(
        super::execute::recover_turn(session.clone(), TEST_SESSION_ID.into()).await,
        Err(crate::session::ApiError::Busy(_))
    ));
    drop(lock);
    let recovered = super::execute::recover_turn(session.clone(), TEST_SESSION_ID.into())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(recovered.turn_id, pending.snapshot.turn_id);
    assert_eq!(recovered.origin, lattice_session::ClosureOrigin::Reconciled);
    assert!(
        super::execute::recover_turn(session.clone(), TEST_SESSION_ID.into())
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(session.log(TEST_SESSION_ID).unwrap().len(), 1);
}
#[tokio::test]
async fn another_session_executes_while_busy_session_rejects_new_turns() {
    let dir = tempfile::tempdir().unwrap();
    let session = make_session(dir.path());
    let workspace = crate::root::LocalWorkspaceRoot::resolve().unwrap();
    let (_, lock) = session.try_lock_execution(TEST_SESSION_ID).unwrap();
    let client = Arc::new(
        FakeStreamingLlmClient::builder()
            .push_events(happy_events())
            .build(),
    );
    let mut request = make_request("busy request");
    request.test_llm = Some((client.clone(), lattice_llm::ProviderKind::Anthropic));
    let id = lancet_protocol::TurnId::parse("44444444-4444-4444-8444-444444444444").unwrap();
    let busy = super::execute::start_turn(
        session.clone(),
        id,
        workspace.clone(),
        request.clone(),
        None,
    );
    assert!(matches!(
        tokio::time::timeout(std::time::Duration::from_secs(1), busy.acceptance)
            .await
            .unwrap()
            .unwrap(),
        Err(crate::session::ApiError::Busy(_))
    ));
    assert!(busy.completion.await.unwrap().is_err());
    assert_eq!(client.call_count(), 0);
    let other_id = "fedcba9876543210fedcba9876543210";
    session
        .store()
        .create_session(other_id, None, "test", "main", &workspace.store_key())
        .unwrap();
    request.session_id = other_id.into();
    let other = super::execute::start_turn(session.clone(), id, workspace, request, None);
    assert!(matches!(
        other.acceptance.await.unwrap().unwrap(),
        super::types::TurnAcceptance::Accepted { .. }
    ));
    drop(other.events);
    assert!(matches!(
        other.completion.await.unwrap().unwrap(),
        super::types::TurnCompletion::Committed(_)
    ));
    assert_eq!(client.call_count(), 1);
    drop(lock);
}

#[tokio::test]
async fn recovery_rejects_a_turn_closing_checkpoint_that_no_longer_matches_current() {
    let dir = tempfile::tempdir().unwrap();
    let session = make_session(dir.path());
    let layout = session.state_layout().unwrap();
    let workspace = crate::root::LocalWorkspaceRoot::resolve().unwrap();
    let key = workspace.store_key();
    let turn = lancet_protocol::TurnId::parse("11111111-1111-4111-8111-111111111111").unwrap();
    begin_pending_turn(&session, key);
    let store = session.git_store_for_coordinator().unwrap();
    let (_, lock) = session.lock_execution(TEST_SESSION_ID).unwrap();
    store
        .advance_pending_turn(
            TEST_SESSION_ID,
            lattice_session::pending::PendingStage::ModelAttemptStarted {
                key: lancet_protocol::ModelAttemptKey {
                    turn_id: turn,
                    model_step_ordinal: 0,
                    attempt_ordinal: 0,
                },
            },
            1_800_000_000,
            &lock,
        )
        .unwrap();
    let message = lattice_conversation::NativeMessage::new(
        lattice_conversation::MessageRole::Assistant,
        None,
        vec![lattice_conversation::MessagePart::semantic_only(
            lattice_conversation::MessageSemantic::Text {
                text: "done".into(),
            },
        )],
    );
    store
        .advance_pending_turn(
            TEST_SESSION_ID,
            lattice_session::pending::PendingStage::MessageComplete {
                key: lancet_protocol::ModelAttemptKey {
                    turn_id: turn,
                    model_step_ordinal: 0,
                    attempt_ordinal: 0,
                },
                message: message.clone(),
                usage: None,
            },
            1_800_000_001,
            &lock,
        )
        .unwrap();
    let closed = store
        .advance_pending_turn(
            TEST_SESSION_ID,
            lattice_session::pending::PendingStage::StepClosed {
                kind: lattice_session::pending::StepClosedKind::FinishTurn {
                    final_message: message,
                },
            },
            1_800_000_002,
            &lock,
        )
        .unwrap();
    let candidate = closed
        .snapshot
        .turn_commit_candidate_basis(
            lattice_session::TurnOutcome::Completed {
                finish_reason: lattice_session::TurnFinishReason::Stop,
            },
            lattice_session::ClosureOrigin::Live,
        )
        .unwrap()
        .with_workspace_history_checkpoint(lattice_session::WorkspaceHistoryCheckpoint::Empty)
        .unwrap();
    let closing = store
        .advance_pending_turn(
            TEST_SESSION_ID,
            lattice_session::pending::PendingStage::TurnClosing(
                lattice_session::pending::TurnClosingFacts {
                    turn_id: turn,
                    candidate,
                },
            ),
            1_800_000_003,
            &lock,
        )
        .unwrap();
    drop(lock);

    let history = lancet_runtime::WorkspaceHistory::open(
        lancet_runtime::WorkspaceHistoryBacking::for_key(layout.clone(), key),
    )
    .unwrap();
    history
        .record(&lancet_runtime::MutationRecord {
            method: "mutate.create".into(),
            turn_id: turn,
            invocation_id: lancet_protocol::InvocationId::parse(
                "22222222-2222-4222-8222-222222222222",
            )
            .unwrap(),
            path: "checkpoint.txt".into(),
            before: None,
            after: b"changed".to_vec(),
            content_hash: "test-revision".into(),
            recorded_at: 1_800_000_004,
        })
        .unwrap();
    assert!(history.current().unwrap().is_some());

    let client = Arc::new(
        FakeStreamingLlmClient::builder()
            .push_events(happy_events())
            .build(),
    );
    let mut request = make_request("must not run");
    request.test_llm = Some((client.clone(), lattice_llm::ProviderKind::Anthropic));
    request.test_project_root = Some(workspace.canonical_path().to_string_lossy().into_owned());
    let error = match super::execute::stream_turn(session.clone(), request).await {
        Ok(_) => panic!("mismatched pinned checkpoint must fail closed"),
        Err(error) => error,
    };
    assert!(
        error.to_string().contains("differs from current"),
        "{error}"
    );
    assert_eq!(client.call_count(), 0);
    assert_eq!(
        store
            .read_active_turn(TEST_SESSION_ID)
            .unwrap()
            .unwrap()
            .oid,
        closing.oid,
        "rejection must not delete or advance active-turn"
    );
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
    request.test_llm = Some((client.clone(), lattice_llm::ProviderKind::Anthropic));
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
