//! Full-turn audit rows carry PROV-relevant fields after a live CEP turn.

use std::collections::BTreeSet;
use std::sync::Arc;

use lancet_catalog::methods::audit::{AuditReplay, AuditReplayParams};
use lancet_native::{
    register_production_method_registry, AuditQueryFilters, CepAuditStore, CepTransport,
    CepTurnContext, MethodRegistry, ServerRunConfig,
};
use lancet_protocol::envelope::{
    BaseExecutionEnvelope, Capability, Constraints, ExecutionEnvelope, Impact, ImpactConfidence,
    ImpactKind, Intent, OnFailure, Scope,
};
use lattice_agent::testing::FakeStreamingLlmClient;
use lattice_agent::LlmClient;
use lattice_api::{RunTurnRequest, RunTurnStatus};
use lattice_llm::LlmEvent;
use lattice_profile::{DiscoveryPaths, LoadedAgentProfile, ProfileSource};
use serde_json::Value;
use tempfile::TempDir;

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
            usage: None,
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
            usage: None,
        },
    ]
}

struct Workspace {
    _dir: TempDir,
    root: std::path::PathBuf,
    loaded: LoadedAgentProfile,
}

fn setup_workspace() -> Workspace {
    let ws = TempDir::new().unwrap();
    let root = ws.path().to_path_buf();
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/lib.rs"), "pub fn answer() -> i32 { 1 }").unwrap();

    let profile_dir = root.join(".lattice/agents/default");
    std::fs::create_dir_all(&profile_dir).unwrap();
    let enabled = [
        "observe.workspace",
        "exec.command_run",
        "session.envelope",
        "mutate.patch",
    ];
    let toml_content = format!(
        r#"schema_version = 1
[agent]
name = "default"
model = "sonnet"
[prompt]
system = "test"
[tools]
enabled = {enabled:?}
"#
    );
    std::fs::write(profile_dir.join("agent.toml"), toml_content).unwrap();

    let global_agents = TempDir::new().unwrap();
    let paths = DiscoveryPaths {
        project_root: root.clone(),
        global_agents_dir: global_agents.path().to_path_buf(),
    };
    let loaded =
        lattice_profile::resolve_agent_profile(ProfileSource::Default, &paths).expect("profile");

    Workspace {
        _dir: ws,
        root,
        loaded,
    }
}

fn metadata_has_capability(meta: Option<&str>) -> bool {
    let Some(raw) = meta else {
        return false;
    };
    let Ok(v) = serde_json::from_str::<Value>(raw) else {
        return false;
    };
    v.get("required_capability").is_some() || v.get("capabilities").is_some()
}

#[tokio::test]
async fn full_turn_audit_rows_and_replay_fragment() {
    let ws = setup_workspace();
    std::env::set_current_dir(&ws.root).unwrap();

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
    let mut capabilities = BTreeSet::new();
    capabilities.insert(Capability::WritePath {
        path_pattern: "src/**".into(),
    });
    let envelope = ExecutionEnvelope {
        intent: Intent {
            description: "audit patch".into(),
        },
        capabilities,
        scope: Scope::default(),
        impact: vec![Impact {
            kind: ImpactKind::WorkspaceMutation,
            target: "src/lib.rs".into(),
            scope: None,
            confidence: ImpactConfidence::Conservative,
            unknowns: vec![],
        }],
        constraints: Constraints::default(),
        verify: None,
        on_failure: Some(OnFailure::RetainDiff),
        extensions: serde_json::Map::new(),
    };
    let declare_args = serde_json::json!({
        "mode": "declare",
        "envelope": envelope,
        "reason": "audit-turn"
    });

    let fake_llm: Arc<dyn LlmClient> = Arc::new(
        FakeStreamingLlmClient::builder()
            .push_events(tool_call_events(
                "c1",
                "observe.workspace",
                r#"{"mode":"read_file","path":"src/lib.rs"}"#,
            ))
            .push_events(tool_call_events(
                "c2",
                "exec.command_run",
                r#"{"argv":["echo","audit-turn"]}"#,
            ))
            .push_events(tool_call_events(
                "c3",
                "session.envelope",
                &declare_args.to_string(),
            ))
            .push_events(tool_call_events(
                "c4",
                "mutate.patch",
                &format!(
                    r#"{{"patch":{}}}"#,
                    serde_json::to_string(&patch_doc).unwrap()
                ),
            ))
            .push_events(final_response_events("done"))
            .build(),
    );

    let session_dir = TempDir::new().unwrap();
    let sessions_path = session_dir.path().join("sessions");
    let session_id = lattice_api::SessionApi::sqlite(&sessions_path)
        .create_session(lattice_api::CreateSessionRequest {
            label: None,
            active_agent: "default".to_string(),
        })
        .expect("create session")
        .session_id;

    let result = lattice_api::run_turn(
        lattice_api::SessionApi::sqlite(&sessions_path),
        RunTurnRequest {
            input: "audit turn provenance".into(),
            session_id,
            loaded_profile: Some(ws.loaded.clone()),
            cancel_flag: None,
            test_llm: Some(fake_llm),
            test_project_root: Some(ws.root.to_string_lossy().to_string()),
            test_persist_audit: Some(false),
        },
    )
    .await
    .expect("run_turn");

    assert_eq!(result.status, RunTurnStatus::Completed);

    let audit_path = ws.root.join(".lattice").join("cep_audit.db");
    assert!(audit_path.exists(), "audit db must be persisted on disk");

    let audit = CepAuditStore::open(&audit_path).expect("open audit");
    let rows = audit
        .query_rows(&AuditQueryFilters {
            session_id: Some(result.session_id.clone()),
            ..Default::default()
        })
        .expect("query rows");

    let tool_rows: Vec<_> = rows
        .iter()
        .filter(|r| !r.method.starts_with("audit."))
        .collect();
    assert!(
        tool_rows.len() >= 3,
        "expected read + command + mutation calls, got {}",
        tool_rows.len()
    );

    for row in &tool_rows {
        assert!(!row.call_id.to_string().is_empty());
        assert!(!row.method.is_empty());
        assert!(!row.impact_kind.is_empty());
        assert!(!row.turn_id.is_empty());
        assert_eq!(row.session_id, result.session_id);
        assert!(
            row.model_id.as_deref().is_some_and(|m| !m.is_empty()),
            "model_id required on row {}",
            row.audit_id
        );
        let meta = row.metadata_json.as_deref();
        assert!(
            metadata_has_capability(meta) || row.method == "session.envelope",
            "row {} missing capability metadata",
            row.method
        );
        if matches!(row.method.as_str(), "mutate.patch" | "mutate.create") {
            let parsed: Value = serde_json::from_str(meta.unwrap_or("{}")).unwrap_or(Value::Null);
            assert!(
                parsed.get("envelope_id").is_some(),
                "mutation row should include envelope_id"
            );
        }
    }

    let mut registry = MethodRegistry::new();
    register_production_method_registry(&mut registry).expect("registry");
    let root = ws.root.to_str().unwrap();
    let mut config = ServerRunConfig::in_memory_for_tests();
    config.audit = Arc::new(audit);
    config.base_envelope =
        BaseExecutionEnvelope::for_workspace_root(root, "audit replay").with_read_audit();
    config.cep_context = Arc::new(CepTurnContext::defaults_for_project_root(root));
    {
        let mut ctx = config.audit_context.write().await;
        ctx.session_id = result.session_id.clone();
    }

    let (client, server) = CepTransport::pair(32);
    let server_task = tokio::spawn(async move {
        server.run(registry, config).await.unwrap();
    });

    let turn_id = tool_rows[0].turn_id.clone();
    let replay = client
        .call::<AuditReplay>(AuditReplayParams { turn_id })
        .await
        .expect("observe.audit_replay");

    assert_eq!(
        replay.trace.fragment.activities.len(),
        tool_rows.len(),
        "replay activity count should match tool audit rows"
    );
    assert!(
        replay
            .trace
            .fragment
            .activities
            .iter()
            .any(|a| { !a.method.is_empty() && a.impact_kind != ImpactKind::AuditObservation }),
        "replay fragment must include tool activities"
    );

    drop(client);
    server_task.abort();
}
