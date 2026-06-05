// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

#![cfg(feature = "test-utils")]
//! live streaming behavioral suite: AgentLoop live sink + cooperative cancellation.
//!
//! Locks the live-sink and cooperative cancellation scenarios exposed by
//! `run_turn_live`. All scenarios with a live sender drive via
//! `drive_run_turn_live`, which `tokio::join!`s the run + drain so the bounded
//! mpsc channel never deadlocks.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::sync::Mutex;

use lattice_agent::testing::FakeStreamingLlmClient;
use lattice_agent::{
    AgentLoop, AgentLoopConfig, AgentTurnEvent, AgentTurnRequest, AgentTurnStatus, LlmClient,
    LlmRetryConfig, ToolCallExecutor, ToolExecutionError,
};
use lattice_llm::{LlmEvent, ToolCall, ToolDefinition};
use lattice_session::HeadTranscript;

// ---------------------------------------------------------------------------
// Test helpers (duplicated from streaming_delta.rs — Rust integration tests
// don't share helpers across files).
// ---------------------------------------------------------------------------

fn read_file_tool_definition() -> ToolDefinition {
    ToolDefinition::new(
        "observe.workspace",
        "Observe workspace content",
        serde_json::json!({"type": "object", "properties": {"path": {"type": "string"}}}),
    )
}

/// Build a minimal `AgentTurnRequest` with no cancel flag and no timeout.
fn make_request(user_input: &str) -> AgentTurnRequest {
    AgentTurnRequest {
        session_id: "test-session".to_string(),
        transcript: HeadTranscript {
            session_id: "test-session".to_string(),
            parent_commit_id: None,
            messages: vec![],
        },
        user_input: user_input.to_string(),
        system_prompt: None,
        model: "test-model".to_string(),
        provider: None,
        tool_definitions: vec![read_file_tool_definition()],
        cancel_flag: None,
        timeout: None,
        llm_retry: LlmRetryConfig::default(),
    }
}

/// Build a minimal `AgentTurnRequest` carrying a shared cancel flag.
fn make_request_with_cancel(user_input: &str, flag: Arc<AtomicBool>) -> AgentTurnRequest {
    AgentTurnRequest {
        session_id: "test-session".to_string(),
        transcript: HeadTranscript {
            session_id: "test-session".to_string(),
            parent_commit_id: None,
            messages: vec![],
        },
        user_input: user_input.to_string(),
        system_prompt: None,
        model: "test-model".to_string(),
        provider: None,
        tool_definitions: vec![read_file_tool_definition()],
        cancel_flag: Some(flag),
        timeout: None,
        llm_retry: LlmRetryConfig::default(),
    }
}

/// Drive `run_turn_live` to completion while concurrently draining the live
/// receiver into a `Vec<AgentTurnEvent>`. `tokio::join!` is intentional so the
/// `&AgentLoop` borrow stays usable; switching to `tokio::spawn` would force
/// callers to wrap `AgentLoop` in `Arc`.
///
/// The bounded channel is sized at 32. The drain loop terminates when
/// `run_turn_live` returns and drops its `Sender`, closing `rx`.
async fn drive_run_turn_live(
    loop_inst: &AgentLoop,
    request: AgentTurnRequest,
    capacity: usize,
) -> (lattice_agent::AgentTurnResult, Vec<AgentTurnEvent>) {
    let (tx, mut rx) = tokio::sync::mpsc::channel::<AgentTurnEvent>(capacity);

    let drain = async move {
        let mut received = Vec::new();
        while let Some(event) = rx.recv().await {
            received.push(event);
        }
        received
    };

    let run = async { loop_inst.run_turn_live(request, tx).await };

    tokio::join!(run, drain)
}

// ---------------------------------------------------------------------------
// FakeToolExecutor — VecDeque<Result<String, String>> + execution-order log.
// Mirrors the shape from streaming_delta.rs (streaming test infrastructure).
// ---------------------------------------------------------------------------

struct FakeToolExecutor {
    results: Mutex<VecDeque<Result<String, ToolExecutionError>>>,
    execution_order: Arc<Mutex<Vec<String>>>,
}

impl FakeToolExecutor {
    fn new(results: Vec<Result<String, ToolExecutionError>>) -> Self {
        Self {
            results: Mutex::new(VecDeque::from(results)),
            execution_order: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

#[async_trait::async_trait]
impl ToolCallExecutor for FakeToolExecutor {
    async fn execute(&self, call: &ToolCall) -> Result<String, ToolExecutionError> {
        self.execution_order.lock().unwrap().push(call.id.clone());
        let mut results = self.results.lock().unwrap();
        match results.pop_front() {
            Some(result) => result,
            None => Err(ToolExecutionError::simple("no more fake results")),
        }
    }
}

/// `ToolCallExecutor` wrapper that flips a shared cancel flag the first time
/// `execute` is called, *before* forwarding to the inner executor and returning
/// the inner's real result.
///
/// Drives scenarios #2 (cancel mid-tool, real result preserved) and
/// #6 (cancel between two tools in a multi-tool batch — checkpoint #5).
struct CancelOnFirstTool {
    inner: FakeToolExecutor,
    cancel: Arc<AtomicBool>,
    fired: AtomicBool,
}

impl CancelOnFirstTool {
    fn new(inner: FakeToolExecutor, cancel: Arc<AtomicBool>) -> Self {
        Self {
            inner,
            cancel,
            fired: AtomicBool::new(false),
        }
    }
}

#[async_trait::async_trait]
impl ToolCallExecutor for CancelOnFirstTool {
    async fn execute(&self, call: &ToolCall) -> Result<String, ToolExecutionError> {
        if !self.fired.swap(true, Ordering::Relaxed) {
            self.cancel.store(true, Ordering::Relaxed);
        }
        self.inner.execute(call).await
    }
}

// ---------------------------------------------------------------------------
// CancelOnDoneLlm — flips cancel flag between the last non-Done event and Done.
// ---------------------------------------------------------------------------

/// LlmClient whose `stream_chat` yields the queued assembly events first, then
/// flips a shared cancel flag, then yields a final `Done`.
///
/// Used by scenario #1 to exercise checkpoint #3 (post-`Done` finalize, pre-tool
/// loop) without tripping checkpoint #2: the cancel flag is set *between* the
/// last non-`Done` event's checkpoint #2 fire and the `Done` arm; `Done` is
/// never a cancel point inside `consume_model_stream`, so the function
/// returns `StreamOutcome::Done` and `run_turn_inner` finalizes the assistant
/// message + emits `ModelFinished` before checkpoint #3 observes the cancel.
struct CancelOnDoneLlm {
    cancel: Arc<AtomicBool>,
    pre_done_events: Mutex<Option<Vec<LlmEvent>>>,
    finish_reason: String,
}

impl CancelOnDoneLlm {
    fn new(cancel: Arc<AtomicBool>, pre_done_events: Vec<LlmEvent>, finish_reason: &str) -> Self {
        Self {
            cancel,
            pre_done_events: Mutex::new(Some(pre_done_events)),
            finish_reason: finish_reason.to_string(),
        }
    }
}

#[async_trait::async_trait]
impl LlmClient for CancelOnDoneLlm {
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
            .pre_done_events
            .lock()
            .unwrap()
            .take()
            .expect("CancelOnDoneLlm only supports a single round");
        let cancel = self.cancel.clone();
        let finish_reason = self.finish_reason.clone();

        // State machine for futures::stream::unfold: emit each pre-Done event,
        // then on the trailing transition flip the cancel flag and emit Done.
        enum StreamState {
            Pending(VecDeque<LlmEvent>, String, Arc<AtomicBool>),
            DoneEmitted,
        }

        let initial = StreamState::Pending(VecDeque::from(pre), finish_reason, cancel);

        let stream = futures::stream::unfold(initial, |state| async move {
            match state {
                StreamState::Pending(mut queue, finish_reason, cancel) => {
                    if let Some(event) = queue.pop_front() {
                        Some((event, StreamState::Pending(queue, finish_reason, cancel)))
                    } else {
                        // Last non-Done event has been consumed (and checkpoint
                        // #2 fired with cancel still false). Flip cancel, then
                        // yield Done — the cancel is observed at checkpoint #3,
                        // not #2 (Done is never a cancel point).
                        cancel.store(true, Ordering::Relaxed);
                        let done = LlmEvent::Done {
                            finish_reason,
                            usage: None,
                        };
                        Some((done, StreamState::DoneEmitted))
                    }
                }
                StreamState::DoneEmitted => None,
            }
        });

        Ok(Box::pin(stream))
    }
}

// ---------------------------------------------------------------------------
// Scenario 1 — cancel after Done finalize, before tool execution (checkpoint #3)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn scenario_01_cancel_before_tool_execution() {
    let cancel = Arc::new(AtomicBool::new(false));

    // Model emits a single valid tool call and Done(tool_calls); cancel flag is
    // flipped between the last assembly event and Done by `CancelOnDoneLlm`.
    let pre_done = vec![
        LlmEvent::ToolCallStart {
            id: "c1".to_string(),
            name: "fake_tool".to_string(),
        },
        LlmEvent::ToolCallDelta {
            id: "c1".to_string(),
            arguments_delta: "{}".to_string(),
        },
        LlmEvent::ToolCallEnd {
            id: "c1".to_string(),
        },
    ];
    let llm = Arc::new(CancelOnDoneLlm::new(cancel.clone(), pre_done, "tool_calls"));

    // Tool executor queued with a result that, if executed, would surface as
    // the assistant_output of a follow-up round — so a missed cancel race
    // would visibly fail the assertion below.
    let executor = Arc::new(FakeToolExecutor::new(vec![
        Ok("never executed".to_string()),
    ]));

    let loop_inst = AgentLoop::new(AgentLoopConfig {
        llm,
        tool_executor: executor,
    });

    let request = make_request_with_cancel("call tool", cancel.clone());
    let (result, received) = drive_run_turn_live(&loop_inst, request, 32).await;

    // Status / terminal_reason: checkpoint #3 cancel branch.
    assert_eq!(result.status, AgentTurnStatus::Cancelled);
    assert_eq!(
        result.terminal_reason,
        Some("cancelled by caller".to_string())
    );

    // Tool execution never started.
    let started = result
        .events
        .iter()
        .filter(|e| matches!(e, AgentTurnEvent::ToolCallStarted { .. }))
        .count();
    assert_eq!(
        started, 0,
        "no ToolCallStarted should be emitted when cancel observed at checkpoint #3"
    );

    // The assistant tool-call GraphMessage is in transcript; tool result
    // GraphMessage is NOT.
    let assistant_with_tool_calls = result
        .messages
        .iter()
        .filter(|m| m.role == "assistant" && m.tool_calls.is_some())
        .count();
    assert_eq!(
        assistant_with_tool_calls, 1,
        "assistant tool-call message must be committed before checkpoint #3 returns"
    );
    let tool_messages = result.messages.iter().filter(|m| m.role == "tool").count();
    assert_eq!(
        tool_messages, 0,
        "no tool result message should be committed when cancelled before tool execution"
    );

    // ModelFinished was emitted before cancellation.
    let model_finished = result
        .events
        .iter()
        .filter(|e| matches!(e, AgentTurnEvent::ModelFinished { .. }))
        .count();
    assert_eq!(
        model_finished, 1,
        "exactly one ModelFinished event should be emitted before cancel branch returns"
    );

    // No tool result summaries.
    assert!(result.tool_results.is_empty());

    // Live mirror saw zero ToolCallStarted events.
    let live_started = received
        .iter()
        .filter(|e| matches!(e, AgentTurnEvent::ToolCallStarted { .. }))
        .count();
    assert_eq!(live_started, 0);
}

// ---------------------------------------------------------------------------
// Scenario 6 — cancel after first tool of multi-tool batch (checkpoint #5)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn scenario_06_cancel_after_first_tool_in_multi_tool_batch() {
    let cancel = Arc::new(AtomicBool::new(false));

    // Two valid tool calls in one Done(tool_calls). The first tool's executor
    // flips the cancel flag during execute, then returns a real result; the
    // checkpoint #5 cancel observation skips the second tool entirely.
    let llm = Arc::new(
        FakeStreamingLlmClient::builder()
            .push_events(vec![
                LlmEvent::ToolCallStart {
                    id: "c1".to_string(),
                    name: "fake_tool".to_string(),
                },
                LlmEvent::ToolCallDelta {
                    id: "c1".to_string(),
                    arguments_delta: "{}".to_string(),
                },
                LlmEvent::ToolCallEnd {
                    id: "c1".to_string(),
                },
                LlmEvent::ToolCallStart {
                    id: "c2".to_string(),
                    name: "fake_tool".to_string(),
                },
                LlmEvent::ToolCallDelta {
                    id: "c2".to_string(),
                    arguments_delta: "{}".to_string(),
                },
                LlmEvent::ToolCallEnd {
                    id: "c2".to_string(),
                },
                LlmEvent::Done {
                    finish_reason: "tool_calls".to_string(),
                    usage: None,
                },
            ])
            .build(),
    );

    let inner = FakeToolExecutor::new(vec![
        Ok("first-tool-output".to_string()),
        Ok("second-tool-output".to_string()),
    ]);
    let executor = Arc::new(CancelOnFirstTool::new(inner, cancel.clone()));

    let loop_inst = AgentLoop::new(AgentLoopConfig {
        llm,
        tool_executor: executor,
    });

    let request = make_request_with_cancel("two tools", cancel.clone());
    let (result, received) = drive_run_turn_live(&loop_inst, request, 32).await;

    assert_eq!(result.status, AgentTurnStatus::Cancelled);
    assert_eq!(
        result.terminal_reason,
        Some("cancelled by caller".to_string())
    );

    // Exactly one ToolCallStarted + one ToolCallFinished — both for c1.
    let started: Vec<&AgentTurnEvent> = result
        .events
        .iter()
        .filter(|e| matches!(e, AgentTurnEvent::ToolCallStarted { .. }))
        .collect();
    assert_eq!(
        started.len(),
        1,
        "exactly one ToolCallStarted (the first tool)"
    );
    assert!(matches!(
        started[0],
        AgentTurnEvent::ToolCallStarted { id, .. } if id == "c1"
    ));

    let finished: Vec<&AgentTurnEvent> = result
        .events
        .iter()
        .filter(|e| matches!(e, AgentTurnEvent::ToolCallFinished { .. }))
        .collect();
    assert_eq!(
        finished.len(),
        1,
        "exactly one ToolCallFinished (the first tool)"
    );
    assert!(matches!(
        finished[0],
        AgentTurnEvent::ToolCallFinished { id, ok: true, .. } if id == "c1"
    ));

    // Second tool never started.
    assert!(!result.events.iter().any(|e| matches!(
        e,
        AgentTurnEvent::ToolCallStarted { id, .. } if id == "c2"
    )));

    // First tool's GraphMessage IS in transcript; second tool's is NOT.
    let tool_msgs: Vec<&lattice_session::GraphMessage> = result
        .messages
        .iter()
        .filter(|m| m.role == "tool")
        .collect();
    assert_eq!(tool_msgs.len(), 1);
    assert_eq!(tool_msgs[0].tool_call_id, Some("c1".to_string()));
    assert_eq!(tool_msgs[0].content, "first-tool-output");

    // Live mirror saw the same two tool-related events.
    assert_eq!(
        received
            .iter()
            .filter(|e| matches!(e, AgentTurnEvent::ToolCallStarted { .. }))
            .count(),
        1
    );
    assert_eq!(
        received
            .iter()
            .filter(|e| matches!(e, AgentTurnEvent::ToolCallFinished { .. }))
            .count(),
        1
    );
}

// ---------------------------------------------------------------------------
// Scenario 2 — cancel during tool execution preserves real tool result
// ---------------------------------------------------------------------------

#[tokio::test]
async fn scenario_02_cancel_during_tool_execution() {
    let cancel = Arc::new(AtomicBool::new(false));

    // Single valid tool call. The CancelOnFirstTool wrapper flips cancel
    // synchronously *before* forwarding to the inner executor — the inner then
    // returns its real result. The agent records the real result, no
    // synthesized "cancelled" tool finish, then checkpoint #5 returns Cancelled
    // before the next round opens.
    let llm = Arc::new(
        FakeStreamingLlmClient::builder()
            .push_events(vec![
                LlmEvent::ToolCallStart {
                    id: "c1".to_string(),
                    name: "fake_tool".to_string(),
                },
                LlmEvent::ToolCallDelta {
                    id: "c1".to_string(),
                    arguments_delta: "{}".to_string(),
                },
                LlmEvent::ToolCallEnd {
                    id: "c1".to_string(),
                },
                LlmEvent::Done {
                    finish_reason: "tool_calls".to_string(),
                    usage: None,
                },
            ])
            .build(),
    );

    let inner = FakeToolExecutor::new(vec![Ok("real-tool-output".to_string())]);
    let executor = Arc::new(CancelOnFirstTool::new(inner, cancel.clone()));

    let loop_inst = AgentLoop::new(AgentLoopConfig {
        llm,
        tool_executor: executor,
    });

    let request = make_request_with_cancel("call tool", cancel.clone());
    let (result, received) = drive_run_turn_live(&loop_inst, request, 32).await;

    assert_eq!(result.status, AgentTurnStatus::Cancelled);
    assert_eq!(
        result.terminal_reason,
        Some("cancelled by caller".to_string())
    );

    // Both ToolCallStarted and ToolCallFinished are recorded — the in-flight
    // tool is never aborted.
    let started_count = result
        .events
        .iter()
        .filter(|e| matches!(e, AgentTurnEvent::ToolCallStarted { .. }))
        .count();
    assert_eq!(started_count, 1);

    let finished: Vec<&AgentTurnEvent> = result
        .events
        .iter()
        .filter(|e| matches!(e, AgentTurnEvent::ToolCallFinished { .. }))
        .collect();
    assert_eq!(finished.len(), 1);
    // The recorded ToolCallFinished MUST be ok=true (the real tool result),
    // not a synthesized cancel marker.
    assert!(matches!(
        finished[0],
        AgentTurnEvent::ToolCallFinished { ok: true, .. }
    ));

    // Exactly one ModelStarted — the cancel branch returned before opening a
    // second LLM round.
    let model_started_count = result
        .events
        .iter()
        .filter(|e| matches!(e, AgentTurnEvent::ModelStarted { .. }))
        .count();
    assert_eq!(model_started_count, 1);

    // Real tool message in transcript.
    let tool_msg = result
        .messages
        .iter()
        .find(|m| m.role == "tool")
        .expect("tool result message should be committed before cancel branch returns");
    assert_eq!(tool_msg.tool_call_id, Some("c1".to_string()));
    assert_eq!(tool_msg.content, "real-tool-output");

    // Live mirror reflects the same shape.
    assert_eq!(
        received
            .iter()
            .filter(|e| matches!(e, AgentTurnEvent::ToolCallStarted { .. }))
            .count(),
        1
    );
    assert_eq!(
        received
            .iter()
            .filter(|e| matches!(e, AgentTurnEvent::ToolCallFinished { ok: true, .. }))
            .count(),
        1
    );
    assert_eq!(
        received
            .iter()
            .filter(|e| matches!(e, AgentTurnEvent::ModelStarted { .. }))
            .count(),
        1
    );
}

// ---------------------------------------------------------------------------
// Scenario 3 — live sender mirrors result.events (band B3, sample 1)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn scenario_03_live_sender_mirrors_result_events() {
    // Round 1: a few tokens + reasoning + one valid tool call (Done(tool_calls)).
    // Round 2: more tokens + Done(stop). Concurrent drain (capacity 32) is
    // mandatory or the bounded channel deadlocks.
    let llm = Arc::new(
        FakeStreamingLlmClient::builder()
            .push_events(vec![
                LlmEvent::Token {
                    content: "thi".to_string(),
                },
                LlmEvent::Reasoning {
                    content: "let me think".to_string(),
                },
                LlmEvent::Token {
                    content: "nking".to_string(),
                },
                LlmEvent::ToolCallStart {
                    id: "c1".to_string(),
                    name: "fake_tool".to_string(),
                },
                LlmEvent::ToolCallDelta {
                    id: "c1".to_string(),
                    arguments_delta: "{}".to_string(),
                },
                LlmEvent::ToolCallEnd {
                    id: "c1".to_string(),
                },
                LlmEvent::Done {
                    finish_reason: "tool_calls".to_string(),
                    usage: None,
                },
            ])
            .push_events(vec![
                LlmEvent::Token {
                    content: "fin".to_string(),
                },
                LlmEvent::Token {
                    content: "al".to_string(),
                },
                LlmEvent::Done {
                    finish_reason: "stop".to_string(),
                    usage: None,
                },
            ])
            .build(),
    );

    let executor = Arc::new(FakeToolExecutor::new(vec![Ok("tool-out".to_string())]));

    let loop_inst = AgentLoop::new(AgentLoopConfig {
        llm,
        tool_executor: executor,
    });

    let (result, received) = drive_run_turn_live(&loop_inst, make_request("hello"), 32).await;

    assert_eq!(result.status, AgentTurnStatus::Completed);

    // Element-wise equality of Vec<AgentTurnEvent> proves the live sink mirrors
    // the buffered result events.
    assert_eq!(received, result.events);
}

// ---------------------------------------------------------------------------
// Scenario 4 — dropping the receiver does not cancel the turn
// ---------------------------------------------------------------------------

#[tokio::test]
async fn scenario_04_receiver_drop_does_not_cancel_turn() {
    let llm = Arc::new(
        FakeStreamingLlmClient::builder()
            .push_events(vec![
                LlmEvent::Token {
                    content: "hi ".to_string(),
                },
                LlmEvent::Token {
                    content: "there".to_string(),
                },
                LlmEvent::Done {
                    finish_reason: "stop".to_string(),
                    usage: None,
                },
            ])
            .build(),
    );

    let executor = Arc::new(FakeToolExecutor::new(vec![]));

    let loop_inst = AgentLoop::new(AgentLoopConfig {
        llm,
        tool_executor: executor,
    });

    let (tx, rx) = tokio::sync::mpsc::channel::<AgentTurnEvent>(8);
    // Drop the receiver before any send has happened. Live mirror uses
    // non-blocking `try_send` (M-19a); the turn must still complete normally.
    drop(rx);

    let result = loop_inst.run_turn_live(make_request("hello"), tx).await;

    assert_eq!(result.status, AgentTurnStatus::Completed);
    // Buffered events still populated end-to-end (ModelStarted .. ModelFinished).
    assert!(
        result.events.len() >= 2,
        "buffered events should still be populated after receiver drop"
    );
    assert!(result
        .events
        .iter()
        .any(|e| matches!(e, AgentTurnEvent::ModelStarted { .. })));
    assert!(result
        .events
        .iter()
        .any(|e| matches!(e, AgentTurnEvent::ModelFinished { .. })));
    // Assistant message present.
    assert_eq!(
        result
            .messages
            .iter()
            .filter(|m| m.role == "assistant")
            .count(),
        1
    );
    assert_eq!(result.assistant_output, "hi there");
}

// ---------------------------------------------------------------------------
// Scenario 4b — full live channel without drain does not block the agent loop
// ---------------------------------------------------------------------------

#[tokio::test]
async fn scenario_04b_full_live_channel_without_drain_completes() {
    let mut token_events = Vec::new();
    for i in 0..200 {
        token_events.push(LlmEvent::Token {
            content: format!("t{i}"),
        });
    }
    token_events.push(LlmEvent::Done {
        finish_reason: "stop".to_string(),
        usage: None,
    });

    let llm = Arc::new(
        FakeStreamingLlmClient::builder()
            .push_events(token_events)
            .build(),
    );
    let loop_inst = AgentLoop::new(AgentLoopConfig {
        llm,
        tool_executor: Arc::new(FakeToolExecutor::new(vec![])),
    });

    let (tx, _rx) = tokio::sync::mpsc::channel::<AgentTurnEvent>(4);
    // Receiver exists but is never read — channel fills; M-19a must not block.

    let result = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        loop_inst.run_turn_live(make_request("flood"), tx),
    )
    .await
    .expect("run_turn_live must not deadlock on full live channel");

    assert_eq!(result.status, AgentTurnStatus::Completed);
    assert!(
        result
            .events
            .iter()
            .any(|e| matches!(e, AgentTurnEvent::ModelFinished { .. })),
        "buffered events must remain complete under backpressure"
    );
}

// ---------------------------------------------------------------------------
// CancelBetweenTokensLlm — flips the cancel flag between two Token events.
// ---------------------------------------------------------------------------

/// Streaming LlmClient that emits Token("p"), flips a shared cancel flag, then
/// would emit Token("ar"), Token("tial"), Done — but per checkpoint #2 the
/// consumer returns `Cancelled` after processing Token("ar"), so the trailing
/// Token("tial") and Done are never observed.
///
/// `FakeStreamingLlmClient` emits its queue synchronously from a `Vec`
/// (`testing.rs:94`) and cannot interleave a side-effect between events; this
/// custom client uses `futures::stream::unfold` so the cancel flip lands in
/// the state transition between yields. No new dev-dependency required —
/// `futures` is already in `Cargo.toml`.
struct CancelBetweenTokensLlm {
    cancel: Arc<AtomicBool>,
}

#[derive(Clone, Copy)]
enum CancelStreamState {
    BeforeP,
    BeforeAr,
    BeforeTial,
    BeforeDone,
    Done,
}

#[async_trait::async_trait]
impl LlmClient for CancelBetweenTokensLlm {
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
        let cancel = self.cancel.clone();
        let stream = futures::stream::unfold(CancelStreamState::BeforeP, move |state| {
            let cancel = cancel.clone();
            async move {
                match state {
                    CancelStreamState::BeforeP => Some((
                        LlmEvent::Token {
                            content: "p".to_string(),
                        },
                        CancelStreamState::BeforeAr,
                    )),
                    CancelStreamState::BeforeAr => {
                        // Flip cancel BEFORE yielding "ar". The consumer
                        // processes "ar" (emit + accumulate), then checkpoint
                        // #2 fires and returns Cancelled — "tial" and Done
                        // are never reached.
                        cancel.store(true, Ordering::Relaxed);
                        Some((
                            LlmEvent::Token {
                                content: "ar".to_string(),
                            },
                            CancelStreamState::BeforeTial,
                        ))
                    }
                    CancelStreamState::BeforeTial => Some((
                        LlmEvent::Token {
                            content: "tial".to_string(),
                        },
                        CancelStreamState::BeforeDone,
                    )),
                    CancelStreamState::BeforeDone => Some((
                        LlmEvent::Done {
                            finish_reason: "stop".to_string(),
                            usage: None,
                        },
                        CancelStreamState::Done,
                    )),
                    CancelStreamState::Done => None,
                }
            }
        });
        Ok(Box::pin(stream))
    }
}

// ---------------------------------------------------------------------------
// Scenario 5 — cancel mid-stream preserves partial assistant content
// ---------------------------------------------------------------------------

#[tokio::test]
async fn scenario_05_cancel_mid_stream_preserves_partial_content() {
    let cancel = Arc::new(AtomicBool::new(false));
    let llm = Arc::new(CancelBetweenTokensLlm {
        cancel: cancel.clone(),
    });
    let executor = Arc::new(FakeToolExecutor::new(vec![]));

    let loop_inst = AgentLoop::new(AgentLoopConfig {
        llm,
        tool_executor: executor,
    });

    let request = make_request_with_cancel("hello", cancel.clone());
    let (result, _received) = drive_run_turn_live(&loop_inst, request, 32).await;

    assert_eq!(result.status, AgentTurnStatus::Cancelled);
    assert_eq!(
        result.terminal_reason,
        Some("cancelled by caller".to_string())
    );

    // Partial assistant content "par" — the consumer emitted "p" (cancel=false
    // at checkpoint #2) and "ar" (cancel=true at checkpoint #2 → returns
    // Cancelled before pulling Token("tial") or Done).
    let assistant_msg = result
        .messages
        .iter()
        .find(|m| m.role == "assistant")
        .expect("partial assistant message should be flushed");
    assert_eq!(assistant_msg.content, "par");
    assert_eq!(
        assistant_msg.reasoning_content, None,
        "no Reasoning event was emitted; reasoning_content must remain None"
    );

    // No ModelFinished event in the Cancelled branch.
    assert!(!result
        .events
        .iter()
        .any(|e| matches!(e, AgentTurnEvent::ModelFinished { .. })));

    // Exactly two TokenDelta events with the expected split.
    let tokens: Vec<&AgentTurnEvent> = result
        .events
        .iter()
        .filter(|e| matches!(e, AgentTurnEvent::TokenDelta { .. }))
        .collect();
    assert_eq!(tokens.len(), 2);
    assert!(matches!(
        tokens[0],
        AgentTurnEvent::TokenDelta { content } if content == "p"
    ));
    assert!(matches!(
        tokens[1],
        AgentTurnEvent::TokenDelta { content } if content == "ar"
    ));

    // assistant_output mirrors the partial content.
    assert_eq!(result.assistant_output, "par");
}

// ---------------------------------------------------------------------------
// Scenario 7 — run_turn buffered regression matches run_turn_live (band B4)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn scenario_07_run_turn_buffered_matches_run_turn_live() {
    // Helper: build a fresh FakeStreamingLlmClient + AgentLoop with the same
    // single-round successful turn fixture. Each invocation gets its own
    // queue because FakeStreamingLlmClient consumes the queue per stream_chat.
    fn build_loop() -> AgentLoop {
        let llm = Arc::new(
            FakeStreamingLlmClient::builder()
                .push_events(vec![
                    LlmEvent::Token {
                        content: "hel".to_string(),
                    },
                    LlmEvent::Token {
                        content: "lo".to_string(),
                    },
                    LlmEvent::Done {
                        finish_reason: "stop".to_string(),
                        usage: None,
                    },
                ])
                .build(),
        );
        let executor = Arc::new(FakeToolExecutor::new(vec![]));
        AgentLoop::new(AgentLoopConfig {
            llm,
            tool_executor: executor,
        })
    }

    // Path A: run_turn (no live sender — None branch in run_turn_inner).
    let loop_a = build_loop();
    let result_a = loop_a.run_turn(make_request("hi")).await;

    // Path B: run_turn_live + concurrent drain (Some(&sender) branch).
    let loop_b = build_loop();
    let (result_b, _received) = drive_run_turn_live(&loop_b, make_request("hi"), 32).await;

    assert_eq!(result_a.status, AgentTurnStatus::Completed);
    assert_eq!(result_b.status, AgentTurnStatus::Completed);
    assert_eq!(result_a.assistant_output, result_b.assistant_output);

    // Buffered events vector identical across both entrypoints.
    assert_eq!(result_a.events, result_b.events);
}

// ---------------------------------------------------------------------------
// Scenario 8 — cooperative deadline timeout preserves partial state (deadline)
// ---------------------------------------------------------------------------

/// LlmClient whose `stream_chat` yields a few Token events with small delays,
/// then hangs forever (never yields Done). Used to prove the cooperative
/// deadline preserves partial content accumulated before the timeout fires.
struct SlowStreamLlm;

#[async_trait::async_trait]
impl LlmClient for SlowStreamLlm {
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
        // State machine: yield Token("partial "), Token("content"), then hang.
        enum StreamState {
            Token1,
            Token2,
            Hanging,
        }

        let stream = futures::stream::unfold(StreamState::Token1, |state| async move {
            match state {
                StreamState::Token1 => {
                    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                    Some((
                        LlmEvent::Token {
                            content: "partial ".to_string(),
                        },
                        StreamState::Token2,
                    ))
                }
                StreamState::Token2 => {
                    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                    Some((
                        LlmEvent::Token {
                            content: "content".to_string(),
                        },
                        StreamState::Hanging,
                    ))
                }
                StreamState::Hanging => {
                    // Never resolve — cooperative deadline will fire
                    futures::future::pending::<()>().await;
                    unreachable!()
                }
            }
        });

        Ok(Box::pin(stream))
    }
}

#[tokio::test]
async fn scenario_08_timeout_cooperative_preserves_partial() {
    let llm = Arc::new(SlowStreamLlm);
    let executor = Arc::new(FakeToolExecutor::new(vec![]));
    let loop_inst = AgentLoop::new(AgentLoopConfig {
        llm,
        tool_executor: executor,
    });

    let mut request = make_request("hello");
    request.timeout = Some(std::time::Duration::from_millis(100));

    let (result, received) = drive_run_turn_live(&loop_inst, request, 32).await;

    // Timeout assertions:
    // (1) status == Error
    assert_eq!(result.status, AgentTurnStatus::Error);
    // (2) terminal_reason contains "timeout"
    assert_eq!(result.terminal_reason, Some("timeout".to_string()));
    // (3) result.events is NOT empty (contains at least ModelStarted, TokenDelta events)
    assert!(
        !result.events.is_empty(),
        "cooperative deadline must preserve events emitted before timeout"
    );
    assert!(
        result
            .events
            .iter()
            .any(|e| matches!(e, AgentTurnEvent::ModelStarted { .. })),
        "ModelStarted must be present in events"
    );
    assert!(
        result
            .events
            .iter()
            .any(|e| matches!(e, AgentTurnEvent::TokenDelta { .. })),
        "TokenDelta events must be present (partial tokens yielded before timeout)"
    );
    // (4) partial content preserved in assistant_output
    assert_eq!(result.assistant_output, "partial content");
    // (5) messages is NOT empty (flush_partial preserves the partial assistant message)
    assert!(
        !result.messages.is_empty(),
        "partial assistant message must be flushed on timeout"
    );
    let assistant_msg = result
        .messages
        .iter()
        .find(|m| m.role == "assistant")
        .expect("partial assistant message should exist");
    assert_eq!(assistant_msg.content, "partial content");
    // (6) received (live events) == result.events (same events, same order)
    assert_eq!(
        received, result.events,
        "live events must match result.events on timeout (run/stream consistency)"
    );
}

// ---------------------------------------------------------------------------
// Scenario 8b — timeout on stream_chat creation (deadline)
// ---------------------------------------------------------------------------

/// LlmClient whose `stream_chat` hangs forever (never returns a stream).
/// Used to prove the cooperative deadline fires on stream creation timeout.
struct HangingStreamChatLlm;

#[async_trait::async_trait]
impl LlmClient for HangingStreamChatLlm {
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
        // Hang forever — never return a stream
        futures::future::pending::<()>().await;
        unreachable!()
    }
}

#[tokio::test]
async fn scenario_08b_timeout_on_stream_creation() {
    let llm = Arc::new(HangingStreamChatLlm);
    let executor = Arc::new(FakeToolExecutor::new(vec![]));
    let loop_inst = AgentLoop::new(AgentLoopConfig {
        llm,
        tool_executor: executor,
    });

    let mut request = make_request("hello");
    request.timeout = Some(std::time::Duration::from_millis(50));

    let (result, received) = drive_run_turn_live(&loop_inst, request, 32).await;

    // Status is Error with timeout reason
    assert_eq!(result.status, AgentTurnStatus::Error);
    assert_eq!(result.terminal_reason, Some("timeout".to_string()));
    // ModelStarted was emitted before stream_chat was called
    assert!(
        result
            .events
            .iter()
            .any(|e| matches!(e, AgentTurnEvent::ModelStarted { .. })),
        "ModelStarted must be emitted before stream_chat call"
    );
    // No TokenDelta events (stream was never created)
    assert!(
        !result
            .events
            .iter()
            .any(|e| matches!(e, AgentTurnEvent::TokenDelta { .. })),
        "no TokenDelta should exist when stream_chat itself timed out"
    );
    // Live events match result.events
    assert_eq!(
        received, result.events,
        "live events must match result.events on stream-creation timeout"
    );
}

// ---------------------------------------------------------------------------
// Scenario 8c — timeout during tool execution (deadline review fix)
// ---------------------------------------------------------------------------

struct HangingToolExecutor;

#[async_trait::async_trait]
impl ToolCallExecutor for HangingToolExecutor {
    async fn execute(&self, _call: &ToolCall) -> Result<String, ToolExecutionError> {
        futures::future::pending::<()>().await;
        unreachable!()
    }
}

#[tokio::test]
async fn scenario_08c_timeout_during_tool_execution_preserves_state() {
    let llm = Arc::new(
        FakeStreamingLlmClient::builder()
            .push_events(vec![
                LlmEvent::ToolCallStart {
                    id: "call_1".to_string(),
                    name: "observe.workspace".to_string(),
                },
                LlmEvent::ToolCallDelta {
                    id: "call_1".to_string(),
                    arguments_delta: "{}".to_string(),
                },
                LlmEvent::ToolCallEnd {
                    id: "call_1".to_string(),
                },
                LlmEvent::Done {
                    finish_reason: "tool_calls".to_string(),
                    usage: None,
                },
            ])
            .build(),
    );
    let executor = Arc::new(HangingToolExecutor);
    let loop_inst = AgentLoop::new(AgentLoopConfig {
        llm,
        tool_executor: executor,
    });

    let mut request = make_request("read a file");
    request.timeout = Some(std::time::Duration::from_millis(50));

    let (result, received) = drive_run_turn_live(&loop_inst, request, 32).await;

    assert_eq!(result.status, AgentTurnStatus::Error);
    assert_eq!(result.terminal_reason, Some("timeout".to_string()));
    assert!(
        result
            .events
            .iter()
            .any(|e| matches!(e, AgentTurnEvent::ModelStarted { .. })),
        "ModelStarted must be preserved"
    );
    assert!(
        result
            .events
            .iter()
            .any(|e| matches!(e, AgentTurnEvent::ModelFinished { .. })),
        "ModelFinished for the tool-call round must be preserved"
    );
    assert!(
        result
            .events
            .iter()
            .any(|e| matches!(e, AgentTurnEvent::ToolCallStarted { .. })),
        "ToolCallStarted must be preserved"
    );
    assert!(
        !result
            .events
            .iter()
            .any(|e| matches!(e, AgentTurnEvent::ToolCallFinished { .. })),
        "timeout during tool execution must not synthesize ToolCallFinished"
    );
    assert_eq!(
        received, result.events,
        "live events must match result.events on tool timeout"
    );

    let assistant_msg = result
        .messages
        .iter()
        .find(|m| m.role == "assistant")
        .expect("assistant tool-call message should be preserved");
    assert!(
        assistant_msg
            .tool_calls
            .as_ref()
            .is_some_and(|calls| calls.len() == 1),
        "assistant tool-call message should retain the pending tool call"
    );
    assert!(
        !result.messages.iter().any(|m| m.role == "tool"),
        "no tool result message should be produced after tool timeout"
    );
}

// ---------------------------------------------------------------------------
// Scenario 8d — expired deadline wins over ready stream (deadline review fix)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn scenario_08d_expired_deadline_wins_over_ready_stream() {
    let llm = Arc::new(
        FakeStreamingLlmClient::builder()
            .push_events(vec![
                LlmEvent::Token {
                    content: "must not be consumed".to_string(),
                },
                LlmEvent::Done {
                    finish_reason: "stop".to_string(),
                    usage: None,
                },
            ])
            .build(),
    );
    let executor = Arc::new(FakeToolExecutor::new(vec![]));
    let loop_inst = AgentLoop::new(AgentLoopConfig {
        llm,
        tool_executor: executor,
    });

    let mut request = make_request("hello");
    request.timeout = Some(std::time::Duration::from_millis(0));

    let (result, received) = drive_run_turn_live(&loop_inst, request, 32).await;

    assert_eq!(result.status, AgentTurnStatus::Error);
    assert_eq!(result.terminal_reason, Some("timeout".to_string()));
    assert!(
        result
            .events
            .iter()
            .any(|e| matches!(e, AgentTurnEvent::ModelStarted { .. })),
        "ModelStarted emitted before the deadline check should be preserved"
    );
    assert!(
        !result
            .events
            .iter()
            .any(|e| matches!(e, AgentTurnEvent::TokenDelta { .. })),
        "expired deadline must win over an immediately ready stream"
    );
    assert!(result.messages.is_empty());
    assert!(result.assistant_output.is_empty());
    assert_eq!(
        received, result.events,
        "live events must match result.events on expired-deadline timeout"
    );
}
