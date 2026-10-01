// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 使用真实文件会话库与 Component，保护调用结果、续接上下文和失败 usage。
mod support;

use lattice_agent::LlmClient;
use lattice_agent::testing::FakeStreamingLlmClient;
use lattice_api::{RunTurnRequest, SessionApi};
use lattice_llm::{LlmEvent, TokenUsage};
use lattice_profile::{DiscoveryPaths, LoadedAgentProfile, ProfileSource};
use lattice_session::{StateLayout, store::*};
use std::sync::Arc;

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
