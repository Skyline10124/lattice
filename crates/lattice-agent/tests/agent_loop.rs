use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use lattice_agent::{
    AgentLoop, AgentLoopConfig, AgentTurnEvent, AgentTurnRequest, AgentTurnStatus, LlmClient,
    ToolCallExecutor,
};
use lattice_session::{GraphMessage, HeadTranscript};

struct FakeLlm {
    responses: std::sync::Mutex<Vec<Result<lattice_llm::ChatResponse, String>>>,
    call_count: AtomicUsize,
}

impl FakeLlm {
    fn new(responses: Vec<Result<lattice_llm::ChatResponse, String>>) -> Self {
        Self {
            responses: std::sync::Mutex::new(responses),
            call_count: AtomicUsize::new(0),
        }
    }
}

#[async_trait::async_trait]
impl LlmClient for FakeLlm {
    async fn chat(
        &self,
        _request: lattice_llm::ChatRequest,
    ) -> Result<lattice_llm::ChatResponse, Box<dyn std::error::Error + Send + Sync>> {
        self.call_count.fetch_add(1, Ordering::SeqCst);
        let mut responses = self.responses.lock().unwrap();
        if responses.is_empty() {
            return Err("no more fake responses".into());
        }
        let resp = responses.remove(0);
        resp.map_err(|e| e.into())
    }
}

struct FakeToolExecutor {
    results: std::sync::Mutex<Vec<String>>,
}

impl FakeToolExecutor {
    fn new(results: Vec<String>) -> Self {
        Self {
            results: std::sync::Mutex::new(results),
        }
    }
}

#[async_trait::async_trait]
impl ToolCallExecutor for FakeToolExecutor {
    async fn execute(&self, _call: &lattice_llm::ToolCall) -> Result<String, String> {
        let mut results = self.results.lock().unwrap();
        if results.is_empty() {
            return Err("no more fake results".to_string());
        }
        Ok(results.remove(0))
    }
}

fn make_request(user_input: &str, enabled_tools: Vec<String>) -> AgentTurnRequest {
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
        enabled_tools,
        project_root: "/tmp".to_string(),
        cancel_flag: None,
        timeout: None,
    }
}

fn make_request_with_transcript(
    user_input: &str,
    transcript_messages: Vec<GraphMessage>,
    enabled_tools: Vec<String>,
) -> AgentTurnRequest {
    AgentTurnRequest {
        session_id: "test-session".to_string(),
        transcript: HeadTranscript {
            session_id: "test-session".to_string(),
            parent_commit_id: Some("parent-1".to_string()),
            messages: transcript_messages,
        },
        user_input: user_input.to_string(),
        system_prompt: None,
        model: "test-model".to_string(),
        provider: None,
        enabled_tools,
        project_root: "/tmp".to_string(),
        cancel_flag: None,
        timeout: None,
    }
}

#[tokio::test]
async fn agent_loop_builds_messages_from_transcript_and_user_input() {
    let transcript_messages = vec![
        GraphMessage {
            role: "user".to_string(),
            content: "previous question".to_string(),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: None,
            name: None,
        },
        GraphMessage {
            role: "assistant".to_string(),
            content: "previous answer".to_string(),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: None,
            name: None,
        },
    ];

    let llm = Arc::new(FakeLlm::new(vec![Ok(lattice_llm::ChatResponse {
        content: "final answer".to_string(),
        reasoning_content: None,
        tool_calls: None,
        finish_reason: "stop".to_string(),
        model: "test-model".to_string(),
        usage: None,
    })]));

    let request_capture: Arc<std::sync::Mutex<Option<lattice_llm::ChatRequest>>> =
        Arc::new(std::sync::Mutex::new(None));
    let capture_clone = request_capture.clone();

    struct CapturingLlm {
        inner: Arc<FakeLlm>,
        captured: Arc<std::sync::Mutex<Option<lattice_llm::ChatRequest>>>,
    }

    #[async_trait::async_trait]
    impl LlmClient for CapturingLlm {
        async fn chat(
            &self,
            request: lattice_llm::ChatRequest,
        ) -> Result<lattice_llm::ChatResponse, Box<dyn std::error::Error + Send + Sync>> {
            *self.captured.lock().unwrap() = Some(request.clone());
            self.inner.chat(request).await
        }
    }

    let capturing_llm = Arc::new(CapturingLlm {
        inner: llm,
        captured: capture_clone,
    });

    let executor = Arc::new(FakeToolExecutor::new(vec![]));
    let loop_inst = AgentLoop::new(AgentLoopConfig {
        llm: capturing_llm,
        tool_executor: executor,
    });

    let request = make_request_with_transcript(
        "current question",
        transcript_messages,
        vec!["read_file".to_string()],
    );

    let result = loop_inst.run_turn(request).await;
    assert_eq!(result.status, AgentTurnStatus::Completed);

    let captured = request_capture.lock().unwrap().take().unwrap();
    assert_eq!(captured.messages.len(), 3);
    assert_eq!(captured.messages[0].content, "previous question");
    assert_eq!(captured.messages[0].role, lattice_llm::Role::User);
    assert_eq!(captured.messages[1].content, "previous answer");
    assert_eq!(captured.messages[1].role, lattice_llm::Role::Assistant);
    assert_eq!(captured.messages[2].content, "current question");
    assert_eq!(captured.messages[2].role, lattice_llm::Role::User);
}

#[tokio::test]
async fn agent_loop_prepends_system_prompt_when_present() {
    let request_capture: Arc<std::sync::Mutex<Option<lattice_llm::ChatRequest>>> =
        Arc::new(std::sync::Mutex::new(None));
    let capture_clone = request_capture.clone();

    struct CapturingLlm {
        captured: Arc<std::sync::Mutex<Option<lattice_llm::ChatRequest>>>,
    }

    #[async_trait::async_trait]
    impl LlmClient for CapturingLlm {
        async fn chat(
            &self,
            request: lattice_llm::ChatRequest,
        ) -> Result<lattice_llm::ChatResponse, Box<dyn std::error::Error + Send + Sync>> {
            *self.captured.lock().unwrap() = Some(request.clone());
            Ok(lattice_llm::ChatResponse {
                content: "ok".to_string(),
                reasoning_content: None,
                tool_calls: None,
                finish_reason: "stop".to_string(),
                model: "test".to_string(),
                usage: None,
            })
        }
    }

    let llm = Arc::new(CapturingLlm {
        captured: capture_clone,
    });
    let executor = Arc::new(FakeToolExecutor::new(vec![]));
    let loop_inst = AgentLoop::new(AgentLoopConfig {
        llm,
        tool_executor: executor,
    });

    let mut request = make_request("hello", vec!["read_file".to_string()]);
    request.system_prompt = Some("You are a helpful assistant.".to_string());

    let result = loop_inst.run_turn(request).await;
    assert_eq!(result.status, AgentTurnStatus::Completed);

    let captured = request_capture.lock().unwrap().take().unwrap();
    assert_eq!(captured.messages.len(), 2);
    assert_eq!(captured.messages[0].role, lattice_llm::Role::System);
    assert_eq!(captured.messages[0].content, "You are a helpful assistant.");
    assert_eq!(captured.messages[1].role, lattice_llm::Role::User);
    assert_eq!(captured.messages[1].content, "hello");
}

#[tokio::test]
async fn agent_loop_unknown_enabled_tools_fails_before_llm_call() {
    let llm = Arc::new(FakeLlm::new(vec![]));
    let executor = Arc::new(FakeToolExecutor::new(vec![]));
    let loop_inst = AgentLoop::new(AgentLoopConfig {
        llm,
        tool_executor: executor,
    });

    let request = make_request(
        "hello",
        vec!["read_file".to_string(), "nonexistent_tool".to_string()],
    );

    let result = loop_inst.run_turn(request).await;
    assert_eq!(result.status, AgentTurnStatus::Error);
    assert!(result
        .terminal_reason
        .as_ref()
        .unwrap()
        .contains("unknown enabled tools"));
    assert!(result
        .terminal_reason
        .as_ref()
        .unwrap()
        .contains("nonexistent_tool"));
    assert!(result.messages.is_empty());
    assert_eq!(result.assistant_output, "");
}

#[tokio::test]
async fn agent_loop_executes_one_tool_then_final_answer() {
    let llm = Arc::new(FakeLlm::new(vec![
        Ok(lattice_llm::ChatResponse {
            content: String::new(),
            reasoning_content: None,
            tool_calls: Some(vec![lattice_llm::ToolCall {
                id: "call_1".to_string(),
                function: lattice_llm::FunctionCall {
                    name: "read_file".to_string(),
                    arguments: r#"{"path": "test.txt"}"#.to_string(),
                },
            }]),
            finish_reason: "tool_calls".to_string(),
            model: "test-model".to_string(),
            usage: Some(lattice_llm::TokenUsage {
                prompt_tokens: 10,
                completion_tokens: 5,
                total_tokens: 15,
            }),
        }),
        Ok(lattice_llm::ChatResponse {
            content: "Here is the file content.".to_string(),
            reasoning_content: None,
            tool_calls: None,
            finish_reason: "stop".to_string(),
            model: "test-model".to_string(),
            usage: Some(lattice_llm::TokenUsage {
                prompt_tokens: 20,
                completion_tokens: 10,
                total_tokens: 30,
            }),
        }),
    ]));

    let executor = Arc::new(FakeToolExecutor::new(vec!["file content here".to_string()]));
    let loop_inst = AgentLoop::new(AgentLoopConfig {
        llm,
        tool_executor: executor,
    });

    let request = make_request("read test.txt", vec!["read_file".to_string()]);
    let result = loop_inst.run_turn(request).await;

    assert_eq!(result.status, AgentTurnStatus::Completed);
    assert_eq!(result.assistant_output, "Here is the file content.");
    assert_eq!(result.usage.prompt_tokens, 30);
    assert_eq!(result.usage.completion_tokens, 15);
    assert_eq!(result.usage.total_tokens, 45);

    assert_eq!(result.tool_results.len(), 1);
    assert_eq!(result.tool_results[0].name, "read_file");
    assert!(result.tool_results[0].ok);
    assert_eq!(result.messages.len(), 3);
    assert_eq!(result.messages[0].role, "assistant");
    assert_eq!(result.messages[1].role, "tool");
    assert_eq!(result.messages[1].tool_call_id, Some("call_1".to_string()));
    assert_eq!(result.messages[2].role, "assistant");
    assert_eq!(result.messages[2].content, "Here is the file content.");
}

#[tokio::test]
async fn agent_loop_executes_tool_calls_sequentially() {
    let llm = Arc::new(FakeLlm::new(vec![
        Ok(lattice_llm::ChatResponse {
            content: String::new(),
            reasoning_content: None,
            tool_calls: Some(vec![
                lattice_llm::ToolCall {
                    id: "call_a".to_string(),
                    function: lattice_llm::FunctionCall {
                        name: "read_file".to_string(),
                        arguments: r#"{"path": "a.txt"}"#.to_string(),
                    },
                },
                lattice_llm::ToolCall {
                    id: "call_b".to_string(),
                    function: lattice_llm::FunctionCall {
                        name: "read_file".to_string(),
                        arguments: r#"{"path": "b.txt"}"#.to_string(),
                    },
                },
            ]),
            finish_reason: "tool_calls".to_string(),
            model: "test-model".to_string(),
            usage: None,
        }),
        Ok(lattice_llm::ChatResponse {
            content: "Done reading both files.".to_string(),
            reasoning_content: None,
            tool_calls: None,
            finish_reason: "stop".to_string(),
            model: "test-model".to_string(),
            usage: None,
        }),
    ]));

    let execution_order: Arc<std::sync::Mutex<Vec<String>>> =
        Arc::new(std::sync::Mutex::new(Vec::new()));
    let order_clone = execution_order.clone();

    struct OrderTrackingExecutor {
        order: Arc<std::sync::Mutex<Vec<String>>>,
    }

    #[async_trait::async_trait]
    impl ToolCallExecutor for OrderTrackingExecutor {
        async fn execute(&self, call: &lattice_llm::ToolCall) -> Result<String, String> {
            self.order.lock().unwrap().push(call.id.clone());
            Ok(format!("content of {}", call.function.name))
        }
    }

    let executor = Arc::new(OrderTrackingExecutor { order: order_clone });
    let loop_inst = AgentLoop::new(AgentLoopConfig {
        llm,
        tool_executor: executor,
    });

    let request = make_request("read both", vec!["read_file".to_string()]);
    let result = loop_inst.run_turn(request).await;

    assert_eq!(result.status, AgentTurnStatus::Completed);
    let order = execution_order.lock().unwrap();
    assert_eq!(*order, vec!["call_a", "call_b"]);
    assert_eq!(result.messages.len(), 4);
    assert_eq!(result.messages[0].role, "assistant");
    assert_eq!(result.messages[1].role, "tool");
    assert_eq!(result.messages[1].tool_call_id, Some("call_a".to_string()));
    assert_eq!(result.messages[2].role, "tool");
    assert_eq!(result.messages[2].tool_call_id, Some("call_b".to_string()));
    assert_eq!(result.messages[3].role, "assistant");
}

#[tokio::test]
async fn agent_loop_persists_full_tool_text_in_messages() {
    let large_output = "x".repeat(10_000);
    let large_output_clone = large_output.clone();

    let llm = Arc::new(FakeLlm::new(vec![
        Ok(lattice_llm::ChatResponse {
            content: String::new(),
            reasoning_content: None,
            tool_calls: Some(vec![lattice_llm::ToolCall {
                id: "call_1".to_string(),
                function: lattice_llm::FunctionCall {
                    name: "read_file".to_string(),
                    arguments: "{}".to_string(),
                },
            }]),
            finish_reason: "tool_calls".to_string(),
            model: "test-model".to_string(),
            usage: None,
        }),
        Ok(lattice_llm::ChatResponse {
            content: "ok".to_string(),
            reasoning_content: None,
            tool_calls: None,
            finish_reason: "stop".to_string(),
            model: "test-model".to_string(),
            usage: None,
        }),
    ]));

    let executor = Arc::new(FakeToolExecutor::new(vec![large_output_clone]));
    let loop_inst = AgentLoop::new(AgentLoopConfig {
        llm,
        tool_executor: executor,
    });

    let request = make_request("read file", vec!["read_file".to_string()]);
    let result = loop_inst.run_turn(request).await;

    assert_eq!(result.status, AgentTurnStatus::Completed);
    let tool_msg = result
        .messages
        .iter()
        .find(|m| m.role == "tool")
        .expect("should have tool message");
    assert_eq!(tool_msg.content, large_output);
    assert_eq!(tool_msg.tool_call_id, Some("call_1".to_string()));
    assert_eq!(tool_msg.name, Some("read_file".to_string()));
}

#[tokio::test]
async fn agent_loop_accumulates_usage() {
    let llm = Arc::new(FakeLlm::new(vec![
        Ok(lattice_llm::ChatResponse {
            content: String::new(),
            reasoning_content: None,
            tool_calls: Some(vec![lattice_llm::ToolCall {
                id: "call_1".to_string(),
                function: lattice_llm::FunctionCall {
                    name: "read_file".to_string(),
                    arguments: "{}".to_string(),
                },
            }]),
            finish_reason: "tool_calls".to_string(),
            model: "test-model".to_string(),
            usage: Some(lattice_llm::TokenUsage {
                prompt_tokens: 100,
                completion_tokens: 20,
                total_tokens: 120,
            }),
        }),
        Ok(lattice_llm::ChatResponse {
            content: String::new(),
            reasoning_content: None,
            tool_calls: Some(vec![lattice_llm::ToolCall {
                id: "call_2".to_string(),
                function: lattice_llm::FunctionCall {
                    name: "read_file".to_string(),
                    arguments: "{}".to_string(),
                },
            }]),
            finish_reason: "tool_calls".to_string(),
            model: "test-model".to_string(),
            usage: Some(lattice_llm::TokenUsage {
                prompt_tokens: 200,
                completion_tokens: 30,
                total_tokens: 230,
            }),
        }),
        Ok(lattice_llm::ChatResponse {
            content: "done".to_string(),
            reasoning_content: None,
            tool_calls: None,
            finish_reason: "stop".to_string(),
            model: "test-model".to_string(),
            usage: Some(lattice_llm::TokenUsage {
                prompt_tokens: 300,
                completion_tokens: 40,
                total_tokens: 340,
            }),
        }),
    ]));

    let executor = Arc::new(FakeToolExecutor::new(vec![
        "result1".to_string(),
        "result2".to_string(),
    ]));
    let loop_inst = AgentLoop::new(AgentLoopConfig {
        llm,
        tool_executor: executor,
    });

    let request = make_request("read files", vec!["read_file".to_string()]);
    let result = loop_inst.run_turn(request).await;

    assert_eq!(result.status, AgentTurnStatus::Completed);
    assert_eq!(result.usage.prompt_tokens, 600);
    assert_eq!(result.usage.completion_tokens, 90);
    assert_eq!(result.usage.total_tokens, 690);
}

#[tokio::test]
async fn agent_loop_timeout_returns_failed_status() {
    struct SlowLlm;

    #[async_trait::async_trait]
    impl LlmClient for SlowLlm {
        async fn chat(
            &self,
            _request: lattice_llm::ChatRequest,
        ) -> Result<lattice_llm::ChatResponse, Box<dyn std::error::Error + Send + Sync>> {
            tokio::time::sleep(Duration::from_secs(10)).await;
            Ok(lattice_llm::ChatResponse {
                content: "should not reach here".to_string(),
                reasoning_content: None,
                tool_calls: None,
                finish_reason: "stop".to_string(),
                model: "test".to_string(),
                usage: None,
            })
        }
    }

    let llm = Arc::new(SlowLlm);
    let executor = Arc::new(FakeToolExecutor::new(vec![]));
    let loop_inst = AgentLoop::new(AgentLoopConfig {
        llm,
        tool_executor: executor,
    });

    let mut request = make_request("hello", vec!["read_file".to_string()]);
    request.timeout = Some(Duration::from_millis(50));

    let started = std::time::Instant::now();
    let result = loop_inst.run_turn(request).await;

    assert_eq!(result.status, AgentTurnStatus::Error);
    assert!(result.terminal_reason.as_ref().unwrap().contains("timeout"));
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "timeout should return promptly"
    );
    assert!(result.messages.is_empty());
    // Cooperative deadline (Phase 17): ModelStarted is emitted before
    // stream_chat is called, so events are preserved on timeout.
    assert!(
        result
            .events
            .iter()
            .any(|e| matches!(e, AgentTurnEvent::ModelStarted { .. })),
        "timeout should preserve ModelStarted emitted before stream_chat"
    );
}

#[tokio::test]
async fn agent_loop_cancel_returns_cancelled_status() {
    let cancel_flag = Arc::new(AtomicBool::new(false));
    let cancel_clone = cancel_flag.clone();

    struct CancelAwareLlm {
        cancel: Arc<AtomicBool>,
        call_count: AtomicUsize,
    }

    #[async_trait::async_trait]
    impl LlmClient for CancelAwareLlm {
        async fn chat(
            &self,
            _request: lattice_llm::ChatRequest,
        ) -> Result<lattice_llm::ChatResponse, Box<dyn std::error::Error + Send + Sync>> {
            let count = self.call_count.fetch_add(1, Ordering::SeqCst);
            if count > 0 {
                self.cancel.store(true, Ordering::Relaxed);
            }
            Ok(lattice_llm::ChatResponse {
                content: String::new(),
                reasoning_content: None,
                tool_calls: Some(vec![lattice_llm::ToolCall {
                    id: format!("call_{count}"),
                    function: lattice_llm::FunctionCall {
                        name: "read_file".to_string(),
                        arguments: "{}".to_string(),
                    },
                }]),
                finish_reason: "tool_calls".to_string(),
                model: "test".to_string(),
                usage: None,
            })
        }
    }

    let llm = Arc::new(CancelAwareLlm {
        cancel: cancel_clone,
        call_count: AtomicUsize::new(0),
    });
    let executor = Arc::new(FakeToolExecutor::new(vec![
        "result1".to_string(),
        "result2".to_string(),
    ]));
    let loop_inst = AgentLoop::new(AgentLoopConfig {
        llm,
        tool_executor: executor,
    });

    let mut request = make_request("hello", vec!["read_file".to_string()]);
    request.cancel_flag = Some(cancel_flag);

    let result = loop_inst.run_turn(request).await;

    assert_eq!(result.status, AgentTurnStatus::Cancelled);
    assert!(result
        .terminal_reason
        .as_ref()
        .unwrap()
        .contains("cancelled"));
    assert!(!result.messages.is_empty());
}

#[tokio::test]
async fn agent_loop_emits_tool_activity_events() {
    let llm = Arc::new(FakeLlm::new(vec![
        Ok(lattice_llm::ChatResponse {
            content: String::new(),
            reasoning_content: None,
            tool_calls: Some(vec![lattice_llm::ToolCall {
                id: "call_1".to_string(),
                function: lattice_llm::FunctionCall {
                    name: "read_file".to_string(),
                    arguments: "{}".to_string(),
                },
            }]),
            finish_reason: "tool_calls".to_string(),
            model: "test-model".to_string(),
            usage: None,
        }),
        Ok(lattice_llm::ChatResponse {
            content: "final answer".to_string(),
            reasoning_content: None,
            tool_calls: None,
            finish_reason: "stop".to_string(),
            model: "test-model".to_string(),
            usage: None,
        }),
    ]));

    let executor = Arc::new(FakeToolExecutor::new(vec!["file content".to_string()]));
    let loop_inst = AgentLoop::new(AgentLoopConfig {
        llm,
        tool_executor: executor,
    });

    let request = make_request("read file", vec!["read_file".to_string()]);
    let result = loop_inst.run_turn(request).await;

    assert_eq!(result.status, AgentTurnStatus::Completed);

    let events = &result.events;

    let has_model_started = events
        .iter()
        .any(|e| matches!(e, AgentTurnEvent::ModelStarted { model, .. } if model == "test-model"));
    assert!(
        has_model_started,
        "should emit ModelStarted before each chat() call"
    );

    let has_tool_start = events
        .iter()
        .any(|e| matches!(e, AgentTurnEvent::ToolCallStarted { id, .. } if id == "call_1"));
    assert!(has_tool_start, "should have ToolCallStarted event");

    let has_tool_finished = events.iter().any(|e| {
        matches!(
            e,
            AgentTurnEvent::ToolCallFinished {
                id,
                name,
                ok: true,
                ..
            } if id == "call_1" && name == "read_file"
        )
    });
    assert!(has_tool_finished, "should have ToolCallFinished event");

    let has_model_finished = events.iter().any(|e| {
        matches!(
            e,
            AgentTurnEvent::ModelFinished { finish_reason, .. } if finish_reason == "stop"
        )
    });
    assert!(
        has_model_finished,
        "should have ModelFinished event on successful completion"
    );

    let has_token = events
        .iter()
        .any(|e| matches!(e, AgentTurnEvent::TokenDelta { content } if content == "final answer"));
    assert!(has_token, "should have TokenDelta event with final answer");
}

#[tokio::test]
async fn agent_loop_llm_error_returns_failed_status() {
    let llm = Arc::new(FakeLlm::new(vec![Err("provider unavailable".to_string())]));
    let executor = Arc::new(FakeToolExecutor::new(vec![]));
    let loop_inst = AgentLoop::new(AgentLoopConfig {
        llm,
        tool_executor: executor,
    });

    let request = make_request("hello", vec!["read_file".to_string()]);
    let result = loop_inst.run_turn(request).await;

    assert_eq!(result.status, AgentTurnStatus::Error);
    assert!(result
        .terminal_reason
        .as_ref()
        .unwrap()
        .contains("LLM error"));
    assert!(result
        .terminal_reason
        .as_ref()
        .unwrap()
        .contains("provider unavailable"));
}

/// Fake LLM that emits a tool call response on every call, forever.
/// Used to verify the agent loop's safety cap (MAX_TURN_ROUNDS).
struct InfiniteToolCallLlm {
    call_count: AtomicUsize,
}

#[async_trait::async_trait]
impl LlmClient for InfiniteToolCallLlm {
    async fn chat(
        &self,
        _request: lattice_llm::ChatRequest,
    ) -> Result<lattice_llm::ChatResponse, Box<dyn std::error::Error + Send + Sync>> {
        let n = self.call_count.fetch_add(1, Ordering::SeqCst);
        Ok(lattice_llm::ChatResponse {
            content: String::new(),
            reasoning_content: None,
            tool_calls: Some(vec![lattice_llm::ToolCall {
                id: format!("call_{n}"),
                function: lattice_llm::FunctionCall {
                    name: "read_file".to_string(),
                    arguments: r#"{"path": "x.txt"}"#.to_string(),
                },
            }]),
            finish_reason: "tool_calls".to_string(),
            model: "test-model".to_string(),
            usage: None,
        })
    }
}

struct InfiniteToolExecutor;

#[async_trait::async_trait]
impl ToolCallExecutor for InfiniteToolExecutor {
    async fn execute(&self, _call: &lattice_llm::ToolCall) -> Result<String, String> {
        Ok("ok".to_string())
    }
}

#[tokio::test]
async fn agent_loop_terminates_at_hard_cap_when_llm_loops_forever() {
    let llm = Arc::new(InfiniteToolCallLlm {
        call_count: AtomicUsize::new(0),
    });
    let executor = Arc::new(InfiniteToolExecutor);
    let loop_inst = AgentLoop::new(AgentLoopConfig {
        llm: llm.clone(),
        tool_executor: executor,
    });

    let request = make_request("loop forever", vec!["read_file".to_string()]);
    let result = loop_inst.run_turn(request).await;

    assert_eq!(result.status, AgentTurnStatus::Error);
    let reason = result.terminal_reason.expect("should have terminal reason");
    assert!(
        reason.contains("max rounds"),
        "expected max-rounds reason, got: {reason}"
    );
    // LLM call count should equal MAX_TURN_ROUNDS (each round makes exactly one LLM call).
    assert_eq!(llm.call_count.load(Ordering::SeqCst), 200);
}
