// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 既有进程内双 edit 验收：真实 Component、文件会话库与私有工作区历史。
//! 不是外部二进制 E2E 或发布批准。
//! Filter: `release_e2e_criterion_8`

mod support;

use std::sync::Arc;

use lancet_api::replay_audit;
use lattice_agent::LlmClient;
use lattice_agent::testing::FakeStreamingLlmClient;
use lattice_api::{RunTurnRequest, SessionApi};
use lattice_conversation::{MessageSemantic, ToolResultOutcome};
use lattice_llm::{LlmEvent, TokenUsage};
use lattice_profile::{DiscoveryPaths, LoadedAgentProfile, ProfileSource};
use lattice_session::{StateLayout, store::*};

// ── Streaming event helpers ──────────────────────────────────────────────────

fn usage_tokens() -> TokenUsage {
    TokenUsage {
        prompt_tokens: 10,
        completion_tokens: 5,
        total_tokens: 15,
    }
}

fn tool_call_events(id: &str, name: &str, args: &str) -> Vec<LlmEvent> {
    vec![
        LlmEvent::ToolCallStart {
            index: 0,
            provider_call_id: Some(id.to_string()),
            name: name.to_string(),
        },
        LlmEvent::ToolCallDelta {
            index: 0,
            arguments_delta: args.to_string(),
        },
        LlmEvent::ToolCallEnd { index: 0 },
        LlmEvent::Done {
            finish_reason: "tool_calls".to_string(),
            usage: Some(usage_tokens()),
        },
    ]
}

fn final_response_events(text: &str) -> Vec<LlmEvent> {
    vec![
        LlmEvent::Token {
            content: text.to_string(),
        },
        LlmEvent::Done {
            finish_reason: "stop".to_string(),
            usage: Some(usage_tokens()),
        },
    ]
}

// ── Workspace setup ──────────────────────────────────────────────────────────

struct ReleaseE2eWorkspace {
    _dir: tempfile::TempDir,
    /// Holds the shared env lock and points `XDG_STATE_HOME` at a temp state
    /// root for the whole fixture lifetime, restoring the previous value on drop.
    _state: lattice_session::ScopedXdgStateHome,
    _global_agents: tempfile::TempDir,
    root: std::path::PathBuf,
    canonical_root: std::path::PathBuf,
    loaded: LoadedAgentProfile,
}

/// Build a temp workspace with a src/lib.rs seed and a custom profile.
///
/// The profile enables `mutate.edit` with an empty workspace deny list
/// (except `.git/**`) so edits apply cleanly.
fn setup_release_e2e_workspace(src_content: &str) -> ReleaseE2eWorkspace {
    let state = lattice_session::ScopedXdgStateHome::new();
    let ws = tempfile::tempdir().unwrap();
    let root = ws.path().to_path_buf();

    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/lib.rs"), src_content).unwrap();
    support::init_git_repo(&root);

    let profile_dir = root.join(".lattice/agents/default");
    std::fs::create_dir_all(&profile_dir).unwrap();

    let toml_content = r#"schema_version = 1
[agent]
name = "default"
model = "sonnet"
[prompt]
system = "You are a test agent."
[tools]
enabled = ["mutate.edit"]
[lancet.workspace]
denied_write_patterns = [".git/**"]
"#;
    std::fs::write(profile_dir.join("agent.toml"), toml_content).unwrap();

    let global_agents = tempfile::tempdir().unwrap();
    let paths = DiscoveryPaths {
        project_root: root.clone(),
        global_agents_dir: global_agents.path().to_path_buf(),
    };
    let loaded = lattice_profile::resolve_agent_profile(ProfileSource::Default, &paths)
        .expect("profile must load");

    let canonical_root = std::fs::canonicalize(&root).unwrap_or_else(|_| root.clone());

    ReleaseE2eWorkspace {
        _dir: ws,
        _state: state,
        _global_agents: global_agents,
        root,
        canonical_root,
        loaded,
    }
}

fn make_release_e2e_request(
    ws: &ReleaseE2eWorkspace,
    input: &str,
    llm: Arc<dyn LlmClient>,
) -> RunTurnRequest {
    // No `set_current_dir` here: `test_project_root` already pins the root, and
    // mutating the process-global CWD from concurrent tests made sibling tests
    // fail once a `TempDir` dropped (see the note in `tests/consistency.rs`).
    RunTurnRequest {
        input: input.to_string(),
        session_id: None,
        agent: Some("default".into()),
        loaded_profile: Some(ws.loaded.clone()),
        cancel_flag: None,
        test_llm: Some((llm, lattice_llm::ProviderKind::OpenAiCompatible)),
        test_project_root: Some(ws.root.to_string_lossy().to_string()),
    }
}

// ── Release criterion-8 test ──────────────────────────────────────────────────

// 同一个 Turn 完成两次 edit 与最终回答，重开后按调用顺序核验持久成功终态。

#[tokio::test]
async fn release_e2e_criterion_8() {
    let initial_content = "pub fn answer() -> i32 { 1 }";
    let bad_content = "pub fn answer() -> i32 { 2 }"; // No profile verify configured, edit succeeds
    let good_content = "pub fn answer() -> i32 { 42 } // RELEASE_E2E_OK"; // verify passes

    let ws = setup_release_e2e_workspace(initial_content);

    // ── Compute blake3 hashes for the two edit steps ───────────────────
    let hash1 = format!(
        "blake3:{}",
        blake3::hash(initial_content.as_bytes()).to_hex()
    );

    // Edit 1: apply bad content (no profile verify configured).
    let bad_edit_args = serde_json::json!({
        "path": "src/lib.rs",
        "expected_hash": hash1,
        "edits": [{
            "op": "replace_text",
            "old": initial_content,
            "new": bad_content
        }]
    });

    // Edit 2: refresh expected_hash after edit 1 mutates Local content.
    let hash2 = format!("blake3:{}", blake3::hash(bad_content.as_bytes()).to_hex());
    let good_edit_args = serde_json::json!({
        "path": "src/lib.rs",
        "expected_hash": hash2,
        "edits": [{
            "op": "replace_text",
            "old": bad_content,
            "new": good_content
        }]
    });

    // ── Build FakeStreamingLlmClient with 3 rounds ─────
    let fake_llm: Arc<dyn LlmClient> = Arc::new(
        FakeStreamingLlmClient::builder()
            // Round 1: mutate.edit (intermediate content)
            .push_events(tool_call_events(
                "call_1",
                "mutate.edit",
                &bad_edit_args.to_string(),
            ))
            // Round 2: mutate.edit (final content with sentinel)
            .push_events(tool_call_events(
                "call_2",
                "mutate.edit",
                &good_edit_args.to_string(),
            ))
            // Round 3: final assistant response after both edits
            .push_events(final_response_events(
                "Criterion-8 verified: edit(fail) → edit(ok).",
            ))
            .build(),
    );

    let layout = StateLayout::resolve().unwrap();
    let store = Arc::new(SessionStore::create(&layout).unwrap());
    let result = lattice_api::run_turn(
        SessionApi::from_store(store.clone()),
        layout.clone(),
        make_release_e2e_request(&ws, "Run release criterion-8 sequence", fake_llm),
    )
    .await
    .expect("run_turn must succeed");
    assert_eq!(result.summary.outcome, PublicTurnOutcome::Completed);
    assert_eq!(
        std::fs::read_to_string(ws.root.join("src/lib.rs")).unwrap(),
        good_content
    );
    // 关闭并严格重开，操作员读取不能依赖尚存的内存 receipt。
    drop(store);
    let store = SessionStore::open(&layout).unwrap();
    let read = store.begin_read().unwrap();
    let log = read.log(result.turn.session_id).unwrap();
    assert_eq!(log.nodes.len(), 5); // Root、User、三个完整 Step。
    assert_eq!(
        log.nodes
            .iter()
            .filter(|node| node.turn_start.is_some())
            .count(),
        1
    );
    let project = lattice_session::ProjectStoreKey::derive(&ws.canonical_root).unwrap();
    let replay =
        replay_audit(&read, &project, result.turn.session_id, result.turn.turn_id).unwrap();
    assert_eq!(replay.calls.len(), 2);
    for (ordinal, row) in replay.calls.iter().enumerate() {
        assert_eq!(row.binding.turn_id, result.turn.turn_id);
        assert_eq!(row.binding.model_step_ordinal, ordinal as u32);
        assert_eq!(row.binding.method, "mutate.edit");
        assert!(matches!(
            row.state,
            OperatorInvocationState::Terminal {
                outcome: ToolResultOutcome::Succeeded,
                ..
            }
        ));
    }
    let Node::Step { content, .. } = read
        .node(NodeRef {
            session_id: result.turn.session_id,
            node: log.session.current,
        })
        .unwrap()
    else {
        panic!("final Step")
    };
    assert!(content.assistant.parts.iter().any(|part| matches!(&part.semantic, MessageSemantic::Text { text } if text.contains("Criterion-8 verified"))));
    assert_eq!(
        read.turn_detail(result.turn.session_id, result.turn.turn_id)
            .unwrap()
            .steps,
        result.steps
    );
}
