// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 使用真实文件会话库与 Component，保护调用结果、续接上下文和失败 usage。
mod support;

use lattice_agent::LlmClient;
use lattice_agent::testing::FakeStreamingLlmClient;
use lattice_api::{RunTurnRequest, SessionApi};
use lattice_conversation::{ConversationItem, MessageSemantic};
use lattice_llm::{LlmEvent, TokenUsage};
use lattice_profile::{DiscoveryPaths, LoadedAgentProfile, ProfileSource};
use lattice_session::{StateLayout, store::*};
use std::sync::{Arc, Mutex};

fn usage_tokens() -> TokenUsage {
    TokenUsage {
        prompt_tokens: 10,
        completion_tokens: 5,
        total_tokens: 15,
    }
}

fn tool_call_events(name: &str, args: serde_json::Value) -> Vec<LlmEvent> {
    vec![
        LlmEvent::ToolCallStart {
            index: 0,
            provider_call_id: Some("call_1".into()),
            name: name.into(),
        },
        LlmEvent::ToolCallDelta {
            index: 0,
            arguments_delta: args.to_string(),
        },
        LlmEvent::ToolCallEnd { index: 0 },
        LlmEvent::Done {
            finish_reason: "tool_calls".into(),
            usage: Some(usage_tokens()),
        },
    ]
}

fn final_response_events(text: &str) -> Vec<LlmEvent> {
    vec![
        LlmEvent::Token {
            content: text.into(),
        },
        LlmEvent::Done {
            finish_reason: "stop".into(),
            usage: Some(usage_tokens()),
        },
    ]
}

struct Workspace {
    _dir: tempfile::TempDir,
    _state: tempfile::TempDir,
    root: std::path::PathBuf,
    loaded: LoadedAgentProfile,
    layout: StateLayout,
    store: Arc<SessionStore>,
}

fn setup_workspace(src_content: &str, enabled_tools: &[&str]) -> Workspace {
    let ws = tempfile::tempdir().unwrap();
    let root = ws.path().to_path_buf();
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/lib.rs"), src_content).unwrap();
    support::init_git_repo(&root);
    let profile_dir = root.join(".lattice/agents/default");
    std::fs::create_dir_all(&profile_dir).unwrap();
    std::fs::write(
        profile_dir.join("agent.toml"),
        format!(
            r#"schema_version = 1
[agent]
name = "default"
model = "sonnet"
[prompt]
system = "You are a test agent."
[tools]
enabled = {enabled_tools:?}
[lancet.workspace]
denied_write_patterns = [".git/**"]
"#
        ),
    )
    .unwrap();
    let global = tempfile::tempdir().unwrap();
    let loaded = lattice_profile::resolve_agent_profile(
        ProfileSource::Default,
        &DiscoveryPaths {
            project_root: root.clone(),
            global_agents_dir: global.path().to_path_buf(),
        },
    )
    .unwrap();
    let state = tempfile::tempdir().unwrap();
    let layout = StateLayout::in_state_root(state.path());
    let store = Arc::new(SessionStore::create(&layout).unwrap());
    Workspace {
        _dir: ws,
        _state: state,
        root,
        loaded,
        layout,
        store,
    }
}

fn make_request(
    ws: &Workspace,
    input: &str,
    session_id: Option<lancet_protocol::SessionId>,
    llm: Arc<dyn LlmClient>,
) -> RunTurnRequest {
    RunTurnRequest {
        input: input.into(),
        session_id,
        agent: Some("default".into()),
        loaded_profile: Some(ws.loaded.clone()),
        cancel_flag: None,
        test_llm: Some((llm, lattice_llm::ProviderKind::OpenAiCompatible)),
        test_project_root: Some(ws.root.to_string_lossy().into()),
    }
}

#[tokio::test]
async fn usage_run_turn_can_edit_workspace_file_and_commit() {
    let original = "pub fn answer() -> i32 { 1 }";
    let ws = setup_workspace(original, &["mutate.edit"]);
    let fake = Arc::new(FakeStreamingLlmClient::builder()
        .push_events(tool_call_events("mutate.edit", serde_json::json!({
            "path":"src/lib.rs", "expected_hash":format!("blake3:{}", blake3::hash(original.as_bytes()).to_hex()),
            "edits":[{"op":"replace_text", "old":original, "new":"pub fn answer() -> i32 { 42 }"}]
        })))
        .push_events(final_response_events("Edited successfully.")).build());
    let result = lattice_api::run_turn(
        SessionApi::from_store(ws.store.clone()),
        ws.layout.clone(),
        make_request(&ws, "Change the answer to 42", None, fake),
    )
    .await
    .unwrap();
    assert_eq!(result.summary.outcome, PublicTurnOutcome::Completed);
    assert_eq!(
        std::fs::read_to_string(ws.root.join("src/lib.rs")).unwrap(),
        "pub fn answer() -> i32 { 42 }"
    );
    let read = ws.store.begin_read().unwrap();
    let detail = read
        .turn_detail(result.turn.session_id, result.turn.turn_id)
        .unwrap();
    assert_eq!(detail.steps, result.steps);
    assert!(
        matches!(&detail.steps[0].state, StepDetailState::Closed { end: StepEnd::Continue { .. }, slots }
        if slots.len() == 1 && slots[0].binding.method == "mutate.edit" && matches!(slots[0].state, OperatorInvocationState::Terminal { outcome: lattice_conversation::ToolResultOutcome::Succeeded, .. }))
    );
    assert_eq!(read.log(result.turn.session_id).unwrap().nodes.len(), 4);
}

struct Capture {
    inner: FakeStreamingLlmClient,
    requests: Mutex<Vec<lattice_llm::ChatRequest>>,
}
#[async_trait::async_trait]
impl LlmClient for Capture {
    async fn stream_chat(
        &self,
        request: lattice_llm::ChatRequest,
    ) -> std::result::Result<lattice_llm::LlmStream, Box<dyn std::error::Error + Send + Sync>> {
        self.requests.lock().unwrap().push(request.clone());
        self.inner.stream_chat(request).await
    }
}

#[tokio::test]
async fn usage_resume_session_includes_previous_turn_context() {
    let ws = setup_workspace("pub fn answer() -> i32 { 42 }", &["observe.workspace"]);
    let fake = Arc::new(Capture {
        inner: FakeStreamingLlmClient::builder()
            .push_events(final_response_events("First turn done."))
            .push_events(final_response_events("Second turn done."))
            .push_events(final_response_events("Third turn done."))
            .build(),
        requests: Mutex::default(),
    });
    let api = SessionApi::from_store(ws.store.clone());
    let first = lattice_api::run_turn(
        api.clone(),
        ws.layout.clone(),
        make_request(&ws, "Hello", None, fake.clone()),
    )
    .await
    .unwrap();
    let second = lattice_api::run_turn(
        api.clone(),
        ws.layout.clone(),
        make_request(&ws, "Follow up", Some(first.turn.session_id), fake.clone()),
    )
    .await
    .unwrap();
    let third = lattice_api::run_turn(
        api,
        ws.layout.clone(),
        make_request(&ws, "Last input", Some(first.turn.session_id), fake.clone()),
    )
    .await
    .unwrap();
    assert_eq!(third.summary.outcome, PublicTurnOutcome::Completed);
    assert_eq!(first.summary.outcome, PublicTurnOutcome::Completed);
    assert_eq!(second.summary.outcome, PublicTurnOutcome::Completed);
    let read = ws.store.begin_read().unwrap();
    let detail = read
        .turn_detail(second.turn.session_id, second.turn.turn_id)
        .unwrap();
    assert!(matches!(
        read.node(NodeRef {
            session_id: second.turn.session_id,
            node: detail.user_node
        })
        .unwrap(),
        Node::UserMessage {
            parent: NodeId(2),
            ..
        }
    ));
    assert_eq!(read.log(second.turn.session_id).unwrap().nodes.len(), 7);
    let requests = fake.requests.lock().unwrap();
    assert_eq!(requests.len(), 3);
    let text =
        |message: &lattice_conversation::NativeMessage<lattice_conversation::BoundToolCall>| {
            let MessageSemantic::Text { text } = &message.parts[0].semantic else {
                panic!("text")
            };
            (message.role, text.clone())
        };
    use lattice_conversation::MessageRole::{Assistant, System, User};
    let supplied: Vec<_> = requests[2]
        .messages
        .iter()
        .map(|item| match item {
            ConversationItem::Message(message) => text(message),
            ConversationItem::Step(step) => text(&step.assistant),
        })
        .collect();
    assert_eq!(
        supplied,
        vec![
            (System, "You are a test agent.".into()),
            (User, "Hello".into()),
            (Assistant, "First turn done.".into()),
            (User, "Follow up".into()),
            (Assistant, "Second turn done.".into()),
            (User, "Last input".into()),
        ]
    );
    let mut saved = Vec::new();
    for n in 1..=6 {
        let node = read
            .node(NodeRef {
                session_id: first.turn.session_id,
                node: NodeId(n),
            })
            .unwrap();
        saved.push(match node {
            Node::UserMessage { message, .. } => text(&message),
            Node::Step { content, .. } => text(&content.assistant),
            Node::Root => panic!("content node"),
        });
    }
    assert_eq!(
        saved,
        vec![
            (User, "Hello".into()),
            (Assistant, "First turn done.".into()),
            (User, "Follow up".into()),
            (Assistant, "Second turn done.".into()),
            (User, "Last input".into()),
            (Assistant, "Third turn done.".into()),
        ]
    );
}

#[tokio::test]
async fn usage_rejected_out_of_catalog_tool_batch_keeps_the_attempt_usage() {
    let ws = setup_workspace("pub fn answer() -> i32 { 1 }", &["observe.workspace"]);
    let fake = Arc::new(
        FakeStreamingLlmClient::builder()
            .push_events(tool_call_events(
                "fetch.web",
                serde_json::json!({"url":"https://example.invalid"}),
            ))
            .build(),
    );
    let result = lattice_api::run_turn(
        SessionApi::from_store(ws.store.clone()),
        ws.layout.clone(),
        make_request(&ws, "fetch it", None, fake),
    )
    .await
    .unwrap();
    assert_eq!(result.summary.outcome, PublicTurnOutcome::Failed);
    let detail = ws
        .store
        .begin_read()
        .unwrap()
        .turn_detail(result.turn.session_id, result.turn.turn_id)
        .unwrap();
    assert!(matches!(
        detail.steps[0].state,
        StepDetailState::Closed {
            end: StepEnd::Stop {
                node: None,
                cause: StopCause::InvalidBatch
            },
            ..
        }
    ));
    assert_eq!(
        detail.usage,
        UsageSummary::Complete {
            total: usage_tokens()
        }
    );
}
