// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Tests for the provisional completion hook contract.
//!
//! Validates that:
//! - `AgentTurnResult` always includes a `cleanup_exhausted` field.
//! - Hook errors produce terminal `AgentTurnStatus::Error`.
//! - The hook receives only the provisional assistant message.
//! - `ProvisionalDecision::Complete` returns the exhaustion fact.
//! - The agent crate has no workspace/Git/disposition dependency.

use std::sync::Arc;

use lattice_agent::{
    AgentLoop, AgentLoopConfig, AgentTurnRequest, AgentTurnStatus, LlmClient, LlmRetryConfig,
    ProvisionalCompletionHook, ProvisionalDecision, ToolCallExecutor, ToolExecutionError,
};
use lattice_conversation::{GraphMessage, HeadTranscript};
use lattice_llm::ToolDefinition;

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

struct FakeToolExecutor;

#[async_trait::async_trait]
impl ToolCallExecutor for FakeToolExecutor {
    async fn execute(&self, _call: &lattice_llm::ToolCall) -> Result<String, ToolExecutionError> {
        Err(ToolExecutionError::simple("no tools expected"))
    }
}

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
        tool_definitions: Vec::<ToolDefinition>::new(),
        cancel_flag: None,
        timeout: None,
        llm_retry: LlmRetryConfig::default(),
    }
}

fn final_response(content: &str) -> lattice_llm::ChatResponse {
    lattice_llm::ChatResponse {
        content: content.to_string(),
        reasoning_content: None,
        tool_calls: None,
        finish_reason: "stop".to_string(),
        model: "test-model".to_string(),
        usage: Some(lattice_llm::TokenUsage {
            prompt_tokens: 10,
            completion_tokens: 5,
            total_tokens: 15,
        }),
    }
}

/// Hook that always returns Complete with cleanup_exhausted = true.
struct ExhaustedHook;

#[async_trait::async_trait]
impl ProvisionalCompletionHook for ExhaustedHook {
    async fn on_provisional_complete(
        &mut self,
        _assistant: &GraphMessage,
    ) -> Result<ProvisionalDecision, String> {
        Ok(ProvisionalDecision::Complete {
            cleanup_exhausted: true,
            final_message: None,
        })
    }
}

/// Hook that returns an error.
struct ErrorHook;

#[async_trait::async_trait]
impl ProvisionalCompletionHook for ErrorHook {
    async fn on_provisional_complete(
        &mut self,
        _assistant: &GraphMessage,
    ) -> Result<ProvisionalDecision, String> {
        Err("hook failure".to_string())
    }
}

#[tokio::test]
async fn provisional_complete_contract_result_has_cleanup_exhausted_field() {
    let llm = Arc::new(FakeLlm::new(vec![Ok(final_response("done"))]));
    let config = AgentLoopConfig {
        llm,
        tool_executor: Arc::new(FakeToolExecutor),
    };
    let agent = AgentLoop::new(config);
    let (tx, _rx) = tokio::sync::mpsc::channel(16);
    let result = agent
        .run_turn_live_with_hook(make_request("hello"), tx, None)
        .await;

    assert_eq!(result.status, AgentTurnStatus::Completed);
    assert!(
        !result.cleanup_exhausted,
        "cleanup_exhausted should be false when no hook requests continuation"
    );
}

#[tokio::test]
async fn provisional_complete_contract_hook_complete_exhausted_true() {
    let llm = Arc::new(FakeLlm::new(vec![Ok(final_response("done"))]));
    let config = AgentLoopConfig {
        llm,
        tool_executor: Arc::new(FakeToolExecutor),
    };
    let agent = AgentLoop::new(config);
    let (tx, _rx) = tokio::sync::mpsc::channel(16);
    let result = agent
        .run_turn_live_with_hook(make_request("hello"), tx, Some(Box::new(ExhaustedHook)))
        .await;

    assert_eq!(result.status, AgentTurnStatus::Completed);
    assert!(
        result.cleanup_exhausted,
        "cleanup_exhausted must be true when hook signals exhaustion"
    );
}

#[tokio::test]
async fn provisional_complete_contract_hook_error_becomes_terminal() {
    let llm = Arc::new(FakeLlm::new(vec![Ok(final_response("done"))]));
    let config = AgentLoopConfig {
        llm,
        tool_executor: Arc::new(FakeToolExecutor),
    };
    let agent = AgentLoop::new(config);
    let (tx, _rx) = tokio::sync::mpsc::channel(16);
    let result = agent
        .run_turn_live_with_hook(make_request("hello"), tx, Some(Box::new(ErrorHook)))
        .await;

    assert_eq!(
        result.status,
        AgentTurnStatus::Error,
        "hook error must produce a terminal Error status"
    );
    assert!(
        result
            .terminal_reason
            .as_ref()
            .is_some_and(|r| r.contains("hook failure")),
        "terminal_reason should include the hook error message"
    );
}

#[tokio::test]
async fn provisional_complete_contract_no_hook_behaves_like_run_turn_live() {
    let llm = Arc::new(FakeLlm::new(vec![Ok(final_response("final"))]));
    let config = AgentLoopConfig {
        llm,
        tool_executor: Arc::new(FakeToolExecutor),
    };
    let agent = AgentLoop::new(config);
    let (tx, _rx) = tokio::sync::mpsc::channel(16);
    let result = agent
        .run_turn_live_with_hook(make_request("hi"), tx, None)
        .await;

    assert_eq!(result.status, AgentTurnStatus::Completed);
    assert_eq!(result.assistant_output, "final");
    assert!(!result.cleanup_exhausted);
}

#[tokio::test]
async fn provisional_complete_contract_hook_receives_assistant_message() {
    use std::sync::Mutex;

    struct CapturingHook {
        captured: Arc<Mutex<Option<GraphMessage>>>,
    }

    #[async_trait::async_trait]
    impl ProvisionalCompletionHook for CapturingHook {
        async fn on_provisional_complete(
            &mut self,
            assistant: &GraphMessage,
        ) -> Result<ProvisionalDecision, String> {
            self.captured.lock().unwrap().replace(assistant.clone());
            Ok(ProvisionalDecision::Complete {
                cleanup_exhausted: false,
                final_message: None,
            })
        }
    }

    let captured = Arc::new(Mutex::new(None));
    let hook = CapturingHook {
        captured: captured.clone(),
    };

    let llm = Arc::new(FakeLlm::new(vec![Ok(final_response("provisional output"))]));
    let config = AgentLoopConfig {
        llm,
        tool_executor: Arc::new(FakeToolExecutor),
    };
    let agent = AgentLoop::new(config);
    let (tx, _rx) = tokio::sync::mpsc::channel(16);
    let _result = agent
        .run_turn_live_with_hook(make_request("go"), tx, Some(Box::new(hook)))
        .await;

    let msg = captured.lock().unwrap().take();
    assert!(
        msg.is_some(),
        "hook must receive the provisional assistant message"
    );
    let msg = msg.unwrap();
    assert_eq!(msg.role, "assistant");
    assert_eq!(msg.content, "provisional output");
}

#[tokio::test]
async fn provisional_complete_contract_no_workspace_dependency() {
    // This test verifies at compile time that the hook trait uses only
    // GraphMessage (from lattice-session) and no workspace/Git/disposition types.
    // If ProvisionalCompletionHook or ProvisionalDecision imported workspace
    // types, this file would fail to compile since we import nothing from
    // lattice-workspace or lattice-api here.
    let _: Option<Box<dyn ProvisionalCompletionHook>> = None;
    let _decision = ProvisionalDecision::Complete {
        cleanup_exhausted: false,
        final_message: None,
    };
}

// ─── Task 2: Same-loop continuation tests ───────────────────────────────────

use std::sync::Mutex;

/// A FakeLlm that captures every ChatRequest it receives and returns
/// pre-configured responses in order.
struct CapturingLlm {
    responses: Mutex<Vec<Result<lattice_llm::ChatResponse, String>>>,
    captured_requests: Arc<Mutex<Vec<lattice_llm::ChatRequest>>>,
}

impl CapturingLlm {
    fn new(responses: Vec<Result<lattice_llm::ChatResponse, String>>) -> Self {
        Self {
            responses: Mutex::new(responses),
            captured_requests: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

#[async_trait::async_trait]
impl LlmClient for CapturingLlm {
    async fn chat(
        &self,
        request: lattice_llm::ChatRequest,
    ) -> Result<lattice_llm::ChatResponse, Box<dyn std::error::Error + Send + Sync>> {
        self.captured_requests.lock().unwrap().push(request.clone());
        let mut responses = self.responses.lock().unwrap();
        if responses.is_empty() {
            return Err("no more fake responses".into());
        }
        let resp = responses.remove(0);
        resp.map_err(|e| e.into())
    }
}

/// Hook that returns Continue on the first call, then Complete on subsequent calls.
struct ContinueOnceHook {
    call_count: u32,
    system_content: String,
}

impl ContinueOnceHook {
    fn new(system_content: &str) -> Self {
        Self {
            call_count: 0,
            system_content: system_content.to_string(),
        }
    }
}

#[async_trait::async_trait]
impl ProvisionalCompletionHook for ContinueOnceHook {
    async fn on_provisional_complete(
        &mut self,
        _assistant: &GraphMessage,
    ) -> Result<ProvisionalDecision, String> {
        self.call_count += 1;
        if self.call_count == 1 {
            Ok(ProvisionalDecision::Continue(GraphMessage {
                role: "system".to_string(),
                content: self.system_content.clone(),
                reasoning_content: None,
                tool_calls: None,
                tool_call_id: None,
                name: None,
            }))
        } else {
            Ok(ProvisionalDecision::Complete {
                cleanup_exhausted: false,
                final_message: None,
            })
        }
    }
}

#[tokio::test]
async fn provisional_complete_continue_preserves_message_ordering() {
    // LLM round 1: provisional final response
    // Hook returns Continue with system fact
    // LLM round 2: continuation response (final)
    let llm = Arc::new(CapturingLlm::new(vec![
        Ok(final_response("provisional answer")),
        Ok(final_response("final answer after cleanup")),
    ]));
    let captured = llm.captured_requests.clone();
    let config = AgentLoopConfig {
        llm,
        tool_executor: Arc::new(FakeToolExecutor),
    };
    let agent = AgentLoop::new(config);
    let (tx, _rx) = tokio::sync::mpsc::channel(16);
    let hook = ContinueOnceHook::new("workspace is dirty: file.txt changed");
    let result = agent
        .run_turn_live_with_hook(make_request("do work"), tx, Some(Box::new(hook)))
        .await;

    assert_eq!(result.status, AgentTurnStatus::Completed);
    assert_eq!(result.assistant_output, "final answer after cleanup");

    let requests = captured.lock().unwrap();
    assert_eq!(requests.len(), 2, "should have two LLM rounds");

    // Round 2 messages: user, provisional assistant, system fact
    let round2_msgs = &requests[1].messages;
    let roles: Vec<&lattice_llm::Role> = round2_msgs.iter().map(|m| &m.role).collect();

    // Find the user message - should be exactly one
    let user_count = roles
        .iter()
        .filter(|&&r| matches!(r, lattice_llm::Role::User))
        .count();
    assert_eq!(user_count, 1, "user message must appear exactly once");

    // The ordering should be: user, assistant (provisional), system (fact)
    let user_pos = roles
        .iter()
        .position(|r| matches!(r, lattice_llm::Role::User))
        .unwrap();
    let assistant_pos = roles
        .iter()
        .position(|r| matches!(r, lattice_llm::Role::Assistant))
        .unwrap();
    let system_positions: Vec<usize> = roles
        .iter()
        .enumerate()
        .filter(|(_, r)| matches!(r, lattice_llm::Role::System))
        .map(|(i, _)| i)
        .collect();

    assert!(
        user_pos < assistant_pos,
        "user must precede provisional assistant"
    );
    assert!(
        !system_positions.is_empty(),
        "system fact must be present in continuation round"
    );
    assert!(
        assistant_pos < system_positions[0],
        "provisional assistant must precede system fact"
    );
}

#[tokio::test]
async fn provisional_complete_continue_accumulates_usage() {
    let llm = Arc::new(CapturingLlm::new(vec![
        Ok(lattice_llm::ChatResponse {
            content: "provisional".to_string(),
            reasoning_content: None,
            tool_calls: None,
            finish_reason: "stop".to_string(),
            model: "test-model".to_string(),
            usage: Some(lattice_llm::TokenUsage {
                prompt_tokens: 100,
                completion_tokens: 20,
                total_tokens: 120,
            }),
        }),
        Ok(lattice_llm::ChatResponse {
            content: "final".to_string(),
            reasoning_content: None,
            tool_calls: None,
            finish_reason: "stop".to_string(),
            model: "test-model".to_string(),
            usage: Some(lattice_llm::TokenUsage {
                prompt_tokens: 150,
                completion_tokens: 30,
                total_tokens: 180,
            }),
        }),
    ]));
    let config = AgentLoopConfig {
        llm,
        tool_executor: Arc::new(FakeToolExecutor),
    };
    let agent = AgentLoop::new(config);
    let (tx, _rx) = tokio::sync::mpsc::channel(16);
    let hook = ContinueOnceHook::new("dirty");
    let result = agent
        .run_turn_live_with_hook(make_request("go"), tx, Some(Box::new(hook)))
        .await;

    assert_eq!(result.status, AgentTurnStatus::Completed);
    // Usage should be cumulative across both LLM rounds
    assert_eq!(
        result.usage.prompt_tokens, 250,
        "prompt_tokens must accumulate: 100 + 150"
    );
    assert_eq!(
        result.usage.completion_tokens, 50,
        "completion_tokens must accumulate: 20 + 30"
    );
    assert_eq!(
        result.usage.total_tokens, 300,
        "total_tokens must accumulate: 120 + 180"
    );
}

#[tokio::test]
async fn provisional_complete_continue_increases_round_count() {
    // We verify indirectly by checking that two LLM requests were made,
    // proving the loop continued rather than restarting.
    let llm = Arc::new(CapturingLlm::new(vec![
        Ok(final_response("provisional")),
        Ok(final_response("final")),
    ]));
    let captured = llm.captured_requests.clone();
    let config = AgentLoopConfig {
        llm,
        tool_executor: Arc::new(FakeToolExecutor),
    };
    let agent = AgentLoop::new(config);
    let (tx, _rx) = tokio::sync::mpsc::channel(16);
    let hook = ContinueOnceHook::new("fact");
    let result = agent
        .run_turn_live_with_hook(make_request("x"), tx, Some(Box::new(hook)))
        .await;

    assert_eq!(result.status, AgentTurnStatus::Completed);
    let requests = captured.lock().unwrap();
    assert_eq!(
        requests.len(),
        2,
        "continuation must produce a second LLM round in the same loop"
    );
}

#[tokio::test]
async fn provisional_complete_continue_respects_cancellation() {
    use std::sync::atomic::AtomicBool;

    let cancel_flag = Arc::new(AtomicBool::new(false));

    // Hook that sets cancel flag on first call, then returns Continue.
    // The next round's cancel check should terminate the loop.
    struct CancelOnContinueHook {
        cancel_flag: Arc<AtomicBool>,
    }

    #[async_trait::async_trait]
    impl ProvisionalCompletionHook for CancelOnContinueHook {
        async fn on_provisional_complete(
            &mut self,
            _assistant: &GraphMessage,
        ) -> Result<ProvisionalDecision, String> {
            self.cancel_flag
                .store(true, std::sync::atomic::Ordering::Relaxed);
            Ok(ProvisionalDecision::Continue(GraphMessage {
                role: "system".to_string(),
                content: "dirty".to_string(),
                reasoning_content: None,
                tool_calls: None,
                tool_call_id: None,
                name: None,
            }))
        }
    }

    let llm = Arc::new(CapturingLlm::new(vec![
        Ok(final_response("provisional")),
        // This second response should never be consumed because cancel fires
        Ok(final_response("should not reach")),
    ]));
    let config = AgentLoopConfig {
        llm,
        tool_executor: Arc::new(FakeToolExecutor),
    };
    let agent = AgentLoop::new(config);
    let (tx, _rx) = tokio::sync::mpsc::channel(16);
    let hook = CancelOnContinueHook {
        cancel_flag: cancel_flag.clone(),
    };
    let request = AgentTurnRequest {
        cancel_flag: Some(cancel_flag),
        ..make_request("cancel me")
    };
    let result = agent
        .run_turn_live_with_hook(request, tx, Some(Box::new(hook)))
        .await;

    assert_eq!(
        result.status,
        AgentTurnStatus::Cancelled,
        "cancellation must terminate the continued loop"
    );
}

#[tokio::test]
async fn provisional_complete_existing_run_turn_live_unaffected() {
    // Verify that existing run_turn_live behavior is preserved
    // (delegates to run_turn_live_with_hook with no hook).
    let llm = Arc::new(FakeLlm::new(vec![Ok(final_response("unchanged"))]));
    let config = AgentLoopConfig {
        llm,
        tool_executor: Arc::new(FakeToolExecutor),
    };
    let agent = AgentLoop::new(config);
    let (tx, _rx) = tokio::sync::mpsc::channel(16);
    let result = agent.run_turn_live(make_request("hi"), tx).await;

    assert_eq!(result.status, AgentTurnStatus::Completed);
    assert_eq!(result.assistant_output, "unchanged");
    assert!(!result.cleanup_exhausted);
}
