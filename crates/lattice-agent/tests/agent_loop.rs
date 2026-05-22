use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use lattice_agent::{
    AgentLoop, AgentLoopConfig, AgentTurnRequest, AgentTurnStatus, LlmClient, ToolCallExecutor,
};
use lattice_session::{GraphMessage, HeadTranscript};

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
    assert_eq!(llm.call_count.load(Ordering::SeqCst), 200);
}
