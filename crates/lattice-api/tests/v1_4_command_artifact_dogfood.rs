// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! v1.4-alpha no-key command/artifact metadata dogfood proof.
//!
//! This test is release-gated as offline dogfood evidence. It proves the
//! production API/CEP/audit path records command artifact metadata and fallback
//! honesty without provider keys or uncontrolled network access. It does not
//! claim live artifact isolation when the local host reports no overlay path.

use std::collections::BTreeSet;
use std::sync::Arc;

use lancet_authority::CepTurnContext;
use lancet_authority::policy::default::{DefaultPolicyHook, PolicyConfig};
use lancet_catalog::methods::audit::{AuditReplay, AuditReplayParams};
use lancet_native::{
    AuditQueryFilters, CepAuditContext, CepAuditStore, CepTransport, MethodRegistry,
    ServerRunConfig, register_production_method_registry,
};
use lancet_protocol::envelope::{
    BaseExecutionEnvelope, Capability, Constraints, ExecutionEnvelope, Impact, ImpactConfidence,
    ImpactKind, Intent, Scope,
};
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
    _global_agents: tempfile::TempDir,
    root: std::path::PathBuf,
    canonical_root: std::path::PathBuf,
    loaded: LoadedAgentProfile,
}

fn setup_v1_4_workspace() -> V14Workspace {
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

    let profile_dir = root.join(".lattice/agents/default");
    std::fs::create_dir_all(&profile_dir).expect("profile dir");
    let profile = r#"schema_version = 1
[agent]
name = "default"
model = "offline-fake"
[prompt]
system = "You are a deterministic v1.4-alpha dogfood agent."
[tools]
enabled = ["session.envelope", "exec.command_run", "observe.audit_replay"]
[lancet.isolation.command]
mode = "audit"
required_boundaries = ["filesystem"]
unsupported_host = "allow_unbounded"
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
    RunTurnRequest {
        input: "Create the v1.4-alpha artifact through the production API turn.".to_string(),
        session_id: session_id.to_string(),
        loaded_profile: Some(ws.loaded.clone()),
        cancel_flag: None,
        test_llm: Some(llm),
        test_project_root: Some(ws.root.to_string_lossy().to_string()),
        test_persist_audit: Some(false),
    }
}

#[tokio::test]
async fn v1_4_command_artifact_dogfood() {
    let ws = setup_v1_4_workspace();
    let session_dir = tempfile::tempdir().expect("session dir");
    let sessions_path = session_dir.path().join("sessions");
    let session_id = lattice_api::SessionApi::sqlite(&sessions_path)
        .create_session(lattice_api::CreateSessionRequest {
            label: None,
            active_agent: "default".to_string(),
        })
        .expect("create session")
        .session_id;

    let envelope = ExecutionEnvelope {
        intent: Intent {
            description: "v1.4-alpha no-key command artifact dogfood".into(),
        },
        capabilities: BTreeSet::from([
            Capability::RunCommand {
                argv_allowlist: vec!["cp v1_4-template.txt src/v1_4-artifact.txt".into()],
            },
            Capability::WritePath {
                path_pattern: "src/**".into(),
            },
        ]),
        scope: Scope::default(),
        impact: vec![Impact {
            kind: ImpactKind::WorkspaceMutation,
            target: "src/v1_4-artifact.txt".into(),
            scope: None,
            confidence: ImpactConfidence::Conservative,
            unknowns: vec![],
        }],
        constraints: Constraints::default(),
        verify: None,
        on_failure: None,
        extensions: serde_json::Map::new(),
    };
    let declare_args = serde_json::json!({
        "mode": "declare",
        "envelope": envelope,
        "supersede": false,
        "reason": "v1.4-alpha dogfood artifact scope"
    });
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
                "session.envelope",
                &declare_args.to_string(),
            ))
            .push_events(tool_call_events(
                "call_1",
                "exec.command_run",
                &run_args.to_string(),
            ))
            .push_events(final_response_events(
                "v1.4-alpha dogfood artifact produced.",
            ))
            .build(),
    );

    let result = lattice_api::run_turn(
        lattice_api::SessionApi::sqlite(&sessions_path),
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

    let declare_result = result
        .tool_results
        .iter()
        .find(|tool| tool.name == "session.envelope")
        .expect("session.envelope tool result");
    assert!(
        declare_result.ok,
        "session.envelope must succeed before command: {:?}",
        declare_result.error_summary
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
    let artifact_body = std::fs::read_to_string(&artifact).expect("artifact file");
    assert_eq!(
        artifact_body, "v1.4 artifact produced by local command\n",
        "artifact must be written by local process execution"
    );

    let audit_path = ws.canonical_root.join(".lattice").join("cep_audit.db");
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
    let declare_pos = methods
        .iter()
        .position(|method| *method == "session.envelope")
        .expect("session.envelope audit row");
    let command_pos = methods
        .iter()
        .position(|method| *method == "exec.command_run")
        .expect("exec.command_run audit row");
    assert!(
        declare_pos < command_pos,
        "session.envelope must precede exec.command_run in audit order: {methods:?}"
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
    assert!(
        command_meta.get("effective_boundary").is_some(),
        "command metadata must include effective_boundary: {command_meta}"
    );
    assert!(
        command_meta.get("overlay_status").is_some(),
        "command metadata must include overlay_status: {command_meta}"
    );
    assert!(
        command_meta.get("artifact_adjudication").is_some(),
        "command metadata must include artifact_adjudication: {command_meta}"
    );
    assert_eq!(
        command_meta["classification"].as_str(),
        Some("workspace_mutation"),
        "command metadata must record the effective declared workspace mutation"
    );
    assert!(
        !command_meta.to_string().contains(&artifact_body),
        "command metadata must not leak artifact body"
    );

    let overlay_status = command_meta["overlay_status"].as_str().unwrap_or("");
    let artifact_adjudication = command_meta["artifact_adjudication"].as_str().unwrap_or("");
    let commit_outcome = command_meta["commit_outcome"].as_str().unwrap_or("");
    assert!(
        matches!(
            overlay_status,
            "applied" | "not_applied" | "not_requested" | "setup_failed"
        ),
        "unexpected overlay_status={overlay_status:?}"
    );
    if overlay_status == "applied" {
        assert_eq!(
            artifact_adjudication, "accepted",
            "applied dogfood path must adjudicate the artifact as accepted: {command_meta}"
        );
        assert_eq!(
            commit_outcome, "committed",
            "applied dogfood path must commit through the artifact pipeline: {command_meta}"
        );
    } else {
        assert_eq!(
            artifact_adjudication, "skipped_no_overlay",
            "fallback dogfood path must label artifact adjudication honestly: {command_meta}"
        );
        assert_eq!(
            commit_outcome, "not_started",
            "fallback dogfood path must not imply artifact commit happened: {command_meta}"
        );
        let boundary = command_meta["effective_boundary"]
            .as_object()
            .expect("effective_boundary object");
        for (dimension, entry) in boundary {
            assert_ne!(
                entry["status"].as_str(),
                Some("physically_bounded"),
                "no-overlay or unsupported dogfood path must not claim physical proof for {dimension}: {entry}"
            );
        }
    }

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
        command_activity
            .attributes
            .contains_key("effective_boundary"),
        "replay command activity must preserve effective_boundary attributes"
    );
    assert!(
        replay
            .trace
            .fragment
            .was_informed_by
            .iter()
            .any(|edge| edge.informed_activity_id != edge.informing_activity_id),
        "replay must include non-self provenance ordering edges"
    );
    let statement = replay.statement.expect("replay statement");
    let statement_json = serde_json::to_value(statement).expect("statement JSON");
    assert!(
        statement_json.to_string().contains("exec.command_run"),
        "replay statement materials must include command activity"
    );
    assert!(
        !statement_json.to_string().contains(&artifact_body),
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
    let replay_base = BaseExecutionEnvelope::for_workspace_root(root, "v1.4 observe.audit_replay")
        .with_read_audit();
    let replay_audit_ctx = Arc::new(RwLock::new(CepAuditContext {
        turn_id: "v1_4-replay-turn".into(),
        session_id: session_id.to_string(),
        ..Default::default()
    }));
    let policy_cfg = PolicyConfig::defaults_for_project_root(ws.canonical_root.clone());
    let hook = Arc::new(DefaultPolicyHook::new(policy_cfg));

    let mut config = ServerRunConfig::in_memory_for_tests();
    config.audit = audit;
    config.base_envelope = replay_base;
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
