// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Behavioral tests for the live `stream_turn` path.
//!
//! These tests exercise the full live path: real SessionApi backed by SQLite,
//! real AgentLoop with FakeStreamingLlmClient, and the live stream_turn_live_with_agent
//! function. No internal boundaries are mocked.

use std::sync::Arc;

use super::types::{RunTurnRequest, TurnEvent, TurnEventStream};
use crate::session::SessionApi;
use crate::turn::model_rounds::{ModelRoundSequencer, ToolDispatch, ToolExecutionError};
use futures::StreamExt;
use lattice_agent::testing::FakeStreamingLlmClient;
use lattice_llm::{LlmEvent, TokenUsage};

// ── Helpers ────────────────────────────────────────────────────────────

const TEST_SESSION_ID: &str = "test-session";

/// No-op tool executor that rejects all calls (tests don't invoke tools
/// unless explicitly configured).
struct NoOpToolExecutor;

#[async_trait::async_trait]
impl ToolDispatch for NoOpToolExecutor {
    async fn execute(&self, _call: &lattice_llm::ToolCall) -> Result<String, ToolExecutionError> {
        Err(ToolExecutionError::simple("no tools configured in test"))
    }
}

fn make_session(dir: &std::path::Path) -> SessionApi {
    let api = SessionApi::git_create(dir.join("sessions.git")).expect("create session store");
    // These turns run with `test_project_root: None`, so the live workspace
    // root is the process CWD; bind the session to it.
    let key = crate::root::LocalWorkspaceRoot::resolve()
        .expect("resolve cwd")
        .store_key();
    let _ = api
        .store()
        .create_session(TEST_SESSION_ID, None, "test", "main", &key);
    api
}

fn open_session(dir: &std::path::Path) -> SessionApi {
    SessionApi::git_open(dir.join("sessions.git")).expect("open session store")
}

fn make_agent_loop(client: FakeStreamingLlmClient) -> Arc<ModelRoundSequencer> {
    Arc::new(ModelRoundSequencer::new(
        Arc::new(client),
        Arc::new(NoOpToolExecutor),
    ))
}

async fn stream_turn_live_with_agent(
    session: SessionApi,
    sequencer: Arc<ModelRoundSequencer>,
    request: RunTurnRequest,
    tool_definitions: Vec<lattice_llm::ToolDefinition>,
) -> crate::session::Result<TurnEventStream> {
    let (cep_client, _server) = lancet_native::CepTransport::pair(1);
    let deps = super::cep::ProductionTurnDeps {
        workspace_history: std::sync::Arc::new(lancet_api::WorkspaceHistory::disabled()),
        sequencer,
        tool_definitions,
        runtime: lancet_api::ServerLifecycle::noop(),
        cep_client,
        audit: Arc::new(lancet_native::CepAuditStore::in_memory().expect("in-memory audit store")),
        audit_context: Arc::new(tokio::sync::RwLock::new(
            lancet_native::CepAuditContext::default(),
        )),
        #[cfg(test)]
        build_metrics: super::cep::CepRuntimeBuildMetrics {
            adapter_instances: 0,
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
        test_persist_audit: None,
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
        test_persist_audit: None,
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

/// Standard happy-path LLM events: token output + done.
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

// ── Tests ──────────────────────────────────────────────────────────────
#[tokio::test]
async fn stream_turn_live_rejects_a_project_store_key_mismatch() {
    let dir = tempfile::tempdir().unwrap();
    let session = make_session(dir.path());
    let other_workspace = tempfile::tempdir().unwrap();
    let client = FakeStreamingLlmClient::builder()
        .push_events(happy_events())
        .build();
    let agent_loop = make_agent_loop(client);
    let mut request = make_request("wrong workspace");
    request.test_project_root = Some(other_workspace.path().to_string_lossy().into_owned());

    let err = match stream_turn_live_with_agent(session, agent_loop, request, vec![]).await {
        Ok(_) => panic!("a session bound to another workspace must fail closed"),
        Err(err) => err,
    };
    assert!(
        matches!(err, crate::session::ApiError::Conflict(_)),
        "got {err:?}"
    );
}

#[tokio::test]
async fn stream_turn_live_with_agent_startup_failure_no_stream() {
    let dir = tempfile::tempdir().unwrap();
    let session = make_session(dir.path());
    let client = FakeStreamingLlmClient::builder()
        .push_events(happy_events())
        .build();
    let agent_loop = make_agent_loop(client);

    // Pass a request with session_id pointing to a non-existent session
    let request = make_request_with_session("hello", "non-existent-session-id");

    let result = stream_turn_live_with_agent(session, agent_loop, request, vec![]).await;

    // Should return Err, not Ok with error events inside the stream.
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
    let agent_loop = make_agent_loop(client);
    let request = make_request("What is 2+2?");

    let stream = stream_turn_live_with_agent(session, agent_loop, request, vec![])
        .await
        .expect("stream_turn_live_with_agent should succeed");
    let events: Vec<TurnEvent> = stream.collect().await;

    // Extract session_id and commit_id from TurnCommitted
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

    // Re-open a fresh SessionApi from the same tempdir to read the committed data
    let session2 = make_session(dir.path());

    // Verify commit exists in log
    let log = session2.log(&session_id).expect("should read log");
    assert_eq!(log.len(), 1, "should have exactly one commit");
    assert_eq!(log[0].commit_id, commit_id);

    // Read the full transcript — since this is the first turn, the transcript
    // messages ARE the committed delta (user + agent messages only).
    let transcript = session2
        .head_transcript(&session_id)
        .expect("should read transcript");
    let committed_messages = &transcript.messages;

    // First message should be user message matching request.input
    assert_eq!(committed_messages[0].role, "user");
    assert_eq!(committed_messages[0].content, "What is 2+2?");

    // Remaining messages are agent response messages
    assert!(
        committed_messages.len() >= 2,
        "should have at least user + assistant messages"
    );
    assert_eq!(committed_messages[1].role, "assistant");

    // No prior transcript messages are duplicated
    // Since this is the first turn, there should be exactly user + agent messages
    let user_count = committed_messages
        .iter()
        .filter(|m| m.role == "user")
        .count();
    assert_eq!(
        user_count, 1,
        "commit should contain exactly one user message (delta only)"
    );

    // Commit message is <= 72 chars
    assert!(
        log[0].commit_message.len() <= 72,
        "commit message should be <= 72 chars, got: {} ('{}')",
        log[0].commit_message.len(),
        log[0].commit_message
    );

    // ── Second turn: prove delta-only ───────────────
    // A second turn on the same session must commit only its own delta,
    // not replay the first turn's messages into the commit.
    let session3 = make_session(dir.path());
    let client2 = FakeStreamingLlmClient::builder()
        .push_events(happy_events())
        .build();
    let agent_loop2 = make_agent_loop(client2);
    let request2 = make_request_with_session("follow-up question", &session_id);

    let stream2 = stream_turn_live_with_agent(session3, agent_loop2, request2, vec![])
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
        .expect("second turn should have TurnCommitted");

    // Re-open and check the log now has 2 commits
    let session4 = make_session(dir.path());
    let log2 = session4.log(&session_id).expect("should read log");
    assert_eq!(log2.len(), 2, "should have two commits after two turns");
    assert_eq!(log2[1].commit_id, commit_id2);

    // Read transcript at HEAD — should have all 4 messages (2 turns × (user + assistant))
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

    // The second commit's delta is only the second turn's messages.
    // Verify: second user message is "follow-up question", not a repeat of first.
    assert_eq!(all_messages[2].role, "user");
    assert_eq!(all_messages[2].content, "follow-up question");
    assert_eq!(all_messages[3].role, "assistant");

    // The first turn's messages are unchanged at positions 0-1
    assert_eq!(all_messages[0].content, "What is 2+2?");
}

#[tokio::test]
async fn stream_turn_live_with_agent_receiver_drop_does_not_cancel() {
    // Receiver drop does not cancel a live turn; the background
    // task completes and commits despite the stream being dropped.
    let dir = tempfile::tempdir().unwrap();
    let session = make_session(dir.path());
    let client = FakeStreamingLlmClient::builder()
        .push_events(happy_events())
        .build();
    let agent_loop = make_agent_loop(client);
    let request = make_request("receiver drop test");

    // Extract session_id from the first event, then let the stream drop
    // when the block ends — receiver gone, background task should still
    // commit.
    let session_id = {
        let mut stream = stream_turn_live_with_agent(session, agent_loop, request, vec![])
            .await
            .expect("stream_turn_live_with_agent should succeed");
        let first_event = stream.next().await.expect("should get TurnStarted");
        if let TurnEvent::TurnStarted { session_id, .. } = first_event {
            session_id
        } else {
            panic!("first event should be TurnStarted");
        }
        // stream drops here — receiver gone
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

/// Many small token events to fill the bounded public channel when the
/// consumer stops reading after `TurnStarted`.
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
            completion_tokens: count as u32,
            total_tokens: (count as u32) + 1,
        }),
    });
    events
}

#[tokio::test]
async fn stream_turn_live_with_agent_slow_observer_still_commits() {
    // a consumer that stops reading must not block commit.
    let dir = tempfile::tempdir().unwrap();
    let session = make_session(dir.path());
    let client = FakeStreamingLlmClient::builder()
        .push_events(flood_token_events(400))
        .build();
    let agent_loop = make_agent_loop(client);
    let request = make_request("slow observer");

    let session_id = {
        let mut stream = stream_turn_live_with_agent(session, agent_loop, request, vec![])
            .await
            .expect("stream_turn_live_with_agent should succeed");
        let first = stream.next().await.expect("TurnStarted");
        match first {
            TurnEvent::TurnStarted { session_id, .. } => session_id,
            other => panic!("expected TurnStarted, got {other:?}"),
        }
        // Stream dropped without draining token deltas — public channel backpressure.
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
