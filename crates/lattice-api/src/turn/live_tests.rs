// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Behavioral tests for the live `stream_turn` path.
//!
//! These tests exercise the full live path: real SessionApi backed by SQLite,
//! real AgentLoop with FakeStreamingLlmClient, and the live stream_turn_live_with_agent
//! function. No internal boundaries are mocked.

use std::collections::VecDeque;
use std::sync::Arc;

use futures::StreamExt;
use lattice_agent::testing::FakeStreamingLlmClient;
use lattice_agent::{AgentLoop, AgentLoopConfig, LlmClient};
use lattice_llm::{LlmEvent, TokenUsage};
use lattice_session::WorkspaceDisposition;

use super::types::{RunTurnRequest, RunTurnStatus, TurnEvent, TurnEventStream};
use crate::session::SessionApi;

// ── Helpers ────────────────────────────────────────────────────────────

const TEST_SESSION_ID: &str = "test-session";

/// No-op tool executor that rejects all calls (tests don't invoke tools
/// unless explicitly configured).
struct NoOpToolExecutor;

#[async_trait::async_trait]
impl lattice_agent::ToolCallExecutor for NoOpToolExecutor {
    async fn execute(
        &self,
        _call: &lattice_llm::ToolCall,
    ) -> Result<String, lattice_agent::ToolExecutionError> {
        Err(lattice_agent::ToolExecutionError::simple(
            "no tools configured in test",
        ))
    }
}

fn make_session(dir: &std::path::Path) -> SessionApi {
    let api = SessionApi::git(dir);
    let _ = api
        .store()
        .create_session(TEST_SESSION_ID, None, "test", "main");
    api
}

fn make_agent_loop(client: FakeStreamingLlmClient) -> Arc<AgentLoop> {
    Arc::new(AgentLoop::new(AgentLoopConfig {
        llm: Arc::new(client),
        tool_executor: Arc::new(NoOpToolExecutor),
    }))
}

async fn stream_turn_live_with_agent(
    session: SessionApi,
    agent_loop: Arc<AgentLoop>,
    request: RunTurnRequest,
    tool_definitions: Vec<lattice_llm::ToolDefinition>,
) -> crate::session::Result<TurnEventStream> {
    let (cep_client, _server) = lancet_native::CepTransport::pair(1);
    let deps = super::cep::ProductionTurnDeps {
        agent_loop,
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

// ── BlockingLlmClient ─────────────────────────────────────────────────
//
// A streaming LLM fake that yields initial events, then blocks on a
// `tokio::sync::Notify` before emitting the remaining events. This gives
// the test thread deterministic control over when the agent loop progresses
// past model consumption.

struct BlockingLlmClient {
    pre_block_events: std::sync::Mutex<Option<Vec<LlmEvent>>>,
    resume: Arc<tokio::sync::Notify>,
    post_resume_events: std::sync::Mutex<Option<Vec<LlmEvent>>>,
}

impl BlockingLlmClient {
    fn new(
        pre_block_events: Vec<LlmEvent>,
        resume: Arc<tokio::sync::Notify>,
        post_resume_events: Vec<LlmEvent>,
    ) -> Self {
        Self {
            pre_block_events: std::sync::Mutex::new(Some(pre_block_events)),
            resume,
            post_resume_events: std::sync::Mutex::new(Some(post_resume_events)),
        }
    }
}

#[async_trait::async_trait]
impl LlmClient for BlockingLlmClient {
    async fn chat(
        &self,
        _request: lattice_llm::ChatRequest,
    ) -> Result<lattice_llm::ChatResponse, Box<dyn std::error::Error + Send + Sync>> {
        unreachable!("stream-only fake")
    }

    async fn stream_chat(
        &self,
        _request: lattice_llm::ChatRequest,
    ) -> Result<lattice_llm::LlmStream, Box<dyn std::error::Error + Send + Sync>> {
        let pre = self
            .pre_block_events
            .lock()
            .unwrap()
            .take()
            .expect("BlockingLlmClient only supports a single round");
        let post = self
            .post_resume_events
            .lock()
            .unwrap()
            .take()
            .expect("BlockingLlmClient only supports a single round");
        let resume = self.resume.clone();

        // State machine: Pre -> (await notify) -> Post
        enum StreamState {
            Pre(
                VecDeque<LlmEvent>,
                Arc<tokio::sync::Notify>,
                VecDeque<LlmEvent>,
            ),
            Post(VecDeque<LlmEvent>),
        }

        let initial = StreamState::Pre(VecDeque::from(pre), resume, VecDeque::from(post));

        let stream = futures::stream::unfold(initial, |state| async move {
            match state {
                StreamState::Pre(mut queue, notify, mut post) => {
                    if let Some(event) = queue.pop_front() {
                        Some((event, StreamState::Pre(queue, notify, post)))
                    } else {
                        // All pre-block events emitted; block until notified
                        notify.notified().await;
                        // Transition to Post state
                        post.pop_front()
                            .map(|event| (event, StreamState::Post(post)))
                    }
                }
                StreamState::Post(mut queue) => queue
                    .pop_front()
                    .map(|event| (event, StreamState::Post(queue))),
            }
        });

        Ok(Box::pin(stream))
    }
}

// ── Tests ──────────────────────────────────────────────────────────────



#[tokio::test]
async fn stream_turn_live_with_agent_error_commits_error() {
    let dir = tempfile::tempdir().unwrap();
    let session = make_session(dir.path());
    // Push an error event that will cause the agent loop to fail
    let client = FakeStreamingLlmClient::builder()
        .push_error("provider unavailable")
        .build();
    let agent_loop = make_agent_loop(client);
    let request = make_request("hello");

    let stream = stream_turn_live_with_agent(session, agent_loop, request, vec![])
        .await
        .expect("stream_turn_live_with_agent should succeed (error is in-stream)");
    let events: Vec<TurnEvent> = stream.collect().await;

    // Contains TurnFailed with non-empty reason
    let failed = events
        .iter()
        .find(|e| matches!(e, TurnEvent::TurnFailed { .. }))
        .expect("should have TurnFailed event");
    if let TurnEvent::TurnFailed { reason } = failed {
        assert!(!reason.is_empty(), "TurnFailed reason should not be empty");
    }

    // TurnFailed appears before TurnCommitting
    let failed_pos = events
        .iter()
        .position(|e| matches!(e, TurnEvent::TurnFailed { .. }))
        .unwrap();
    let committing_pos = events
        .iter()
        .position(|e| matches!(e, TurnEvent::TurnCommitting))
        .expect("should have TurnCommitting");
    assert!(
        failed_pos < committing_pos,
        "TurnFailed must appear before TurnCommitting"
    );

    // Contains TurnCommitted with status Error
    let committed = events
        .iter()
        .find(|e| matches!(e, TurnEvent::TurnCommitted { .. }))
        .expect("should have TurnCommitted event");
    if let TurnEvent::TurnCommitted { status, .. } = committed {
        assert_eq!(*status, RunTurnStatus::Error);
    }

    // No CommitFailed event
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, TurnEvent::CommitFailed { .. })),
        "should not contain CommitFailed"
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
async fn stream_turn_live_with_agent_commit_failure_deterministic() {
    // Deterministic commit failure using BlockingLlmClient.
    // and real SQLite parent conflict.
    let dir = tempfile::tempdir().unwrap();
    let session = make_session(dir.path());

    // Create a session manually so we can manipulate it
    let create_req = crate::session::CreateSessionRequest {
        label: None,
        active_agent: "test".to_string(),
        project_branch: "main".to_string(),
    };
    let created = session.create_session(create_req).expect("create session");
    let session_id = created.session_id.clone();

    // First turn: establish a commit (so we have a parent)
    let client1 = FakeStreamingLlmClient::builder()
        .push_events(happy_events())
        .build();
    let agent_loop1 = make_agent_loop(client1);
    let request1 = make_request_with_session("first turn", &session_id);
    let stream1 = stream_turn_live_with_agent(session, agent_loop1, request1, vec![])
        .await
        .expect("first turn should succeed");
    let events1: Vec<TurnEvent> = stream1.collect().await;
    assert!(
        events1
            .iter()
            .any(|e| matches!(e, TurnEvent::TurnCommitted { .. })),
        "first turn should commit"
    );

    // Build BlockingLlmClient: yields tokens, then blocks until resumed
    let resume = Arc::new(tokio::sync::Notify::new());
    let blocking_client = BlockingLlmClient::new(
        vec![
            LlmEvent::Token {
                content: "partial".to_string(),
            },
            LlmEvent::Token {
                content: " response".to_string(),
            },
        ],
        resume.clone(),
        vec![LlmEvent::Done {
            finish_reason: "stop".to_string(),
            usage: Some(TokenUsage {
                prompt_tokens: 10,
                completion_tokens: 5,
                total_tokens: 15,
            }),
        }],
    );

    // Start second turn with BlockingLlmClient
    let session2 = make_session(dir.path());
    let agent_loop2 = Arc::new(AgentLoop::new(AgentLoopConfig {
        llm: Arc::new(blocking_client),
        tool_executor: Arc::new(NoOpToolExecutor),
    }));
    let request2 = make_request_with_session("second turn", &session_id);

    // stream_turn_live_with_agent returning Ok guarantees prepare_live_turn completed
    // with the stale parent.
    let stream2 = stream_turn_live_with_agent(session2, agent_loop2, request2, vec![])
        .await
        .expect("second turn prepare should succeed");

    // Insert racing commit while BlockingLlmClient is paused.
    let session3 = make_session(dir.path());
    let log = session3.log(&session_id).expect("log");
    let current_head = log.last().unwrap().commit_id.clone();

    let racing_input = lattice_session::TurnCommitInput {
        session_id: session_id.clone(),
        parent_ids: vec![current_head],
        messages: vec![lattice_session::GraphMessage {
            role: "user".to_string(),
            content: "racing turn".to_string(),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: None,
            name: None,
        }],
        agent_name: "coder".to_string(),
        profile_snapshot_hash: "test-profile".to_string(),
        profile_snapshot_json: "{}".to_string(),
        model: "sonnet".to_string(),
        provider: "anthropic".to_string(),
        status: lattice_session::TurnCommitStatus::Completed,
        disposition: WorkspaceDisposition::NotApplicable,
        lifecycle: None,
        commit_message: "Racing commit".to_string(),
        usage: None,
        terminal_reason: None,
    };
    session3
        .store()
        .commit_turn(racing_input)
        .expect("racing commit should succeed");

    // Resume BlockingLlmClient — agent finishes, commit_turn_inner finds
    // parent mismatch.
    resume.notify_one();

    // Collect second turn's events
    let events2: Vec<TurnEvent> = stream2.collect().await;

    // Assert: TokenDelta events present before CommitFailed (proves live
    // activity before commit)
    let token_pos = events2
        .iter()
        .position(|e| matches!(e, TurnEvent::TokenDelta { .. }));
    assert!(
        token_pos.is_some(),
        "should have TokenDelta events (live activity before commit)"
    );

    // Assert: TurnCommitting present
    assert!(
        events2
            .iter()
            .any(|e| matches!(e, TurnEvent::TurnCommitting)),
        "should have TurnCommitting event"
    );

    // Assert: CommitFailed present with non-empty reason
    let commit_failed = events2
        .iter()
        .find(|e| matches!(e, TurnEvent::CommitFailed { .. }));
    assert!(
        commit_failed.is_some(),
        "should have CommitFailed event when parent is stale. Events: {events2:?}"
    );
    if let Some(TurnEvent::CommitFailed { reason }) = commit_failed {
        assert!(
            !reason.is_empty(),
            "CommitFailed reason should not be empty"
        );
    }

    // Assert: TurnCommitted absent
    assert!(
        !events2
            .iter()
            .any(|e| matches!(e, TurnEvent::TurnCommitted { .. })),
        "should not contain TurnCommitted when commit fails"
    );

    // Assert: HEAD = 2 commits (first turn + racing), not 3
    let session4 = make_session(dir.path());
    let log_after = session4.log(&session_id).expect("log after failed commit");
    assert_eq!(
        log_after.len(),
        2,
        "HEAD should have exactly 2 commits (first + racing), got {}",
        log_after.len()
    );
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
            let session = make_session(dir.path());
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
            let session = make_session(dir.path());
            if session.log(&session_id).map(|log| log.len()).unwrap_or(0) == 1 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("turn must commit despite slow observer");
}
