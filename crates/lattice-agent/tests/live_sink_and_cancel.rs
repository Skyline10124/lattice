#![cfg(feature = "test-utils")]
//! Phase 15 behavioral suite: AgentLoop live sink + cooperative cancellation.
//!
//! Locks the seven D-18 scenarios that prove VER-11, AGENT-08, AGENT-09 and the
//! Plan 15-02 invariants exposed by `run_turn_live` + the five new cancel
//! checkpoints (1b / 2 / 3 / 4 / 5). All scenarios with a live sender drive via
//! `drive_run_turn_live`, which `tokio::join!`s the run + drain so the bounded
//! mpsc channel never deadlocks (RESEARCH §5 / D-19).

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::sync::Mutex;

use lattice_agent::testing::FakeStreamingLlmClient;
use lattice_agent::{
    AgentLoop, AgentLoopConfig, AgentTurnEvent, AgentTurnRequest, AgentTurnStatus, LlmClient,
    ToolCallExecutor,
};
use lattice_llm::{LlmEvent, ToolCall};
use lattice_session::HeadTranscript;

// ---------------------------------------------------------------------------
// Test helpers (duplicated from streaming_delta.rs — Rust integration tests
// don't share helpers across files).
// ---------------------------------------------------------------------------

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
        enabled_tools: vec!["read_file".to_string()],
        project_root: "/tmp".to_string(),
        cancel_flag: None,
        timeout: None,
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
        enabled_tools: vec!["read_file".to_string()],
        project_root: "/tmp".to_string(),
        cancel_flag: Some(flag),
        timeout: None,
    }
}

/// Drive `run_turn_live` to completion while concurrently draining the live
/// receiver into a `Vec<AgentTurnEvent>`. `tokio::join!` is intentional so the
/// `&AgentLoop` borrow stays usable; switching to `tokio::spawn` would force
/// callers to wrap `AgentLoop` in `Arc`.
///
/// The bounded channel is sized at 32 (D-20). The drain loop terminates when
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
// Mirrors the shape from streaming_delta.rs (Phase 14 test infrastructure).
// ---------------------------------------------------------------------------

struct FakeToolExecutor {
    results: Mutex<VecDeque<Result<String, String>>>,
    execution_order: Arc<Mutex<Vec<String>>>,
}

impl FakeToolExecutor {
    fn new(results: Vec<Result<String, String>>) -> Self {
        Self {
            results: Mutex::new(VecDeque::from(results)),
            execution_order: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

#[async_trait::async_trait]
impl ToolCallExecutor for FakeToolExecutor {
    async fn execute(&self, call: &ToolCall) -> Result<String, String> {
        self.execution_order.lock().unwrap().push(call.id.clone());
        let mut results = self.results.lock().unwrap();
        match results.pop_front() {
            Some(Ok(output)) => Ok(output),
            Some(Err(msg)) => Err(msg),
            None => Err("no more fake results".to_string()),
        }
    }
}

/// `ToolCallExecutor` wrapper that flips a shared cancel flag the first time
/// `execute` is called, *before* forwarding to the inner executor and returning
/// the inner's real result.
///
/// Drives scenarios #2 (cancel mid-tool, real result preserved per D-07) and
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
    async fn execute(&self, call: &ToolCall) -> Result<String, String> {
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
/// never a cancel point inside `consume_model_stream` (D-03), so the function
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
        enum Phase {
            Pending(VecDeque<LlmEvent>, String, Arc<AtomicBool>),
            DoneEmitted,
        }

        let initial = Phase::Pending(VecDeque::from(pre), finish_reason, cancel);

        let stream = futures::stream::unfold(initial, |state| async move {
            match state {
                Phase::Pending(mut queue, finish_reason, cancel) => {
                    if let Some(event) = queue.pop_front() {
                        Some((event, Phase::Pending(queue, finish_reason, cancel)))
                    } else {
                        // Last non-Done event has been consumed (and checkpoint
                        // #2 fired with cancel still false). Flip cancel, then
                        // yield Done — the cancel is observed at checkpoint #3,
                        // not #2 (Done is never a cancel point per D-03).
                        cancel.store(true, Ordering::Relaxed);
                        let done = LlmEvent::Done {
                            finish_reason,
                            usage: None,
                        };
                        Some((done, Phase::DoneEmitted))
                    }
                }
                Phase::DoneEmitted => None,
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

    // Per D-08: assistant tool-call GraphMessage IS in transcript; tool result
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

    // ModelFinished was emitted (D-08 — model response finalized before cancel).
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
    // returns its real result. Per D-07: agent records the real result, no
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
    // tool is never aborted (D-07).
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
    // mandatory or the bounded channel deadlocks (D-19).
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

    // Element-wise equality of Vec<AgentTurnEvent> via the PartialEq derive
    // landed in Plan 15-01. This is the load-bearing assertion of band B3.
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
    // Drop the receiver before any send has happened. emit_agent_event's
    // `let _ = sender.send(event).await` swallows every SendError; the turn
    // must still complete normally.
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
enum CancelStreamPhase {
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
        let stream = futures::stream::unfold(CancelStreamPhase::BeforeP, move |phase| {
            let cancel = cancel.clone();
            async move {
                match phase {
                    CancelStreamPhase::BeforeP => Some((
                        LlmEvent::Token {
                            content: "p".to_string(),
                        },
                        CancelStreamPhase::BeforeAr,
                    )),
                    CancelStreamPhase::BeforeAr => {
                        // Flip cancel BEFORE yielding "ar". The consumer
                        // processes "ar" (emit + accumulate), then checkpoint
                        // #2 fires and returns Cancelled — "tial" and Done
                        // are never reached.
                        cancel.store(true, Ordering::Relaxed);
                        Some((
                            LlmEvent::Token {
                                content: "ar".to_string(),
                            },
                            CancelStreamPhase::BeforeTial,
                        ))
                    }
                    CancelStreamPhase::BeforeTial => Some((
                        LlmEvent::Token {
                            content: "tial".to_string(),
                        },
                        CancelStreamPhase::BeforeDone,
                    )),
                    CancelStreamPhase::BeforeDone => Some((
                        LlmEvent::Done {
                            finish_reason: "stop".to_string(),
                            usage: None,
                        },
                        CancelStreamPhase::Done,
                    )),
                    CancelStreamPhase::Done => None,
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
        .expect("partial assistant message should be flushed via D-04 helper");
    assert_eq!(assistant_msg.content, "par");
    assert_eq!(
        assistant_msg.reasoning_content, None,
        "no Reasoning event was emitted; reasoning_content must remain None"
    );

    // No ModelFinished event in the Cancelled branch (D-04).
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

    // Buffered events vector identical across both entrypoints — the B4
    // backward-compat invariant. Relies on PartialEq from Plan 15-01.
    assert_eq!(result_a.events, result_b.events);
}

// ---------------------------------------------------------------------------
// Scenario 8 (optional D-21) — run_turn_live honors AgentTurnRequest.timeout
// ---------------------------------------------------------------------------

/// LlmClient whose `stream_chat` returns a stream that never yields. Used to
/// drive the timeout helper without racing against any wallclock fixture.
struct PendingStreamLlm;

#[async_trait::async_trait]
impl LlmClient for PendingStreamLlm {
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
        Ok(Box::pin(futures::stream::pending::<LlmEvent>()))
    }
}

#[tokio::test]
async fn scenario_08_run_turn_live_honors_timeout() {
    let llm = Arc::new(PendingStreamLlm);
    let executor = Arc::new(FakeToolExecutor::new(vec![]));
    let loop_inst = AgentLoop::new(AgentLoopConfig {
        llm,
        tool_executor: executor,
    });

    let mut request = make_request("hello");
    request.timeout = Some(std::time::Duration::from_millis(50));

    let (result, _received) = drive_run_turn_live(&loop_inst, request, 32).await;

    // apply_optional_timeout returns the buffered timeout shape (D-10).
    assert_eq!(result.status, AgentTurnStatus::Error);
    assert_eq!(result.terminal_reason, Some("timeout".to_string()));
    assert!(result.messages.is_empty());
    assert!(result.assistant_output.is_empty());
    assert!(result.events.is_empty());
}
