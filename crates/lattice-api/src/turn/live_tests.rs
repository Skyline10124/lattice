//! Behavioral tests for the live `stream_turn` path (Phase 16 Plan 03).
//!
//! These tests exercise the full live path: real SessionApi backed by SQLite,
//! real AgentLoop with FakeStreamingLlmClient, and the live stream_turn_live
//! function. No internal boundaries are mocked.

use std::sync::Arc;

use futures::StreamExt;
use lattice_agent::testing::FakeStreamingLlmClient;
use lattice_agent::{AgentLoop, AgentLoopConfig};
use lattice_llm::{LlmEvent, TokenUsage};
use lattice_session::SessionGraphStore;

use super::live::stream_turn_live;
use super::types::{RunTurnRequest, RunTurnStatus, TurnEvent};
use crate::session::SessionApi;

// ── Helpers ────────────────────────────────────────────────────────────

/// No-op tool executor that rejects all calls (tests don't invoke tools
/// unless explicitly configured).
struct NoOpToolExecutor;

#[async_trait::async_trait]
impl lattice_tools::ToolExecutor for NoOpToolExecutor {
    async fn execute(&self, _call: &lattice_llm::ToolCall) -> Result<String, String> {
        Err("no tools configured in test".to_string())
    }
}

fn make_session(dir: &std::path::Path) -> SessionApi {
    SessionApi::sqlite(dir)
}

fn make_agent_loop(client: FakeStreamingLlmClient) -> Arc<AgentLoop> {
    Arc::new(AgentLoop::new(AgentLoopConfig {
        llm: Arc::new(client),
        tool_executor: Arc::new(NoOpToolExecutor),
    }))
}

fn make_request(input: &str) -> RunTurnRequest {
    RunTurnRequest {
        input: input.to_string(),
        session_id: None,
        profile: None,
        model: None,
        loaded_profile: None,
    }
}

fn make_request_with_session(input: &str, session_id: &str) -> RunTurnRequest {
    RunTurnRequest {
        input: input.to_string(),
        session_id: Some(session_id.to_string()),
        profile: None,
        model: None,
        loaded_profile: None,
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

/// Multi-event sequence for ordering tests.
fn ordered_events() -> Vec<LlmEvent> {
    vec![
        LlmEvent::Token {
            content: "response text".to_string(),
        },
        LlmEvent::Done {
            finish_reason: "stop".to_string(),
            usage: Some(TokenUsage {
                prompt_tokens: 8,
                completion_tokens: 3,
                total_tokens: 11,
            }),
        },
    ]
}

// ── Tests ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn stream_turn_live_basic_completed() {
    let dir = tempfile::tempdir().unwrap();
    let session = make_session(dir.path());
    let client = FakeStreamingLlmClient::builder()
        .push_events(happy_events())
        .build();
    let agent_loop = make_agent_loop(client);
    let request = make_request("hello");

    let stream = stream_turn_live(session, agent_loop, request)
        .await
        .expect("stream_turn_live should succeed");
    let events: Vec<TurnEvent> = stream.collect().await;

    // First event is TurnStarted with non-empty session_id
    let first = &events[0];
    if let TurnEvent::TurnStarted {
        session_id,
        parent_commit_id: _,
        branch: _,
    } = first
    {
        assert!(!session_id.is_empty(), "session_id should not be empty");
    } else {
        panic!("first event should be TurnStarted, got: {first:?}");
    }

    // Contains TokenDelta events
    assert!(
        events
            .iter()
            .any(|e| matches!(e, TurnEvent::TokenDelta { .. })),
        "should contain TokenDelta events"
    );

    // Contains ModelFinished
    assert!(
        events
            .iter()
            .any(|e| matches!(e, TurnEvent::ModelFinished { .. })),
        "should contain ModelFinished event"
    );

    // No TurnFailed or TurnCancelled
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, TurnEvent::TurnFailed { .. })),
        "should not contain TurnFailed"
    );
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, TurnEvent::TurnCancelled { .. })),
        "should not contain TurnCancelled"
    );

    // Contains TurnCommitting followed by TurnCommitted with status Completed
    let committing_pos = events
        .iter()
        .position(|e| matches!(e, TurnEvent::TurnCommitting))
        .expect("should have TurnCommitting");
    let committed_pos = events
        .iter()
        .position(|e| matches!(e, TurnEvent::TurnCommitted { .. }))
        .expect("should have TurnCommitted");
    assert!(
        committing_pos < committed_pos,
        "TurnCommitting must precede TurnCommitted"
    );

    if let TurnEvent::TurnCommitted {
        session_id: committed_sid,
        commit_id,
        status,
    } = &events[committed_pos]
    {
        assert_eq!(*status, RunTurnStatus::Completed);
        assert!(!commit_id.is_empty(), "commit_id should not be empty");

        // TurnCommitted.session_id matches TurnStarted.session_id
        if let TurnEvent::TurnStarted { session_id, .. } = first {
            assert_eq!(committed_sid, session_id);
        }
    } else {
        panic!("expected TurnCommitted at committed_pos");
    }
}

#[tokio::test]
async fn stream_turn_live_error_commits_error() {
    let dir = tempfile::tempdir().unwrap();
    let session = make_session(dir.path());
    // Push an error event that will cause the agent loop to fail
    let client = FakeStreamingLlmClient::builder()
        .push_error("provider unavailable")
        .build();
    let agent_loop = make_agent_loop(client);
    let request = make_request("hello");

    let stream = stream_turn_live(session, agent_loop, request)
        .await
        .expect("stream_turn_live should succeed (error is in-stream)");
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
async fn stream_turn_live_event_ordering() {
    let dir = tempfile::tempdir().unwrap();
    let session = make_session(dir.path());
    let client = FakeStreamingLlmClient::builder()
        .push_events(ordered_events())
        .build();
    let agent_loop = make_agent_loop(client);
    let request = make_request("test ordering");

    let stream = stream_turn_live(session, agent_loop, request)
        .await
        .expect("stream_turn_live should succeed");
    let events: Vec<TurnEvent> = stream.collect().await;

    // TurnStarted is first
    assert!(
        matches!(&events[0], TurnEvent::TurnStarted { .. }),
        "first event should be TurnStarted"
    );

    // ModelStarted appears after TurnStarted
    let started_pos = 0;
    let model_started_pos = events
        .iter()
        .position(|e| matches!(e, TurnEvent::ModelStarted { .. }))
        .expect("should have ModelStarted");
    assert!(model_started_pos > started_pos);

    // TokenDelta appears after ModelStarted
    let token_pos = events
        .iter()
        .position(|e| matches!(e, TurnEvent::TokenDelta { .. }))
        .expect("should have TokenDelta");
    assert!(
        token_pos > model_started_pos,
        "TokenDelta should appear after ModelStarted"
    );

    // ModelFinished appears after TokenDelta
    let model_finished_pos = events
        .iter()
        .position(|e| matches!(e, TurnEvent::ModelFinished { .. }))
        .expect("should have ModelFinished");
    assert!(
        model_finished_pos > token_pos,
        "ModelFinished should appear after TokenDelta"
    );

    // TurnCommitting appears after ModelFinished
    let committing_pos = events
        .iter()
        .position(|e| matches!(e, TurnEvent::TurnCommitting))
        .expect("should have TurnCommitting");
    assert!(
        committing_pos > model_finished_pos,
        "TurnCommitting should appear after ModelFinished"
    );

    // TurnCommitted is last
    let committed_pos = events
        .iter()
        .position(|e| matches!(e, TurnEvent::TurnCommitted { .. }))
        .expect("should have TurnCommitted");
    assert_eq!(
        committed_pos,
        events.len() - 1,
        "TurnCommitted should be the last event"
    );
}

#[tokio::test]
async fn stream_turn_live_startup_failure_no_stream() {
    let dir = tempfile::tempdir().unwrap();
    let session = make_session(dir.path());
    let client = FakeStreamingLlmClient::builder()
        .push_events(happy_events())
        .build();
    let agent_loop = make_agent_loop(client);

    // Pass a request with session_id pointing to a non-existent session
    let request = make_request_with_session("hello", "non-existent-session-id");

    let result = stream_turn_live(session, agent_loop, request).await;

    // Should return Err, not Ok with error events inside the stream (API-04/API-05)
    assert!(
        result.is_err(),
        "startup failure should return Err, not Ok with error events in stream"
    );
}

#[tokio::test]
async fn stream_turn_live_commit_stores_delta_only() {
    let dir = tempfile::tempdir().unwrap();
    let session = make_session(dir.path());
    let client = FakeStreamingLlmClient::builder()
        .push_events(happy_events())
        .build();
    let agent_loop = make_agent_loop(client);
    let request = make_request("What is 2+2?");

    let stream = stream_turn_live(session, agent_loop, request)
        .await
        .expect("stream_turn_live should succeed");
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

    // No prior transcript messages are duplicated (COMM-01)
    // Since this is the first turn, there should be exactly user + agent messages
    let user_count = committed_messages
        .iter()
        .filter(|m| m.role == "user")
        .count();
    assert_eq!(
        user_count, 1,
        "commit should contain exactly one user message (delta only)"
    );

    // Commit message is <= 72 chars (COMM-04)
    assert!(
        log[0].commit_message.len() <= 72,
        "commit message should be <= 72 chars, got: {} ('{}')",
        log[0].commit_message.len(),
        log[0].commit_message
    );

    // ── Second turn: prove delta-only (COMM-01 multi-turn) ───────────────
    // A second turn on the same session must commit only its own delta,
    // not replay the first turn's messages into the commit.
    let session3 = make_session(dir.path());
    let client2 = FakeStreamingLlmClient::builder()
        .push_events(happy_events())
        .build();
    let agent_loop2 = make_agent_loop(client2);
    let request2 = make_request_with_session("follow-up question", &session_id);

    let stream2 = stream_turn_live(session3, agent_loop2, request2)
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
async fn stream_turn_live_branch_populated() {
    let dir = tempfile::tempdir().unwrap();
    let session = make_session(dir.path());
    let client = FakeStreamingLlmClient::builder()
        .push_events(happy_events())
        .build();
    let agent_loop = make_agent_loop(client);
    let request = make_request("hello");

    let stream = stream_turn_live(session, agent_loop, request)
        .await
        .expect("stream_turn_live should succeed");
    let events: Vec<TurnEvent> = stream.collect().await;

    // TurnStarted.branch should be Some (default branch name) when session is on a branch
    if let TurnEvent::TurnStarted { branch, .. } = &events[0] {
        assert!(
            branch.is_some(),
            "TurnStarted.branch should be Some when session is on a branch"
        );
        assert_eq!(
            branch.as_deref(),
            Some("main"),
            "default branch should be 'main'"
        );
    } else {
        panic!("first event should be TurnStarted");
    }
}

#[tokio::test]
async fn stream_turn_live_commit_failure_emits_commit_failed() {
    let dir = tempfile::tempdir().unwrap();
    let session = make_session(dir.path());

    // Create a session manually so we can manipulate it
    let create_req = crate::session::CreateSessionRequest {
        label: None,
        profile: None,
        model: None,
    };
    let created = session.create_session(create_req).expect("create session");
    let session_id = created.session_id.clone();

    // Do a first turn to establish a commit (so we have a parent)
    let client1 = FakeStreamingLlmClient::builder()
        .push_events(happy_events())
        .build();
    let agent_loop1 = make_agent_loop(client1);
    let request1 = make_request_with_session("first turn", &session_id);
    let stream1 = stream_turn_live(session, agent_loop1, request1)
        .await
        .expect("first turn should succeed");
    let events1: Vec<TurnEvent> = stream1.collect().await;
    assert!(
        events1
            .iter()
            .any(|e| matches!(e, TurnEvent::TurnCommitted { .. })),
        "first turn should commit"
    );

    // Now open a fresh session for the second turn
    let session2 = make_session(dir.path());
    let client2 = FakeStreamingLlmClient::builder()
        .push_events(happy_events())
        .build();
    let agent_loop2 = make_agent_loop(client2);
    let request2 = make_request_with_session("second turn", &session_id);

    // Start the second live turn — prepare_live_turn reads the current parent
    let stream2 = stream_turn_live(session2, agent_loop2, request2)
        .await
        .expect("second turn prepare should succeed");

    // Now race a concurrent commit on the same session to make the parent stale.
    // This simulates a conflict: the background task's commit_turn will find
    // that HEAD has moved since prepare_live_turn read it.
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
        profile_snapshot_id: None,
        model: "sonnet".to_string(),
        provider: "anthropic".to_string(),
        status: lattice_session::TurnCommitStatus::Completed,
        commit_message: "Racing commit".to_string(),
        usage: None,
        terminal_reason: None,
    };
    session3
        .store()
        .commit_turn(racing_input)
        .expect("racing commit should succeed");

    // Now collect the second turn's events — its commit should fail due to
    // stale parent (HEAD moved from under it)
    let events2: Vec<TurnEvent> = stream2.collect().await;

    // Stream contains TurnCommitting
    assert!(
        events2
            .iter()
            .any(|e| matches!(e, TurnEvent::TurnCommitting)),
        "should have TurnCommitting event"
    );

    // Stream contains CommitFailed with non-empty reason
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

    // Stream does NOT contain TurnCommitted
    assert!(
        !events2
            .iter()
            .any(|e| matches!(e, TurnEvent::TurnCommitted { .. })),
        "should not contain TurnCommitted when commit fails"
    );

    // HEAD unchanged — log should still have the same number of commits as
    // before the failed second turn (1 from first turn + 1 racing commit = 2)
    let session4 = make_session(dir.path());
    let log_after = session4.log(&session_id).expect("log after failed commit");
    assert_eq!(
        log_after.len(),
        2,
        "HEAD should not advance after commit failure — expected 2 commits, got {}",
        log_after.len()
    );
}

#[tokio::test]
async fn stream_turn_live_receiver_drop_still_commits() {
    let dir = tempfile::tempdir().unwrap();
    let session = make_session(dir.path());
    let client = FakeStreamingLlmClient::builder()
        .push_events(happy_events())
        .build();
    let agent_loop = make_agent_loop(client);
    let request = make_request("receiver drop test");

    let stream = stream_turn_live(session, agent_loop, request)
        .await
        .expect("stream_turn_live should succeed");

    // Extract session_id from the first event before dropping
    let mut stream = std::pin::pin!(stream);
    let first_event = stream.next().await.expect("should get TurnStarted");
    let session_id = if let TurnEvent::TurnStarted { session_id, .. } = first_event {
        session_id
    } else {
        panic!("first event should be TurnStarted");
    };

    // Drop the stream immediately — receiver gone, background task should still commit (API-10)
    drop(stream);

    // Give the background task time to finish and commit
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    // Verify commit appeared despite receiver drop
    let session2 = make_session(dir.path());
    let log = session2.log(&session_id).expect("should read log");
    assert_eq!(
        log.len(),
        1,
        "background task should still commit even after receiver drop (API-10)"
    );
}
