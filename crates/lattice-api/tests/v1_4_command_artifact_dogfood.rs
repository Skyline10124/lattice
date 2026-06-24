// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! v1.4-alpha no-key command/artifact metadata dogfood proof.
//!
//! This test is release-gated as offline dogfood evidence. It proves the
//! production API/CEP/audit path records command artifact metadata and fallback
//! honesty without provider keys or uncontrolled network access. It does not
//! claim live artifact isolation when the local host reports no overlay path.

mod support;

use std::sync::Arc;

use lancet_api::audit_db_path_for_project_in;
use lancet_authority::CepTurnContext;
use lancet_authority::policy::default::{DefaultPolicyHook, PolicyConfig};
use lancet_catalog::methods::audit::{AuditReplay, AuditReplayParams};
use lancet_native::{
    AuditQueryFilters, CepAuditContext, CepAuditStore, CepTransport, MethodRegistry,
    ServerRunConfig, register_production_method_registry,
};
use lancet_protocol::effect_types::ImpactKind;

use lattice_agent::LlmClient;
use lattice_agent::testing::FakeStreamingLlmClient;
use lattice_api::{RunTurnRequest, RunTurnStatus};
use lattice_llm::{LlmEvent, TokenUsage};
use lattice_profile::{DiscoveryPaths, LoadedAgentProfile, ProfileSource};
use serde_json::Value;
use tokio::sync::RwLock;

fn usage_tokens() -> TokenUsage {
    TokenUsage {
        prompt_tokens: 12,
        completion_tokens: 6,
        total_tokens: 18,
    }
}

fn tool_call_events(id: &str, name: &str, args: &str) -> Vec<LlmEvent> {
    vec![
        LlmEvent::ToolCallStart {
            id: id.to_string(),
            name: name.to_string(),
        },
        LlmEvent::ToolCallDelta {
            id: id.to_string(),
            arguments_delta: args.to_string(),
        },
        LlmEvent::ToolCallEnd { id: id.to_string() },
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

struct V14Workspace {
    _dir: tempfile::TempDir,
    _state: tempfile::TempDir,
    _global_agents: tempfile::TempDir,
    root: std::path::PathBuf,
    canonical_root: std::path::PathBuf,
    loaded: LoadedAgentProfile,
}

fn setup_v1_4_workspace() -> V14Workspace {
    let state = tempfile::tempdir().expect("state dir");
    unsafe {
        std::env::set_var("XDG_STATE_HOME", state.path());
    }
    let ws = tempfile::tempdir().expect("temp workspace");
    let root = ws.path().to_path_buf();

    std::fs::create_dir_all(root.join("src")).expect("src dir");
    std::fs::write(
        root.join("src/lib.rs"),
        "pub fn seed() -> &'static str { \"v1_4\" }\n",
    )
    .expect("seed source");
    std::fs::write(
        root.join("v1_4-template.txt"),
        "v1.4 artifact produced by local command\n",
    )
    .expect("artifact template");
    support::init_git_repo(&root);

    let profile_dir = root.join(".lattice/agents/default");
    std::fs::create_dir_all(&profile_dir).expect("profile dir");
    let profile = r#"schema_version = 1
[agent]
name = "default"
model = "offline-fake"
[prompt]
system = "You are a deterministic v1.4-alpha dogfood agent."
[tools]
enabled = ["exec.command_run"]
[workspace]
image = "test-template"
[lancet.workspace]
allowed_write_paths = ["src/**"]
"#;
    std::fs::write(profile_dir.join("agent.toml"), profile).expect("profile");

    let global_agents = tempfile::tempdir().expect("global agents");
    let paths = DiscoveryPaths {
        project_root: root.clone(),
        global_agents_dir: global_agents.path().to_path_buf(),
    };
    let loaded = lattice_profile::resolve_agent_profile(ProfileSource::Default, &paths)
        .expect("profile must load without provider credentials");
    let canonical_root = std::fs::canonicalize(&root).unwrap_or_else(|_| root.clone());

    V14Workspace {
        _dir: ws,
        _state: state,
        _global_agents: global_agents,
        root,
        canonical_root,
        loaded,
    }
}

fn make_v1_4_request(
    ws: &V14Workspace,
    session_id: &str,
    llm: Arc<dyn LlmClient>,
) -> RunTurnRequest {
    std::env::set_current_dir(&ws.root).expect("set workspace cwd");
    support::attach_upper_fixture(
        RunTurnRequest {
            input: "Create the v1.4-alpha artifact through the production API turn.".to_string(),
            session_id: session_id.to_string(),
            loaded_profile: Some(ws.loaded.clone()),
            cancel_flag: None,
            test_llm: Some(llm),
            test_project_root: Some(ws.root.to_string_lossy().to_string()),
            test_persist_audit: Some(false),
            workspace_override: None,
        },
        &["exec.command_run"],
    )
}

/// Create an unattached branch at the current HEAD without checking it out.
fn create_unattached_target_branch(root: &std::path::Path, name: &str) -> String {
    std::process::Command::new("git")
        .args(["branch", name])
        .current_dir(root)
        .status()
        .expect("git branch (create unattached target)");
    name.to_string()
}

#[tokio::test]
async fn v1_4_command_artifact_dogfood() {
    let ws = setup_v1_4_workspace();
    let session_dir = tempfile::tempdir().expect("session dir");
    let sessions_path = session_dir.path().join("sessions");
    let target = create_unattached_target_branch(&ws.root, "lattice-target-dogfood");
    let session_id = lattice_api::SessionApi::git(&sessions_path)
        .create_session(lattice_api::CreateSessionRequest {
            label: None,
            active_agent: "default".to_string(),
            project_branch: target,
        })
        .expect("create session")
        .session_id;

    let run_args = serde_json::json!({
        "argv": [
            "cp",
            "v1_4-template.txt",
            "src/v1_4-artifact.txt"
        ],
        "timeout_ms": 5000,
        "declared_impact": "workspace_mutation"
    });

    let fake_llm: Arc<dyn LlmClient> = Arc::new(
        FakeStreamingLlmClient::builder()
            .push_events(tool_call_events(
                "call_0",
                "exec.command_run",
                &run_args.to_string(),
            ))
            .push_events(final_response_events(
                "v1.4-alpha dogfood artifact produced.",
            ))
            .build(),
    );

    let result = lattice_api::run_turn(
        lattice_api::SessionApi::git(&sessions_path),
        make_v1_4_request(&ws, &session_id, fake_llm),
    )
    .await
    .expect("run_turn must succeed with fake LLM");

    assert_eq!(
        result.status,
        RunTurnStatus::Completed,
        "turn must complete through run_turn; output: {}",
        result.assistant_output
    );
    assert!(
        result
            .assistant_output
            .contains("v1.4-alpha dogfood artifact produced"),
        "assistant output: {}",
        result.assistant_output
    );

    let command_result = result
        .tool_results
        .iter()
        .find(|tool| tool.name == "exec.command_run")
        .expect("exec.command_run tool result");
    assert!(
        command_result.ok,
        "exec.command_run must succeed: {:?}",
        command_result.error_summary
    );

    let artifact = ws.root.join("src/v1_4-artifact.txt");
    assert!(
        !artifact.exists(),
        "workspace-mutating cp must not write artifact to Lower host tree under Direct Upper routing"
    );

    let audit_path =
        audit_db_path_for_project_in(ws._state.path(), &ws.canonical_root).expect("audit path");
    assert!(
        audit_path.exists(),
        "persisted audit DB must exist at {audit_path:?}"
    );
    let audit = Arc::new(CepAuditStore::open(&audit_path).expect("open persisted audit DB"));
    let turn_id = format!("{}:root", result.session_id);
    let rows = audit
        .query_rows(&AuditQueryFilters {
            turn_id: Some(turn_id.clone()),
            ..Default::default()
        })
        .expect("query turn audit rows");
    let methods: Vec<&str> = rows.iter().map(|row| row.method.as_str()).collect();
    assert!(
        methods.contains(&"exec.command_run"),
        "exec.command_run must appear in audit rows: {methods:?}"
    );

    let command_row = rows
        .iter()
        .find(|row| row.method == "exec.command_run")
        .expect("command audit row");
    assert_eq!(
        command_row.impact_kind, "command_execution",
        "audit row impact_kind remains the catalog method class"
    );
    let command_meta: Value = serde_json::from_str(
        command_row
            .metadata_json
            .as_deref()
            .expect("command metadata_json"),
    )
    .expect("command metadata JSON");
    assert_eq!(
        command_meta["classification"].as_str(),
        Some("workspace_mutation"),
        "command metadata must record the effective declared workspace mutation"
    );
    assert_eq!(
        command_meta["exit_code"].as_i64(),
        Some(0),
        "cp must succeed in Upper: {command_meta}"
    );
    assert!(
        command_meta.get("upper_workspace_id").is_some(),
        "command metadata must include upper_workspace_id: {command_meta}"
    );
    assert_eq!(
        command_meta["execution_backend"].as_str(),
        Some("deterministic"),
        "command metadata must record the Upper execution backend: {command_meta}"
    );
    assert!(
        command_meta["backend_invoked"].as_bool().unwrap_or(false),
        "command metadata must record backend invocation: {command_meta}"
    );
    assert!(
        command_meta["policy_enforced"].as_bool().unwrap_or(false),
        "command metadata must record policy enforcement: {command_meta}"
    );
    assert!(
        !command_meta
            .to_string()
            .contains("v1.4 artifact produced by local command"),
        "command metadata must not leak artifact body"
    );

    let replay = replay_turn(&ws, Arc::clone(&audit), &result.session_id, &turn_id).await;
    let command_activity = replay
        .trace
        .fragment
        .activities
        .iter()
        .find(|activity| activity.method == "exec.command_run")
        .expect("replay command activity");
    assert_eq!(
        command_activity.impact_kind,
        ImpactKind::CommandExecution,
        "replay must preserve the catalog command activity impact"
    );
    assert!(
        command_activity.attributes.contains_key("cep_policy"),
        "replay command activity must preserve cep_policy attributes: {:?}",
        command_activity.attributes
    );
    assert!(
        !replay.trace.fragment.activities.is_empty(),
        "replay must include command activity entries"
    );
    let statement = replay.statement.expect("replay statement");
    let statement_json = serde_json::to_value(statement).expect("statement JSON");
    assert!(
        statement_json.to_string().contains("exec.command_run"),
        "replay statement materials must include command activity"
    );
    assert!(
        !statement_json
            .to_string()
            .contains("v1.4 artifact produced by local command"),
        "replay statement must not leak artifact body"
    );
}

async fn replay_turn(
    ws: &V14Workspace,
    audit: Arc<CepAuditStore>,
    session_id: &str,
    turn_id: &str,
) -> lancet_catalog::methods::audit::AuditReplayResult {
    let mut registry = MethodRegistry::new();
    register_production_method_registry(&mut registry).expect("production registry");

    let root = ws.canonical_root.to_str().expect("workspace root string");
    let replay_audit_ctx = Arc::new(RwLock::new(CepAuditContext {
        turn_id: "v1_4-replay-turn".into(),
        session_id: session_id.to_string(),
        ..Default::default()
    }));
    let policy_cfg = PolicyConfig::defaults_for_project_root(ws.canonical_root.clone());
    let hook = Arc::new(DefaultPolicyHook::new(policy_cfg));

    let mut config = ServerRunConfig::in_memory_for_tests();
    config.audit = audit;
    config.cep_context = Arc::new(CepTurnContext::defaults_for_project_root(root));
    config.audit_context = replay_audit_ctx;
    config.policy_hook = hook.clone();
    config.default_policy_hook = Some(hook);

    let (client, server) = CepTransport::pair(16);
    let server_task = tokio::spawn(async move {
        let _ = server.run(registry, config).await;
    });
    tokio::task::yield_now().await;

    let replay = client
        .call::<AuditReplay>(AuditReplayParams {
            turn_id: turn_id.to_string(),
        })
        .await
        .expect("observe.audit_replay");
    drop(client);
    server_task.abort();
    replay
}
