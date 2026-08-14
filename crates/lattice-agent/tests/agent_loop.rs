// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use lattice_agent::{
    AgentLoop, AgentLoopConfig, AgentTurnRequest, AgentTurnStatus, CepErrorPayload, LlmClient,
    LlmRetryConfig, ToolCallExecutor, ToolExecutionError,
};
use lattice_conversation::HeadTranscript;
use lattice_llm::ToolDefinition;

fn read_file_tool_definition() -> ToolDefinition {
    ToolDefinition::new(
        "observe.workspace",
        "Observe workspace content",
        serde_json::json!({"type": "object", "properties": {"path": {"type": "string"}}}),
    )
}

struct FakeLlm {
    responses: std::sync::Mutex<Vec<Result<lattice_llm::ChatResponse, String>>>,
}

impl FakeLlm {
    fn new(responses: Vec<Result<lattice_llm::ChatResponse, String>>) -> Self {
        Self {
            responses: std::sync::Mutex::new(responses),
        }
    }
}

#[async_trait::async_trait]
impl LlmClient for FakeLlm {
    async fn chat(
        &self,
        _request: lattice_llm::ChatRequest,
    ) -> Result<lattice_llm::ChatResponse, Box<dyn std::error::Error + Send + Sync>> {
        let mut responses = self.responses.lock().unwrap();
        if responses.is_empty() {
            return Err("no more fake responses".into());
        }
        let resp = responses.remove(0);
        resp.map_err(|e| e.into())
    }
}

struct FakeToolExecutor {
    results: std::sync::Mutex<Vec<Result<String, ToolExecutionError>>>,
}

impl FakeToolExecutor {
    fn new(results: Vec<Result<String, ToolExecutionError>>) -> Self {
        Self {
            results: std::sync::Mutex::new(results),
        }
    }
}

#[async_trait::async_trait]
impl ToolCallExecutor for FakeToolExecutor {
    async fn execute(&self, _call: &lattice_llm::ToolCall) -> Result<String, ToolExecutionError> {
        let mut results = self.results.lock().unwrap();
        if results.is_empty() {
            return Err(ToolExecutionError::simple("no more fake results"));
        }
        results.remove(0)
    }
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

    let mut request = make_request("hello", vec![read_file_tool_definition()]);
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
                    name: "observe.workspace".to_string(),
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
    async fn execute(&self, _call: &lattice_llm::ToolCall) -> Result<String, ToolExecutionError> {
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

    let request = make_request("loop forever", vec![read_file_tool_definition()]);
    let result = loop_inst.run_turn(request).await;

    assert_eq!(result.status, AgentTurnStatus::Error);
    let reason = result.terminal_reason.expect("should have terminal reason");
    assert!(
        reason.contains("max rounds"),
        "expected max-rounds reason, got: {reason}"
    );
    assert_eq!(llm.call_count.load(Ordering::SeqCst), 200);
}

#[tokio::test]
async fn agent_loop_second_llm_round_includes_tool_result_in_request() {
    struct CapturingMultiRoundLlm {
        responses: std::sync::Mutex<Vec<lattice_llm::ChatResponse>>,
        captured: std::sync::Mutex<Vec<lattice_llm::ChatRequest>>,
    }

    #[async_trait::async_trait]
    impl LlmClient for CapturingMultiRoundLlm {
        async fn chat(
            &self,
            request: lattice_llm::ChatRequest,
        ) -> Result<lattice_llm::ChatResponse, Box<dyn std::error::Error + Send + Sync>> {
            self.captured.lock().unwrap().push(request);
            let mut responses = self.responses.lock().unwrap();
            if responses.is_empty() {
                return Err("no more fake responses".into());
            }
            Ok(responses.remove(0))
        }
    }

    let llm = Arc::new(CapturingMultiRoundLlm {
        responses: std::sync::Mutex::new(vec![
            lattice_llm::ChatResponse {
                content: String::new(),
                reasoning_content: None,
                tool_calls: Some(vec![lattice_llm::ToolCall {
                    id: "call_1".to_string(),
                    function: lattice_llm::FunctionCall {
                        name: "observe.workspace".to_string(),
                        arguments: r#"{"path":"a.txt"}"#.to_string(),
                    },
                }]),
                finish_reason: "tool_calls".to_string(),
                model: "test-model".to_string(),
                usage: None,
            },
            lattice_llm::ChatResponse {
                content: "final answer".to_string(),
                reasoning_content: None,
                tool_calls: None,
                finish_reason: "stop".to_string(),
                model: "test-model".to_string(),
                usage: None,
            },
        ]),
        captured: std::sync::Mutex::new(Vec::new()),
    });

    let executor = Arc::new(FakeToolExecutor::new(vec![Ok("tool body".to_string())]));
    let loop_inst = AgentLoop::new(AgentLoopConfig {
        llm: llm.clone(),
        tool_executor: executor,
    });

    let result = loop_inst
        .run_turn(make_request("hello", vec![read_file_tool_definition()]))
        .await;

    assert_eq!(result.status, AgentTurnStatus::Completed);
    assert_eq!(result.assistant_output, "final answer");

    let captured = llm.captured.lock().unwrap();
    assert_eq!(
        captured.len(),
        2,
        "expected two LLM rounds (tool call then final answer)"
    );

    let second = &captured[1];
    assert_eq!(
        second.messages.len(),
        3,
        "user + assistant tool-call + tool result"
    );
    assert_eq!(second.messages[0].role, lattice_llm::Role::User);
    assert_eq!(second.messages[0].content, "hello");
    assert_eq!(second.messages[1].role, lattice_llm::Role::Assistant);
    assert!(
        second.messages[1].tool_calls.is_some(),
        "second-round request must include assistant tool-call message"
    );
    assert_eq!(second.messages[2].role, lattice_llm::Role::Tool);
    assert_eq!(second.messages[2].content, "tool body");
    assert_eq!(
        second.messages[2].tool_call_id.as_deref(),
        Some("call_1"),
        "second-round request must include prior tool result"
    );
}

#[tokio::test]
async fn tool_execution_error_structured_payload_preserved() {
    let cep_error = CepErrorPayload {
        code: -32042,
        message: "path outside workspace".to_string(),
        data: serde_json::json!({"path": "../secret"}),
        retriability: "Permanent".to_string(),
    };

    let llm = Arc::new(FakeLlm::new(vec![
        Ok(lattice_llm::ChatResponse {
            content: String::new(),
            reasoning_content: None,
            tool_calls: Some(vec![lattice_llm::ToolCall {
                id: "call_err".to_string(),
                function: lattice_llm::FunctionCall {
                    name: "observe.workspace".to_string(),
                    arguments: r#"{"path":"../secret"}"#.to_string(),
                },
            }]),
            finish_reason: "tool_calls".to_string(),
            model: "test-model".to_string(),
            usage: None,
        }),
        Ok(lattice_llm::ChatResponse {
            content: "handled error".to_string(),
            reasoning_content: None,
            tool_calls: None,
            finish_reason: "stop".to_string(),
            model: "test-model".to_string(),
            usage: None,
        }),
    ]));

    let executor = Arc::new(FakeToolExecutor::new(vec![Err(ToolExecutionError {
        message: "path outside workspace".to_string(),
        cep_error: Some(cep_error.clone()),
    })]));
    let loop_inst = AgentLoop::new(AgentLoopConfig {
        llm,
        tool_executor: executor,
    });

    let result = loop_inst
        .run_turn(make_request(
            "read secret",
            vec![read_file_tool_definition()],
        ))
        .await;

    assert_eq!(result.status, AgentTurnStatus::Completed);
    assert_eq!(result.tool_results.len(), 1);
    let summary = &result.tool_results[0];
    assert!(!summary.ok);
    assert_eq!(
        summary.error_summary.as_deref(),
        Some("path outside workspace")
    );
    assert_eq!(summary.cep_error.as_ref(), Some(&cep_error));

    let finished = result
        .events
        .iter()
        .find_map(|event| match event {
            lattice_agent::AgentTurnEvent::ToolCallFinished {
                id,
                name,
                ok,
                error_summary,
                ..
            } if id == "call_err" => Some((name.clone(), *ok, error_summary.clone())),
            _ => None,
        })
        .expect("ToolCallFinished event for failed tool");
    assert_eq!(finished.0, "observe.workspace");
    assert!(!finished.1);
    assert_eq!(finished.2.as_deref(), Some("path outside workspace"));
}
