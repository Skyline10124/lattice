#![cfg(feature = "test-utils")]

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::Mutex;

use lattice_agent::testing::FakeStreamingLlmClient;
use lattice_agent::{
    AgentLoop, AgentLoopConfig, AgentTurnEvent, AgentTurnRequest, AgentTurnStatus, LlmClient,
    LlmRetryConfig, ToolCallExecutor, ToolExecutionError,
};
use lattice_llm::{LlmError, LlmEvent, TokenUsage, ToolDefinition};
use lattice_session::HeadTranscript;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn read_file_tool_definition() -> ToolDefinition {
    ToolDefinition::new(
        "workspace.read_file",
        "Read a file",
        serde_json::json!({"type": "object", "properties": {"path": {"type": "string"}}}),
    )
}

fn make_request(user_input: &str, tool_definitions: Vec<ToolDefinition>) -> AgentTurnRequest {
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
        tool_definitions,
        cancel_flag: None,
        timeout: None,
        llm_retry: LlmRetryConfig::default(),
    }
}

/// Tool executor that returns queued Ok/Err results in FIFO order
/// and records which tool-call IDs were executed.
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

    fn execution_order(&self) -> Arc<Mutex<Vec<String>>> {
        self.execution_order.clone()
    }
}

#[async_trait::async_trait]
impl ToolCallExecutor for FakeToolExecutor {
    async fn execute(&self, call: &lattice_llm::ToolCall) -> Result<String, ToolExecutionError> {
        self.execution_order.lock().unwrap().push(call.id.clone());
        let mut results = self.results.lock().unwrap();
        match results.pop_front() {
            Some(result) => result,
            None => Err(ToolExecutionError::simple("no more fake results")),
        }
    }
}

/// Chat-only fake LLM that implements only `chat()` with no `stream_chat`
/// override. Proves the default buffered fallback still works after AgentLoop
/// switched to `stream_chat`.
struct ChatOnlyFakeLlm {
    responses: Mutex<VecDeque<lattice_llm::ChatResponse>>,
}

impl ChatOnlyFakeLlm {
    fn new(responses: Vec<lattice_llm::ChatResponse>) -> Self {
        Self {
            responses: Mutex::new(VecDeque::from(responses)),
        }
    }
}

#[async_trait::async_trait]
impl LlmClient for ChatOnlyFakeLlm {
    async fn chat(
        &self,
        _request: lattice_llm::ChatRequest,
    ) -> Result<lattice_llm::ChatResponse, Box<dyn std::error::Error + Send + Sync>> {
        let mut responses = self.responses.lock().unwrap();
        match responses.pop_front() {
            Some(resp) => Ok(resp),
            None => Err("no more fake responses".into()),
        }
    }
}

// ---------------------------------------------------------------------------
// Scenario 1: multi-chunk token assembly.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn streaming_multi_chunk_tokens_assemble_into_one_message() {
    let llm = Arc::new(
        FakeStreamingLlmClient::builder()
            .push_events(vec![
                LlmEvent::Token {
                    content: "p".to_string(),
                },
                LlmEvent::Token {
                    content: "ong".to_string(),
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

    let result = loop_inst
        .run_turn(make_request("ping", vec![read_file_tool_definition()]))
        .await;

    assert_eq!(result.status, AgentTurnStatus::Completed);
    assert_eq!(result.assistant_output, "pong");
    assert_eq!(result.messages.len(), 1);
    assert_eq!(result.messages[0].content, "pong");

    // Verify TokenDelta events for each chunk and ModelFinished
    assert!(result
        .events
        .iter()
        .any(|e| matches!(e, AgentTurnEvent::TokenDelta { content } if content == "p")));
    assert!(result
        .events
        .iter()
        .any(|e| matches!(e, AgentTurnEvent::TokenDelta { content } if content == "ong")));
    assert!(result.events.iter().any(|e| matches!(e, AgentTurnEvent::ModelFinished { finish_reason, .. } if finish_reason == "stop")));
}

// ---------------------------------------------------------------------------
// Scenario 2: reasoning separate from visible output.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn streaming_reasoning_separate_from_visible() {
    let llm = Arc::new(
        FakeStreamingLlmClient::builder()
            .push_events(vec![
                LlmEvent::Reasoning {
                    content: "think".to_string(),
                },
                LlmEvent::Token {
                    content: "answer".to_string(),
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

    let result = loop_inst
        .run_turn(make_request("what?", vec![read_file_tool_definition()]))
        .await;

    assert_eq!(result.status, AgentTurnStatus::Completed);
    assert_eq!(result.assistant_output, "answer");
    assert_eq!(result.messages[0].content, "answer");
    assert_eq!(
        result.messages[0].reasoning_content,
        Some("think".to_string())
    );
}

// ---------------------------------------------------------------------------
// Scenario 3: tool call assembly and sequential execution.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn streaming_tool_call_assembly_and_sequential_execution() {
    let llm = Arc::new(
        FakeStreamingLlmClient::builder()
            .push_events(vec![
                LlmEvent::ToolCallStart {
                    id: "c1".to_string(),
                    name: "workspace.read_file".to_string(),
                },
                LlmEvent::ToolCallDelta {
                    id: "c1".to_string(),
                    arguments_delta: r#"{"path":"#.to_string(),
                },
                LlmEvent::ToolCallDelta {
                    id: "c1".to_string(),
                    arguments_delta: r#""a.txt"}"#.to_string(),
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
                    content: "done".to_string(),
                },
                LlmEvent::Done {
                    finish_reason: "stop".to_string(),
                    usage: None,
                },
            ])
            .build(),
    );

    let executor = Arc::new(FakeToolExecutor::new(vec![Ok("file content".to_string())]));
    let order = executor.execution_order();
    let loop_inst = AgentLoop::new(AgentLoopConfig {
        llm,
        tool_executor: executor,
    });

    let result = loop_inst
        .run_turn(make_request(
            "read a.txt",
            vec![read_file_tool_definition()],
        ))
        .await;

    assert_eq!(result.status, AgentTurnStatus::Completed);
    assert_eq!(result.tool_results.len(), 1);
    assert!(result.tool_results[0].ok);
    assert_eq!(result.tool_results[0].name, "workspace.read_file");

    // Verify execution order: c1 was executed
    let exec_order = order.lock().unwrap();
    assert_eq!(*exec_order, vec!["c1"]);

    // Verify event ordering: ModelFinished(tool_calls) < ToolCallStarted(c1) < ToolCallFinished(c1)
    let idx_model_finished_tc = result.events.iter().position(|e| {
        matches!(e, AgentTurnEvent::ModelFinished { finish_reason, .. } if finish_reason == "tool_calls")
    }).expect("should have ModelFinished with tool_calls");
    let idx_tool_started = result
        .events
        .iter()
        .position(|e| matches!(e, AgentTurnEvent::ToolCallStarted { id, .. } if id == "c1"))
        .expect("should have ToolCallStarted for c1");
    let idx_tool_finished = result
        .events
        .iter()
        .position(|e| matches!(e, AgentTurnEvent::ToolCallFinished { id, .. } if id == "c1"))
        .expect("should have ToolCallFinished for c1");

    assert!(
        idx_model_finished_tc < idx_tool_started,
        "ModelFinished(tool_calls) should precede ToolCallStarted"
    );
    assert!(
        idx_tool_started < idx_tool_finished,
        "ToolCallStarted should precede ToolCallFinished"
    );
}

// ---------------------------------------------------------------------------
// Scenario 4: incomplete tool call error, no execution.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn streaming_incomplete_tool_call_error_no_execution() {
    let llm = Arc::new(
        FakeStreamingLlmClient::builder()
            .push_events(vec![
                LlmEvent::Token {
                    content: "partial".to_string(),
                },
                LlmEvent::ToolCallStart {
                    id: "c1".to_string(),
                    name: "workspace.read_file".to_string(),
                },
                LlmEvent::ToolCallDelta {
                    id: "c1".to_string(),
                    arguments_delta: r#"{"path": "a"#.to_string(),
                },
                LlmEvent::Error {
                    message: "connection reset".to_string(),
                },
            ])
            .build(),
    );

    let executor = Arc::new(FakeToolExecutor::new(vec![]));
    let loop_inst = AgentLoop::new(AgentLoopConfig {
        llm,
        tool_executor: executor,
    });

    let result = loop_inst
        .run_turn(make_request("read a", vec![read_file_tool_definition()]))
        .await;

    assert_eq!(result.status, AgentTurnStatus::Error);
    assert_eq!(result.assistant_output, "partial");
    assert!(result.tool_results.is_empty());

    // Partial content preserved in message, no tool_calls
    assert_eq!(result.messages[0].content, "partial");
    assert!(result.messages[0].tool_calls.is_none());

    // No ModelFinished event on error path
    assert!(!result
        .events
        .iter()
        .any(|e| matches!(e, AgentTurnEvent::ModelFinished { .. })));
}

// ---------------------------------------------------------------------------
// Scenario 5: invalid tool call JSON, no execution.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn streaming_invalid_tool_call_json_no_execution() {
    let llm = Arc::new(
        FakeStreamingLlmClient::builder()
            .push_events(vec![
                LlmEvent::ToolCallStart {
                    id: "c1".to_string(),
                    name: "workspace.read_file".to_string(),
                },
                LlmEvent::ToolCallDelta {
                    id: "c1".to_string(),
                    arguments_delta: "not valid json".to_string(),
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

    let executor = Arc::new(FakeToolExecutor::new(vec![]));
    let loop_inst = AgentLoop::new(AgentLoopConfig {
        llm,
        tool_executor: executor,
    });

    let result = loop_inst
        .run_turn(make_request("read", vec![read_file_tool_definition()]))
        .await;

    assert_eq!(result.status, AgentTurnStatus::Error);
    assert!(result
        .terminal_reason
        .as_ref()
        .unwrap()
        .contains("invalid/incomplete tool call"));
    assert!(
        result.messages.is_empty(),
        "malformed tool-only response should not commit an empty assistant message"
    );
    assert!(result.tool_results.is_empty());

    // No ToolCallStarted event (invalid calls are not executed)
    assert!(!result
        .events
        .iter()
        .any(|e| matches!(e, AgentTurnEvent::ToolCallStarted { .. })));
    // No ModelFinished event (invalid tool calls path does not emit ModelFinished)
    assert!(!result
        .events
        .iter()
        .any(|e| matches!(e, AgentTurnEvent::ModelFinished { .. })));
}

#[tokio::test]
async fn streaming_tool_call_delta_after_end_is_error_no_execution() {
    let llm = Arc::new(
        FakeStreamingLlmClient::builder()
            .push_events(vec![
                LlmEvent::ToolCallStart {
                    id: "c1".to_string(),
                    name: "workspace.read_file".to_string(),
                },
                LlmEvent::ToolCallDelta {
                    id: "c1".to_string(),
                    arguments_delta: r#"{"path":"a.txt"}"#.to_string(),
                },
                LlmEvent::ToolCallEnd {
                    id: "c1".to_string(),
                },
                LlmEvent::ToolCallDelta {
                    id: "c1".to_string(),
                    arguments_delta: r#"{"path":"b.txt"}"#.to_string(),
                },
                LlmEvent::Done {
                    finish_reason: "tool_calls".to_string(),
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

    let result = loop_inst
        .run_turn(make_request("read", vec![read_file_tool_definition()]))
        .await;

    assert_eq!(result.status, AgentTurnStatus::Error);
    assert!(result
        .terminal_reason
        .as_ref()
        .unwrap()
        .contains("delta after end"));
    assert!(result.tool_results.is_empty());
    assert!(result
        .messages
        .iter()
        .all(|message| message.tool_calls.is_none()));
    assert!(!result
        .events
        .iter()
        .any(|e| matches!(e, AgentTurnEvent::ToolCallStarted { .. })));
    assert!(!result
        .events
        .iter()
        .any(|e| matches!(e, AgentTurnEvent::ModelFinished { .. })));
}

#[tokio::test]
async fn streaming_duplicate_tool_call_end_is_error_no_execution() {
    let llm = Arc::new(
        FakeStreamingLlmClient::builder()
            .push_events(vec![
                LlmEvent::ToolCallStart {
                    id: "c1".to_string(),
                    name: "workspace.read_file".to_string(),
                },
                LlmEvent::ToolCallDelta {
                    id: "c1".to_string(),
                    arguments_delta: r#"{"path":"a.txt"}"#.to_string(),
                },
                LlmEvent::ToolCallEnd {
                    id: "c1".to_string(),
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

    let executor = Arc::new(FakeToolExecutor::new(vec![]));
    let loop_inst = AgentLoop::new(AgentLoopConfig {
        llm,
        tool_executor: executor,
    });

    let result = loop_inst
        .run_turn(make_request("read", vec![read_file_tool_definition()]))
        .await;

    assert_eq!(result.status, AgentTurnStatus::Error);
    assert!(result
        .terminal_reason
        .as_ref()
        .unwrap()
        .contains("duplicate end"));
    assert!(result.tool_results.is_empty());
    assert!(result
        .messages
        .iter()
        .all(|message| message.tool_calls.is_none()));
    assert!(!result
        .events
        .iter()
        .any(|e| matches!(e, AgentTurnEvent::ToolCallStarted { .. })));
    assert!(!result
        .events
        .iter()
        .any(|e| matches!(e, AgentTurnEvent::ModelFinished { .. })));
}

// ---------------------------------------------------------------------------
// Scenario 6: multiple valid tool calls, sequential execution.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn streaming_multiple_tool_calls_all_valid_sequential() {
    let llm = Arc::new(
        FakeStreamingLlmClient::builder()
            .push_events(vec![
                LlmEvent::ToolCallStart {
                    id: "c1".to_string(),
                    name: "workspace.read_file".to_string(),
                },
                LlmEvent::ToolCallDelta {
                    id: "c1".to_string(),
                    arguments_delta: r#"{"path":"a.txt"}"#.to_string(),
                },
                LlmEvent::ToolCallEnd {
                    id: "c1".to_string(),
                },
                LlmEvent::ToolCallStart {
                    id: "c2".to_string(),
                    name: "workspace.read_file".to_string(),
                },
                LlmEvent::ToolCallDelta {
                    id: "c2".to_string(),
                    arguments_delta: r#"{"path":"b.txt"}"#.to_string(),
                },
                LlmEvent::ToolCallEnd {
                    id: "c2".to_string(),
                },
                LlmEvent::Done {
                    finish_reason: "tool_calls".to_string(),
                    usage: None,
                },
            ])
            .push_events(vec![
                LlmEvent::Token {
                    content: "done".to_string(),
                },
                LlmEvent::Done {
                    finish_reason: "stop".to_string(),
                    usage: None,
                },
            ])
            .build(),
    );

    let executor = Arc::new(FakeToolExecutor::new(vec![
        Ok("a content".to_string()),
        Ok("b content".to_string()),
    ]));
    let order = executor.execution_order();
    let loop_inst = AgentLoop::new(AgentLoopConfig {
        llm,
        tool_executor: executor,
    });

    let result = loop_inst
        .run_turn(make_request("read both", vec![read_file_tool_definition()]))
        .await;

    assert_eq!(result.status, AgentTurnStatus::Completed);
    assert_eq!(result.tool_results.len(), 2);

    let exec_order = order.lock().unwrap();
    assert_eq!(*exec_order, vec!["c1", "c2"]);
}

// ---------------------------------------------------------------------------
// Scenario 7: multiple tool calls, one invalid means none execute.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn streaming_multiple_tool_calls_one_invalid_none_executed() {
    let llm = Arc::new(
        FakeStreamingLlmClient::builder()
            .push_events(vec![
                LlmEvent::ToolCallStart {
                    id: "c1".to_string(),
                    name: "workspace.read_file".to_string(),
                },
                LlmEvent::ToolCallDelta {
                    id: "c1".to_string(),
                    arguments_delta: r#"{"path":"a.txt"}"#.to_string(),
                },
                LlmEvent::ToolCallEnd {
                    id: "c1".to_string(),
                },
                LlmEvent::ToolCallStart {
                    id: "c2".to_string(),
                    name: "bad_tool".to_string(),
                },
                LlmEvent::ToolCallDelta {
                    id: "c2".to_string(),
                    arguments_delta: "{{invalid".to_string(),
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

    let executor = Arc::new(FakeToolExecutor::new(vec![]));
    let loop_inst = AgentLoop::new(AgentLoopConfig {
        llm,
        tool_executor: executor,
    });

    let result = loop_inst
        .run_turn(make_request(
            "call tools",
            vec![read_file_tool_definition()],
        ))
        .await;

    assert_eq!(result.status, AgentTurnStatus::Error);
    assert!(result.tool_results.is_empty());
}

// ---------------------------------------------------------------------------
// Scenario 8: mid-stream error preserves partial content.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn streaming_mid_stream_error_preserves_partial_content() {
    let llm = Arc::new(
        FakeStreamingLlmClient::builder()
            .push_events(vec![
                LlmEvent::Token {
                    content: "hello ".to_string(),
                },
                LlmEvent::Reasoning {
                    content: "thinking".to_string(),
                },
                LlmEvent::Token {
                    content: "world".to_string(),
                },
                LlmEvent::Error {
                    message: "provider timeout".to_string(),
                },
            ])
            .build(),
    );

    let executor = Arc::new(FakeToolExecutor::new(vec![]));
    let loop_inst = AgentLoop::new(AgentLoopConfig {
        llm,
        tool_executor: executor,
    });

    let result = loop_inst
        .run_turn(make_request("hello", vec![read_file_tool_definition()]))
        .await;

    assert_eq!(result.status, AgentTurnStatus::Error);
    assert_eq!(result.assistant_output, "hello world");
    assert_eq!(result.messages[0].content, "hello world");
    assert_eq!(
        result.messages[0].reasoning_content,
        Some("thinking".to_string())
    );

    // No ModelFinished on error
    assert!(!result
        .events
        .iter()
        .any(|e| matches!(e, AgentTurnEvent::ModelFinished { .. })));

    // Terminal reason mentions LLM error
    assert!(result
        .terminal_reason
        .as_ref()
        .unwrap()
        .contains("LLM error"));
}

// ---------------------------------------------------------------------------
// Scenario 9: stream exhaustion without Done is an error.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn streaming_exhausted_without_done_is_error() {
    let llm = Arc::new(
        FakeStreamingLlmClient::builder()
            .push_events(vec![
                LlmEvent::Token {
                    content: "partial".to_string(),
                },
                LlmEvent::Reasoning {
                    content: "thought".to_string(),
                },
            ])
            .build(),
    );

    let executor = Arc::new(FakeToolExecutor::new(vec![]));
    let loop_inst = AgentLoop::new(AgentLoopConfig {
        llm,
        tool_executor: executor,
    });

    let result = loop_inst
        .run_turn(make_request("hello", vec![read_file_tool_definition()]))
        .await;

    assert_eq!(result.status, AgentTurnStatus::Error);
    assert_eq!(result.assistant_output, "partial");
    assert_eq!(
        result.messages[0].reasoning_content,
        Some("thought".to_string())
    );

    assert!(result
        .terminal_reason
        .as_ref()
        .unwrap()
        .contains("stream ended without completion"));

    // No ModelFinished on exhausted stream
    assert!(!result
        .events
        .iter()
        .any(|e| matches!(e, AgentTurnEvent::ModelFinished { .. })));
}

// ---------------------------------------------------------------------------
// Scenario 10: failed tool does not make turn error.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn streaming_failed_tool_does_not_make_turn_error() {
    let llm = Arc::new(
        FakeStreamingLlmClient::builder()
            .push_events(vec![
                LlmEvent::ToolCallStart {
                    id: "c1".to_string(),
                    name: "workspace.read_file".to_string(),
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
                    content: "recovered".to_string(),
                },
                LlmEvent::Done {
                    finish_reason: "stop".to_string(),
                    usage: None,
                },
            ])
            .build(),
    );

    let executor = Arc::new(FakeToolExecutor::new(vec![Err(
        ToolExecutionError::simple("file not found"),
    )]));
    let loop_inst = AgentLoop::new(AgentLoopConfig {
        llm,
        tool_executor: executor,
    });

    let result = loop_inst
        .run_turn(make_request(
            "read missing",
            vec![read_file_tool_definition()],
        ))
        .await;

    assert_eq!(result.status, AgentTurnStatus::Completed);
    assert_eq!(result.assistant_output, "recovered");

    assert!(!result.tool_results[0].ok);
    assert!(result.tool_results[0]
        .error_summary
        .as_ref()
        .unwrap()
        .contains("file not found"));
}

// ---------------------------------------------------------------------------
// Scenario 11: multi-round tool loop.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn streaming_multi_round_tool_loop() {
    let llm = Arc::new(
        FakeStreamingLlmClient::builder()
            .push_events(vec![
                LlmEvent::ToolCallStart {
                    id: "c1".to_string(),
                    name: "workspace.read_file".to_string(),
                },
                LlmEvent::ToolCallDelta {
                    id: "c1".to_string(),
                    arguments_delta: r#"{"path":"a.txt"}"#.to_string(),
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
                LlmEvent::ToolCallStart {
                    id: "c2".to_string(),
                    name: "workspace.read_file".to_string(),
                },
                LlmEvent::ToolCallDelta {
                    id: "c2".to_string(),
                    arguments_delta: r#"{"path":"b.txt"}"#.to_string(),
                },
                LlmEvent::ToolCallEnd {
                    id: "c2".to_string(),
                },
                LlmEvent::Done {
                    finish_reason: "tool_calls".to_string(),
                    usage: None,
                },
            ])
            .push_events(vec![
                LlmEvent::Token {
                    content: "final".to_string(),
                },
                LlmEvent::Done {
                    finish_reason: "stop".to_string(),
                    usage: Some(TokenUsage {
                        prompt_tokens: 100,
                        completion_tokens: 50,
                        total_tokens: 150,
                    }),
                },
            ])
            .build(),
    );

    let executor = Arc::new(FakeToolExecutor::new(vec![
        Ok("a content".to_string()),
        Ok("b content".to_string()),
    ]));
    let loop_inst = AgentLoop::new(AgentLoopConfig {
        llm,
        tool_executor: executor,
    });

    let result = loop_inst
        .run_turn(make_request(
            "multi-round",
            vec![read_file_tool_definition()],
        ))
        .await;

    assert_eq!(result.status, AgentTurnStatus::Completed);
    assert_eq!(result.assistant_output, "final");
    assert_eq!(result.tool_results.len(), 2);

    // Usage is accumulated across rounds (>= 150 from final round alone)
    assert!(result.usage.total_tokens >= 150);
}

// ---------------------------------------------------------------------------
// Scenario 12: Started event ignored, no double ModelStarted.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn streaming_started_event_ignored_no_double_model_started() {
    let llm = Arc::new(
        FakeStreamingLlmClient::builder()
            .push_events(vec![
                LlmEvent::Started,
                LlmEvent::Token {
                    content: "hi".to_string(),
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

    let result = loop_inst
        .run_turn(make_request("hello", vec![read_file_tool_definition()]))
        .await;

    assert_eq!(result.status, AgentTurnStatus::Completed);
    assert_eq!(result.assistant_output, "hi");

    // Exactly one ModelStarted event (from AgentLoop, not from LlmEvent::Started)
    let model_started_count = result
        .events
        .iter()
        .filter(|e| matches!(e, AgentTurnEvent::ModelStarted { .. }))
        .count();
    assert_eq!(
        model_started_count, 1,
        "LlmEvent::Started should not produce a second ModelStarted"
    );
}

// ---------------------------------------------------------------------------
// Scenario 13: backward compatibility, chat-only FakeLlm still works.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn streaming_backward_compat_fake_llm_chat_only_still_works() {
    let llm = Arc::new(ChatOnlyFakeLlm::new(vec![lattice_llm::ChatResponse {
        content: "buffered".to_string(),
        reasoning_content: None,
        tool_calls: None,
        finish_reason: "stop".to_string(),
        model: "test-model".to_string(),
        usage: None,
    }]));

    let executor = Arc::new(FakeToolExecutor::new(vec![]));
    let loop_inst = AgentLoop::new(AgentLoopConfig {
        llm,
        tool_executor: executor,
    });

    let result = loop_inst
        .run_turn(make_request("hello", vec![read_file_tool_definition()]))
        .await;

    assert_eq!(result.status, AgentTurnStatus::Completed);
    assert_eq!(result.assistant_output, "buffered");
}

// ---------------------------------------------------------------------------
// Scenario 14: stream-open LLM failures are classified and retried.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn streaming_open_retries_retryable_transport_error_then_succeeds() {
    let llm = Arc::new(
        FakeStreamingLlmClient::builder()
            .push_llm_error(LlmError::Transport {
                message: "connection reset".to_string(),
                retryable: true,
            })
            .push_llm_error(LlmError::Transport {
                message: "temporary eof".to_string(),
                retryable: true,
            })
            .push_events(vec![
                LlmEvent::Token {
                    content: "ok".to_string(),
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
        llm: llm.clone(),
        tool_executor: executor,
    });
    let mut request = make_request("hello", vec![read_file_tool_definition()]);
    request.llm_retry = LlmRetryConfig {
        max_retries: 2,
        initial_delay_ms: 1,
        max_delay_ms: 1,
    };

    let result = loop_inst.run_turn(request).await;

    assert_eq!(result.status, AgentTurnStatus::Completed);
    assert_eq!(result.assistant_output, "ok");
    assert_eq!(llm.call_count(), 3);

    let retry_events: Vec<_> = result
        .events
        .iter()
        .filter_map(|event| match event {
            AgentTurnEvent::LlmRequestError {
                attempt,
                max_attempts,
                error_kind,
                retryable,
                will_retry,
                delay_ms,
                ..
            } => Some((
                *attempt,
                *max_attempts,
                error_kind.as_str(),
                *retryable,
                *will_retry,
                *delay_ms,
            )),
            _ => None,
        })
        .collect();

    assert_eq!(
        retry_events,
        vec![
            (1, 3, "transport", true, true, Some(1)),
            (2, 3, "transport", true, true, Some(1))
        ]
    );
}

#[tokio::test]
async fn streaming_open_retry_exhaustion_returns_structured_error() {
    let llm = Arc::new(
        FakeStreamingLlmClient::builder()
            .push_llm_error(LlmError::Transport {
                message: "connection reset".to_string(),
                retryable: true,
            })
            .push_llm_error(LlmError::Transport {
                message: "connection reset again".to_string(),
                retryable: true,
            })
            .build(),
    );

    let executor = Arc::new(FakeToolExecutor::new(vec![]));
    let loop_inst = AgentLoop::new(AgentLoopConfig {
        llm: llm.clone(),
        tool_executor: executor,
    });
    let mut request = make_request("hello", vec![read_file_tool_definition()]);
    request.llm_retry = LlmRetryConfig {
        max_retries: 1,
        initial_delay_ms: 1,
        max_delay_ms: 1,
    };

    let result = loop_inst.run_turn(request).await;

    assert_eq!(result.status, AgentTurnStatus::Error);
    assert_eq!(llm.call_count(), 2);
    assert!(result
        .terminal_reason
        .as_deref()
        .unwrap_or_default()
        .contains("transport error: connection reset again"));

    let will_retry: Vec<bool> = result
        .events
        .iter()
        .filter_map(|event| match event {
            AgentTurnEvent::LlmRequestError { will_retry, .. } => Some(*will_retry),
            _ => None,
        })
        .collect();
    assert_eq!(will_retry, vec![true, false]);
}

#[tokio::test]
async fn streaming_open_does_not_retry_non_retryable_provider_http_error() {
    let llm = Arc::new(
        FakeStreamingLlmClient::builder()
            .push_llm_error(LlmError::ProviderHttp {
                status_code: 400,
                message: "bad request".to_string(),
            })
            .build(),
    );

    let executor = Arc::new(FakeToolExecutor::new(vec![]));
    let loop_inst = AgentLoop::new(AgentLoopConfig {
        llm: llm.clone(),
        tool_executor: executor,
    });
    let mut request = make_request("hello", vec![read_file_tool_definition()]);
    request.llm_retry = LlmRetryConfig {
        max_retries: 3,
        initial_delay_ms: 1,
        max_delay_ms: 1,
    };

    let result = loop_inst.run_turn(request).await;

    assert_eq!(result.status, AgentTurnStatus::Error);
    assert_eq!(llm.call_count(), 1);
    assert!(result.events.iter().any(|event| matches!(
        event,
        AgentTurnEvent::LlmRequestError {
            error_kind,
            retryable: false,
            will_retry: false,
            status_code: Some(400),
            ..
        } if error_kind == "provider_http"
    )));
}
