//! Deterministic usage tests for the v1.1 main chain path.
//!
//! These tests exercise the real `run_turn` → `RealTurnDeps` → `AgentLoop` →
//! `DefaultToolExecutor` pipeline with a fake LLM. No network, no real API keys.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use lattice_agent::{AgentLoop, AgentLoopConfig, LlmClient};
use lattice_api::{RunTurnRequest, RunTurnStatus};
use lattice_llm::{ChatRequest, ChatResponse, FunctionCall, TokenUsage, ToolCall};
use lattice_profile::{DiscoveryPaths, LoadedAgentProfile, ProfileSource};
use lattice_tools::executor::DefaultToolExecutor;

// ── Fake LLM ────────────────────────────────────────────────────────────────

struct FakeLlm {
    responses: Mutex<VecDeque<ChatResponse>>,
    requests: Mutex<Vec<ChatRequest>>,
}

impl FakeLlm {
    fn new(responses: Vec<ChatResponse>) -> Arc<Self> {
        Arc::new(Self {
            responses: Mutex::new(VecDeque::from(responses)),
            requests: Mutex::new(Vec::new()),
        })
    }

    fn take_requests(&self) -> Vec<ChatRequest> {
        let mut guard = self.requests.lock().unwrap();
        std::mem::take(&mut *guard)
    }
}

#[async_trait::async_trait]
impl LlmClient for FakeLlm {
    async fn chat(
        &self,
        request: ChatRequest,
    ) -> Result<ChatResponse, Box<dyn std::error::Error + Send + Sync>> {
        self.requests.lock().unwrap().push(request);
        self.responses
            .lock()
            .unwrap()
            .pop_front()
            .ok_or_else(|| "FakeLlm: no more responses queued".into())
    }
}

// ── Tool executor adapter ───────────────────────────────────────────────────
//
// `DefaultToolExecutor` directly implements the unified `ToolExecutor` trait
// (re-exported as `ToolCallExecutor`), so it can be wrapped in an Arc and
// passed to `AgentLoopConfig.tool_executor` without an adapter layer.

// ── Helpers ─────────────────────────────────────────────────────────────────

fn tool_call_response(id: &str, name: &str, args: &str) -> ChatResponse {
    ChatResponse {
        content: String::new(),
        reasoning_content: None,
        tool_calls: Some(vec![ToolCall {
            id: id.to_string(),
            function: FunctionCall {
                name: name.to_string(),
                arguments: args.to_string(),
            },
        }]),
        finish_reason: "tool_calls".to_string(),
        model: "fake-model".to_string(),
        usage: Some(TokenUsage {
            prompt_tokens: 10,
            completion_tokens: 5,
            total_tokens: 15,
        }),
    }
}

fn final_response(text: &str) -> ChatResponse {
    ChatResponse {
        content: text.to_string(),
        reasoning_content: None,
        tool_calls: None,
        finish_reason: "stop".to_string(),
        model: "fake-model".to_string(),
        usage: Some(TokenUsage {
            prompt_tokens: 10,
            completion_tokens: 5,
            total_tokens: 15,
        }),
    }
}

/// Temp workspace with src/lib.rs and a default agent profile.
struct Workspace {
    _dir: tempfile::TempDir,
    _global_agents: tempfile::TempDir,
    root: std::path::PathBuf,
    loaded: LoadedAgentProfile,
}

fn setup_workspace(src_content: &str, enabled_tools: &[&str]) -> Workspace {
    let ws = tempfile::tempdir().unwrap();
    let root = ws.path().to_path_buf();

    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/lib.rs"), src_content).unwrap();

    let profile_dir = root.join(".lattice/agents/default");
    std::fs::create_dir_all(&profile_dir).unwrap();
    let enabled: Vec<String> = enabled_tools.iter().map(|s| s.to_string()).collect();
    let toml_content = format!(
        r#"schema_version = 1
[agent]
name = "default"
model = "sonnet"
[prompt]
system = "You are a test agent."
[tools]
enabled = {enabled:?}
"#
    );
    std::fs::write(profile_dir.join("agent.toml"), toml_content).unwrap();

    let global_agents = tempfile::tempdir().unwrap();
    let paths = DiscoveryPaths {
        project_root: root.clone(),
        global_agents_dir: global_agents.path().to_path_buf(),
    };
    let loaded = lattice_profile::resolve_agent_profile(ProfileSource::Default, &paths)
        .expect("profile must load");

    Workspace {
        _dir: ws,
        _global_agents: global_agents,
        root,
        loaded,
    }
}

fn build_agent_loop(
    fake_llm: Arc<FakeLlm>,
    workspace_root: &std::path::Path,
    enabled_tools: &[&str],
) -> Arc<AgentLoop> {
    let enabled: Vec<String> = enabled_tools.iter().map(|s| s.to_string()).collect();
    let executor =
        DefaultToolExecutor::for_project_root(&workspace_root.to_string_lossy(), enabled)
            .expect("executor creation must succeed");

    Arc::new(AgentLoop::new(AgentLoopConfig {
        llm: fake_llm,
        tool_executor: Arc::new(executor),
    }))
}

// ── Test 1: read workspace file through default profile ─────────────────────

#[tokio::test]
async fn usage_run_turn_can_read_workspace_file_with_default_profile() {
    let ws = setup_workspace("pub fn answer() -> i32 { 42 }", &["read_file"]);

    let fake_llm = FakeLlm::new(vec![
        tool_call_response("call_1", "read_file", r#"{"path":"src/lib.rs"}"#),
        final_response("Read workspace file successfully."),
    ]);
    let agent_loop = build_agent_loop(fake_llm.clone(), &ws.root, &["read_file"]);

    let session_dir = tempfile::tempdir().unwrap();
    let sessions_path = session_dir.path().join("sessions");

    let request = RunTurnRequest {
        input: "Read src/lib.rs".to_string(),
        session_id: None,
        profile: None,
        model: None,
        loaded_profile: Some(ws.loaded),
    };

    let result = lattice_api::run_turn(
        lattice_api::SessionApi::sqlite(&sessions_path),
        agent_loop,
        request,
    )
    .await
    .expect("run_turn must succeed");

    // Turn completed
    assert_eq!(result.status, RunTurnStatus::Completed);
    assert!(
        result
            .assistant_output
            .contains("Read workspace file successfully"),
        "assistant output: {}",
        result.assistant_output
    );

    // Tool result contains the file content
    let tool_result = result
        .tool_results
        .iter()
        .find(|t| t.name == "read_file")
        .expect("should have a read_file tool result");
    assert!(tool_result.output_len > 0, "tool output must be non-empty");

    // Verify the actual tool output was captured by checking the agent messages
    // contain "pub fn answer" — this proves the tool read from workspace root,
    // not from .lattice/agents/default
    let requests = fake_llm.take_requests();
    assert!(
        requests.len() >= 2,
        "LLM should have been called at least twice"
    );
    let second_req = &requests[1];
    let has_file_content = second_req
        .messages
        .iter()
        .any(|m| m.content.contains("pub fn answer"));
    assert!(
        has_file_content,
        "tool result in LLM messages must contain 'pub fn answer'"
    );

    // Session log has exactly one commit
    let verify_api = lattice_api::SessionApi::sqlite(&sessions_path);
    let log = verify_api
        .log(&result.session_id)
        .expect("log must succeed");
    assert_eq!(log.len(), 1, "session log should have exactly one commit");
}

// ── Test 2: patch workspace file and commit ─────────────────────────────────

#[tokio::test]
async fn usage_run_turn_can_patch_workspace_file_and_commit() {
    let ws = setup_workspace("pub fn answer() -> i32 { 1 }", &["read_file", "patch"]);

    let original = "pub fn answer() -> i32 { 1 }";
    let hash = format!("blake3:{}", blake3::hash(original.as_bytes()).to_hex());

    let patch_doc = format!(
        "\
*** Begin Patch
*** Update File: src/lib.rs
*** Old Hash: {hash}
@@
-pub fn answer() -> i32 {{ 1 }}
+pub fn answer() -> i32 {{ 42 }}
*** End Patch
"
    );

    let fake_llm = FakeLlm::new(vec![
        tool_call_response(
            "call_1",
            "patch",
            &format!(
                r#"{{"patch":{}}}"#,
                serde_json::to_string(&patch_doc).unwrap()
            ),
        ),
        final_response("Patched successfully."),
    ]);
    let agent_loop = build_agent_loop(fake_llm, &ws.root, &["read_file", "patch"]);

    let session_dir = tempfile::tempdir().unwrap();
    let sessions_path = session_dir.path().join("sessions");

    let request = RunTurnRequest {
        input: "Change the answer to 42".to_string(),
        session_id: None,
        profile: None,
        model: None,
        loaded_profile: Some(ws.loaded),
    };

    let result = lattice_api::run_turn(
        lattice_api::SessionApi::sqlite(&sessions_path),
        agent_loop,
        request,
    )
    .await
    .expect("run_turn must succeed");

    // File on disk changed to 42
    let patched = std::fs::read_to_string(ws.root.join("src/lib.rs")).unwrap();
    assert!(
        patched.contains("42"),
        "file should contain 42, got: {patched}"
    );
    assert!(!patched.contains("{ 1 }"), "old value should be gone");

    assert_eq!(result.status, RunTurnStatus::Completed);

    let verify_api = lattice_api::SessionApi::sqlite(&sessions_path);
    let log = verify_api
        .log(&result.session_id)
        .expect("log must succeed");
    assert_eq!(log.len(), 1);

    assert!(
        result.tool_results.iter().any(|t| t.name == "patch"),
        "should have a patch tool result"
    );
}

// ── Test 3: resume session includes previous transcript ─────────────────────

#[tokio::test]
async fn usage_resume_session_includes_previous_turn_context() {
    let ws = setup_workspace("pub fn answer() -> i32 { 42 }", &["read_file"]);

    let fake_llm = FakeLlm::new(vec![
        final_response("First turn done."),
        final_response("Second turn done."),
    ]);
    let agent_loop = build_agent_loop(fake_llm.clone(), &ws.root, &["read_file"]);

    let session_dir = tempfile::tempdir().unwrap();
    let sessions_path = session_dir.path().join("sessions");

    // Turn 1
    let request1 = RunTurnRequest {
        input: "Hello".to_string(),
        session_id: None,
        profile: None,
        model: None,
        loaded_profile: Some(ws.loaded.clone()),
    };
    let result1 = lattice_api::run_turn(
        lattice_api::SessionApi::sqlite(&sessions_path),
        agent_loop.clone(),
        request1,
    )
    .await
    .expect("turn 1 must succeed");
    assert_eq!(result1.status, RunTurnStatus::Completed);
    let session_id = result1.session_id.clone();

    // Turn 2 (same session — new SessionApi pointing at same SQLite dir)
    let request2 = RunTurnRequest {
        input: "Follow up".to_string(),
        session_id: Some(session_id.clone()),
        profile: None,
        model: None,
        loaded_profile: Some(ws.loaded),
    };
    let result2 = lattice_api::run_turn(
        lattice_api::SessionApi::sqlite(&sessions_path),
        agent_loop,
        request2,
    )
    .await
    .expect("turn 2 must succeed");
    assert_eq!(result2.status, RunTurnStatus::Completed);

    // Second LLM request must include previous user/assistant messages
    let requests = fake_llm.take_requests();
    assert_eq!(requests.len(), 2, "LLM should have been called twice");

    let second_req_messages = &requests[1].messages;
    let has_prior_user = second_req_messages
        .iter()
        .any(|m| m.role == lattice_llm::Role::User && m.content == "Hello");
    let has_prior_assistant = second_req_messages
        .iter()
        .any(|m| m.role == lattice_llm::Role::Assistant && m.content == "First turn done.");
    assert!(
        has_prior_user,
        "second LLM request must contain first turn's user message"
    );
    assert!(
        has_prior_assistant,
        "second LLM request must contain first turn's assistant response"
    );

    // Session log has 2 commits; second parent is first commit
    let verify_api = lattice_api::SessionApi::sqlite(&sessions_path);
    let log = verify_api.log(&session_id).expect("log must succeed");
    assert_eq!(log.len(), 2, "session log should have 2 commits");

    let second = &log[1];
    assert!(
        second.parent_ids.contains(&result1.commit_id),
        "second commit parent should include first commit"
    );

    // Status confirms 2 commits on main branch
    let status = verify_api.status(&session_id).expect("status must succeed");
    assert_eq!(status.commit_count, 2, "status should report 2 commits");
    assert!(
        matches!(status.head, lattice_session::GraphHead::Branch { name } if name == "main"),
        "HEAD should be on main branch"
    );
}
