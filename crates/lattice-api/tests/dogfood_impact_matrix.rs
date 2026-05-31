//! DOG-04: Production-native five-impact-class matrix.
//!
//! Exercises one representative CEP method per `ImpactKind` through the
//! production method registry (`register_production_method_registry`).
//!
//! The legacy backend (`register_phase20_legacy_backend`) is **never** registered.
//!
//! For each impact class:
//! 1. Declare the required envelope capabilities (where needed).
//! 2. Execute the representative method via [`LancetClient`] typed calls.
//! 3. Assert the audit row: `impact_kind`, `semantic_stage = "phase21_native"`,
//!    `policy_enforced = true`, redacted metadata (no secret bodies).
//! 4. Verify that all rows share one `turn_id` before calling `audit.replay`.
//! 5. Assert PROV `activities` order and `wasInformedBy` chain from replay.

use std::collections::BTreeSet;
use std::sync::Arc;

use lancet_executor::audit::CepAuditContext;
use lancet_executor::policy::default::{DefaultPolicyHook, PolicyConfig};
use lancet_executor::{
    register_production_method_registry, CepAuditStore, LancetTransport, MethodRegistry,
    Phase21PolicyConfig, Phase21TurnContext, ServerRunConfig, SEMANTIC_STAGE_NATIVE,
};
use lancet_protocol::envelope::{
    BaseExecutionEnvelope, Capability, Constraints, ExecutionEnvelope, ImpactKind, Intent, Scope,
};
use lancet_protocol::methods::audit::{AuditReplay, AuditReplayParams};
use lancet_protocol::methods::command::{Run, RunParams};
use lancet_protocol::methods::envelope::{Declare, DeclareParams};
use lancet_protocol::methods::external::{Fetch, FetchParams};
use lancet_protocol::methods::session::{TodoItem, TodoStatus, TodoWrite, TodoWriteParams};
use lancet_protocol::methods::workspace::{Patch, PatchParams, ReadFile, ReadFileParams};
use tempfile::TempDir;
use tokio::sync::RwLock;

const FIXED_TURN_ID: &str = "dog04-turn-001";
const FIXED_SESSION_ID: &str = "dog04-session-001";

// ── Helpers ────────────────────────────────────────────────────────────

async fn spawn_echo_server(status_line: &str, body: &str) -> String {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind localhost");
    let addr = listener.local_addr().expect("local addr");
    let body_owned = body.to_string();
    let status_owned = status_line.to_string();
    tokio::spawn(async move {
        if let Ok((mut stream, _)) = listener.accept().await {
            let mut buf = [0u8; 1024];
            let _ = stream.read(&mut buf).await;
            let response = format!(
                "HTTP/1.1 {status_owned}\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body_owned}",
                body_owned.len()
            );
            let _ = stream.write_all(response.as_bytes()).await;
        }
    });
    format!("http://127.0.0.1:{}/", addr.port())
}

/// Build a production server config with a fixed turn_id and the given base envelope.
async fn build_production_config(
    canonical_root: &str,
    base_envelope: BaseExecutionEnvelope,
    audit: Arc<CepAuditStore>,
    allow_localhost: bool,
) -> ServerRunConfig {
    let mut policy = Phase21PolicyConfig::defaults_for_project_root(canonical_root);
    if allow_localhost {
        policy.network.allow_localhost = true;
        policy.network.allowed_hosts = vec!["127.0.0.1".into(), "localhost".into()];
    }

    let policy_cfg =
        PolicyConfig::defaults_for_project_root(std::path::Path::new(canonical_root).to_path_buf());
    let hook = Arc::new(DefaultPolicyHook::new(policy_cfg));

    let mut config = ServerRunConfig::in_memory_for_tests();
    config.audit = audit;
    config.base_envelope = base_envelope;
    config.phase21 = Arc::new(Phase21TurnContext::from_policy(policy));
    config.production_audit_routing = true;
    config.policy_hook = hook.clone();
    config.default_policy_hook = Some(hook);
    {
        let mut ctx = config.audit_context.write().await;
        ctx.turn_id = FIXED_TURN_ID.to_string();
        ctx.session_id = FIXED_SESSION_ID.to_string();
    }
    config
}

// ── The five-class matrix test ─────────────────────────────────────────

#[tokio::test]
async fn dogfood_impact_matrix_five_production_classes() {
    let dir = TempDir::new().expect("temp workspace");
    let root = dir.path();

    // Create initial file for read and patch operations.
    std::fs::create_dir_all(root.join("src")).expect("src dir");
    let seed_content = "hello dog04\n";
    std::fs::write(root.join("src/dog04.txt"), seed_content).expect("seed file");

    let canonical = std::fs::canonicalize(root).expect("canonicalize root");
    let canonical_str = canonical.to_str().unwrap();

    // Compute blake3 hash for workspace.patch
    let file_hash = format!("blake3:{}", blake3::hash(seed_content.as_bytes()).to_hex());
    let patch_doc = format!(
        "*** Begin Patch\n\
         *** Update File: src/dog04.txt\n\
         *** Old Hash: {file_hash}\n\
         @@\n\
         -hello dog04\n\
         +hello dog04 patched\n\
         *** End Patch\n"
    );

    // Build base envelope: ReadPath(root) + FetchUrl(*) + MutateSessionState + ReadAudit
    let base_envelope =
        BaseExecutionEnvelope::for_workspace_root(canonical_str, "dog04 impact matrix")
            .with_fetch_host("127.0.0.1")
            .with_session_state()
            .with_read_audit();

    let audit = Arc::new(CepAuditStore::in_memory().expect("audit store"));

    // ── Phase 1: execute the five impact classes ───────────────────────

    let mut registry = MethodRegistry::new();
    register_production_method_registry(&mut registry).expect("production registry");

    let fetch_url = spawn_echo_server("200 OK", "dog04-fetch-body").await;

    let config =
        build_production_config(canonical_str, base_envelope, Arc::clone(&audit), true).await;
    let (client1, server1) = LancetTransport::pair(32);
    let server_task1 = tokio::spawn(async move {
        let _ = server1.run(registry, config).await;
    });
    tokio::task::yield_now().await;

    // Row 1 — WorkspaceObservation: workspace.read_file
    let read_result = client1
        .call::<ReadFile>(ReadFileParams {
            path: "src/dog04.txt".into(),
            offset: None,
            limit: None,
            line_numbers: None,
        })
        .await
        .expect("workspace.read_file");
    assert!(
        read_result.content.contains("hello dog04"),
        "read_file returned unexpected content: {:?}",
        read_result.content
    );

    // Declare envelope for CommandExecution (RunCommand requires Declared source).
    let run_envelope = ExecutionEnvelope {
        intent: Intent {
            description: "dog04 command.run".into(),
        },
        capabilities: BTreeSet::from([Capability::RunCommand {
            argv_allowlist: vec!["echo".into()],
        }]),
        scope: Scope::default(),
        impact: vec![],
        constraints: Constraints::default(),
        verify: None,
        on_failure: None,
        extensions: serde_json::Map::new(),
    };
    let declare_run = client1
        .call::<Declare>(DeclareParams {
            envelope: run_envelope,
            supersede: false,
            reason: Some("dog04 RunCommand".into()),
        })
        .await
        .expect("envelope.declare for command.run");
    assert!(declare_run.accepted, "RunCommand envelope must be accepted");

    // Row 2 — CommandExecution: command.run echo
    let run_result = client1
        .call::<Run>(RunParams {
            argv: vec!["echo".into(), "dog-impact-matrix".into()],
            cwd: None,
            env: Default::default(),
            timeout_ms: Some(5000),
            declared_impact: Some(ImpactKind::CommandExecution),
        })
        .await
        .expect("command.run");
    assert_eq!(run_result.exit_code, 0);
    assert!(
        run_result.stdout.contains("dog-impact-matrix"),
        "command.run stdout must contain dog-impact-matrix"
    );

    // Declare envelope for WorkspaceMutation (WritePath requires Declared source).
    let write_envelope = ExecutionEnvelope {
        intent: Intent {
            description: "dog04 workspace.patch".into(),
        },
        capabilities: BTreeSet::from([Capability::WritePath {
            path_pattern: "src/**".into(),
        }]),
        scope: Scope::default(),
        impact: vec![],
        constraints: Constraints::default(),
        verify: None,
        on_failure: None,
        extensions: serde_json::Map::new(),
    };
    let declare_write = client1
        .call::<Declare>(DeclareParams {
            envelope: write_envelope,
            supersede: true, // supersede the RunCommand envelope
            reason: Some("dog04 WritePath".into()),
        })
        .await
        .expect("envelope.declare for workspace.patch");
    assert!(
        declare_write.accepted,
        "WritePath envelope must be accepted"
    );

    // Row 3 — WorkspaceMutation: workspace.patch
    let patch_result = client1
        .call::<Patch>(PatchParams {
            patch: patch_doc.clone(),
        })
        .await
        .expect("workspace.patch");
    assert_eq!(
        patch_result.changed_files.len(),
        1,
        "patch must change 1 file"
    );

    // Row 4 — ExternalObservation: web.fetch (localhost echo server)
    let fetch_result = client1
        .call::<Fetch>(FetchParams {
            url: fetch_url.clone(),
            headers: Default::default(),
        })
        .await
        .expect("web.fetch");
    assert_eq!(fetch_result.status, 200);
    assert_eq!(
        fetch_result.body, "dog04-fetch-body",
        "web.fetch body must match echo server"
    );

    // Row 5 — SessionStateMutation: session.todo_write
    let todo_result = client1
        .call::<TodoWrite>(TodoWriteParams {
            todos: vec![TodoItem {
                content: "dog04 impact matrix verified".into(),
                status: TodoStatus::Completed,
            }],
        })
        .await
        .expect("session.todo_write");
    assert_eq!(todo_result.todos.len(), 1);

    // Shut down first server.
    drop(client1);
    server_task1.abort();
    tokio::task::yield_now().await;

    // ── Phase 2: assert audit rows ─────────────────────────────────────

    // All rows (tool calls + envelope.declare calls) share the fixed turn_id.
    let all_rows = audit
        .query_rows(&lancet_executor::AuditQueryFilters {
            session_id: Some(FIXED_SESSION_ID.to_string()),
            ..Default::default()
        })
        .expect("query audit rows");

    assert!(
        !all_rows.is_empty(),
        "audit store must have rows after five impact classes"
    );

    for row in &all_rows {
        assert_eq!(
            row.turn_id, FIXED_TURN_ID,
            "all rows must share turn_id={FIXED_TURN_ID}"
        );
        assert_eq!(
            row.semantic_stage, SEMANTIC_STAGE_NATIVE,
            "row {} ({}) must use native semantic stage",
            row.audit_id, row.method
        );
        assert!(
            row.policy_enforced,
            "row {} ({}) must be policy_enforced",
            row.audit_id, row.method
        );
    }

    // Filter to the five representative impact-class rows (exclude envelope.declare rows).
    let impact_rows: Vec<_> = all_rows
        .iter()
        .filter(|r| r.method != "envelope.declare")
        .collect();

    assert_eq!(
        impact_rows.len(),
        5,
        "must have exactly 5 non-declare rows, got {}: {:?}",
        impact_rows.len(),
        impact_rows.iter().map(|r| &r.method).collect::<Vec<_>>()
    );

    // Verify the five impact kinds are present.
    let methods_by_impact: std::collections::HashMap<&str, &str> = impact_rows
        .iter()
        .map(|r| (r.impact_kind.as_str(), r.method.as_str()))
        .collect();

    for (expected_impact, expected_method) in [
        ("workspace_observation", "workspace.read_file"),
        ("command_execution", "command.run"),
        ("workspace_mutation", "workspace.patch"),
        ("external_observation", "web.fetch"),
        ("session_state_mutation", "session.todo_write"),
    ] {
        let method = methods_by_impact
            .get(expected_impact)
            .copied()
            .unwrap_or("");
        assert_eq!(
            method, expected_method,
            "impact_kind={expected_impact} must map to method={expected_method}"
        );
    }

    // Verify metadata fields for audit rows.
    for row in &impact_rows {
        let meta = row.metadata_json.as_deref().unwrap_or("{}");
        // Must not contain raw secret bodies (response bodies, file content).
        assert!(
            !meta.contains("dog04-fetch-body"),
            "audit metadata must not contain raw fetch body"
        );
        assert!(
            !meta.contains("dog04 impact matrix verified"),
            "audit metadata must not contain raw todo content"
        );
        // Must include required_capability field from merge_finalize_metadata.
        let meta_val: serde_json::Value = serde_json::from_str(meta).unwrap_or_default();
        assert!(
            meta_val.get("required_capability").is_some(),
            "row {} ({}) metadata must include required_capability",
            row.audit_id,
            row.method
        );
    }

    // ── Phase 3: audit.replay PROV assertions ─────────────────────────

    // Build a fresh server on the same audit store with ReadAudit cap for replay.
    let mut replay_registry = MethodRegistry::new();
    register_production_method_registry(&mut replay_registry).expect("replay registry");

    let replay_base =
        BaseExecutionEnvelope::for_workspace_root(canonical_str, "dog04 replay").with_read_audit();

    let replay_audit_ctx = Arc::new(RwLock::new(CepAuditContext {
        turn_id: "dog04-replay-turn".into(),
        session_id: FIXED_SESSION_ID.to_string(),
        ..Default::default()
    }));

    let policy_cfg2 = PolicyConfig::defaults_for_project_root(canonical.clone());
    let hook2 = Arc::new(DefaultPolicyHook::new(policy_cfg2));

    let mut replay_config = ServerRunConfig::in_memory_for_tests();
    replay_config.audit = Arc::clone(&audit);
    replay_config.base_envelope = replay_base;
    replay_config.phase21 = Arc::new(Phase21TurnContext::defaults_for_project_root(canonical_str));
    replay_config.production_audit_routing = true;
    replay_config.audit_context = replay_audit_ctx;
    replay_config.policy_hook = hook2.clone();
    replay_config.default_policy_hook = Some(hook2);

    let (replay_client, replay_server) = LancetTransport::pair(16);
    let replay_task = tokio::spawn(async move {
        let _ = replay_server.run(replay_registry, replay_config).await;
    });
    tokio::task::yield_now().await;

    let replay_result = replay_client
        .call::<AuditReplay>(AuditReplayParams {
            turn_id: FIXED_TURN_ID.to_string(),
        })
        .await
        .expect("audit.replay");

    drop(replay_client);
    replay_task.abort();

    // The trace covers ALL rows (5 impact rows + 2 envelope.declare rows = 7 total).
    let trace = &replay_result.trace;
    assert_eq!(
        trace.turn_id, FIXED_TURN_ID,
        "replay trace must carry fixed turn_id"
    );

    let activities = &trace.fragment.activities;
    assert_eq!(
        activities.len(),
        all_rows.len(),
        "replay activities count must match all audit rows ({}), got {}",
        all_rows.len(),
        activities.len()
    );

    // Verify activities are in execution order by checking audit_id ascending.
    let call_ids: Vec<i64> = trace.calls.iter().map(|c| c.audit_id).collect();
    let mut sorted_ids = call_ids.clone();
    sorted_ids.sort();
    assert_eq!(
        call_ids, sorted_ids,
        "replay calls must be in audit_id order"
    );

    // Verify impact kinds for the five production classes appear in the trace.
    let activity_impact_kinds: Vec<&ImpactKind> =
        activities.iter().map(|a| &a.impact_kind).collect();
    for expected in [
        ImpactKind::WorkspaceObservation,
        ImpactKind::CommandExecution,
        ImpactKind::WorkspaceMutation,
        ImpactKind::ExternalObservation,
        ImpactKind::SessionStateMutation,
    ] {
        assert!(
            activity_impact_kinds.contains(&&expected),
            "replay activities must include {:?}",
            expected
        );
    }

    // Verify wasInformedBy chain: N rows → N-1 relations linking consecutive activities.
    let was_informed_by = &trace.fragment.was_informed_by;
    assert_eq!(
        was_informed_by.len(),
        all_rows.len().saturating_sub(1),
        "replay must have {} wasInformedBy relations (consecutive pairs), got {}",
        all_rows.len().saturating_sub(1),
        was_informed_by.len()
    );
    // Each wasInformedBy must link two distinct activities.
    for wib in was_informed_by {
        assert_ne!(
            wib.informed_activity_id, wib.informing_activity_id,
            "wasInformedBy must link distinct activities"
        );
    }
}
