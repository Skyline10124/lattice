// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Production-native five-impact-class matrix.
//!
//! Exercises one representative CEP method per `ImpactKind` through the
//! production method registry (`register_production_method_registry`).
//!
//! The retired legacy backend is not present; all calls go through native handlers.
//!
//! For each impact class:
//! 1. Execute the representative method via [`CepClient`] typed calls.
//! 2. Assert the audit row: `impact_kind`, `semantic_stage = "cep_native"`,
//!    `policy_enforced = true`, redacted metadata (no secret bodies).
//! 3. Verify that all rows share one `turn_id` before calling operator `replay_audit`.
//! 4. Assert PROV `activities` order and `wasInformedBy` chain from replay.

use std::sync::Arc;

use lancet_api::audit_db_path_for_project;
use lancet_api::{AuditReplayRequest, replay_audit};
use lancet_authority::policy::default::{DefaultPolicyHook, PolicyConfig, WorkspacePolicy};
use lancet_authority::{CepPolicyConfig, CepTurnContext};
use lancet_catalog::methods::external::{Fetch, FetchParams};
use lancet_catalog::methods::workspace::{
    Edit, EditOperation, EditParams, ObserveWorkspace, ObserveWorkspaceParams,
    ObserveWorkspaceResult, ReadFileParams,
};
use lancet_native::{
    CepAuditStore, CepServerNativeExt, CepTransport, MethodRegistry, SEMANTIC_STAGE_NATIVE,
    ServerRunConfig, register_production_method_registry,
};
use lancet_protocol::effect_types::ImpactKind;

use tempfile::TempDir;

/// The migrated Component Tool (#29). Not a catalog constant: its descriptor
/// authority is `tools/todo-write/manifest.toml`.
const TODO_WRITE: &str = "session.todo_write";

const FIXED_TURN_ID: &str = "impact-matrix-turn-001";
const FIXED_SESSION_ID: &str = "impact-matrix-session-001";

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

/// Build a production server config with a fixed turn_id.
async fn build_production_config(
    canonical_root: &str,
    audit: Arc<CepAuditStore>,
    allow_localhost: bool,
) -> ServerRunConfig {
    let mut policy = CepPolicyConfig::defaults_for_project_root(canonical_root);
    if allow_localhost {
        policy.network.allow_localhost = true;
        policy.network.allowed_hosts = vec!["127.0.0.1".into(), "localhost".into()];
    }

    let mut policy_cfg =
        PolicyConfig::defaults_for_project_root(std::path::Path::new(canonical_root).to_path_buf());
    // Allow all writes under the project root (empty deny list).
    policy_cfg.workspace = WorkspacePolicy {
        denied_write_patterns: vec![],
        ..Default::default()
    };
    if allow_localhost {
        policy_cfg.network.allow_localhost = true;
        policy_cfg.network.allowed_hosts = vec!["127.0.0.1".into(), "localhost".into()];
    }
    let hook = Arc::new(DefaultPolicyHook::new(policy_cfg));

    let mut config = ServerRunConfig::in_memory_for_tests();
    config.audit = audit;
    // #29: the production dispatch surface is native handlers plus the embedded
    // Component catalog, and `enabled_tools` is the stage-1 capability grant.
    config.components = Some(Arc::new(
        lancet_runtime::ComponentRuntime::from_embedded().expect("embedded composition"),
    ));
    policy.capability_admission = lancet_protocol::evidence::CapabilityAdmissionPolicy {
        enabled_tools: std::iter::once(TODO_WRITE.to_string()).collect(),
        ..Default::default()
    };
    config.cep_context = Arc::new(CepTurnContext::from_policy(policy));
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

    // Create initial file for read and edit operations.
    std::fs::create_dir_all(root.join("src")).expect("src dir");
    let seed_content = "hello dog04\n";
    std::fs::write(root.join("src/dog04.txt"), seed_content).expect("seed file");

    let canonical = std::fs::canonicalize(root).expect("canonicalize root");
    let canonical_str = canonical.to_str().unwrap();

    // Content hash used by mutate.edit; line anchors are computed by LANCET.
    let file_hash = format!("blake3:{}", blake3::hash(seed_content.as_bytes()).to_hex());

    // Holds the shared env lock and redirects `XDG_STATE_HOME` for the rest of
    // this test, restoring the previous value on drop.
    let _state = lancet_audit::ScopedXdgStateHome::new();
    let audit_path = audit_db_path_for_project(&canonical).expect("audit path");
    std::fs::create_dir_all(audit_path.parent().unwrap()).expect("audit parent");
    let audit = Arc::new(CepAuditStore::open(&audit_path).expect("audit store"));

    // ── Step 1: execute the five impact classes ────────────────────────

    let mut registry = MethodRegistry::new();
    register_production_method_registry(&mut registry).expect("production registry");

    let fetch_url = spawn_echo_server("200 OK", "dog04-fetch-body").await;

    let config = build_production_config(canonical_str, Arc::clone(&audit), true).await;
    let (client1, server1) = CepTransport::pair(32);
    let server_task1 = tokio::spawn(async move {
        let _ = server1.run_native(registry, config).await;
    });
    tokio::task::yield_now().await;

    // Row 1 — WorkspaceObservation: observe.workspace read_file
    let read_result = client1
        .call::<ObserveWorkspace>(ObserveWorkspaceParams::ReadFile(ReadFileParams {
            path: "src/dog04.txt".into(),
            offset: None,
            limit: None,
            line_numbers: None,
        }))
        .await
        .expect("observe.workspace read_file");
    let ObserveWorkspaceResult::ReadFile(read_result) = read_result else {
        panic!("observe.workspace returned non-read_file result");
    };
    assert!(
        read_result.content.contains("hello dog04"),
        "read_file returned unexpected content: {:?}",
        read_result.content
    );

    // Row 2 — WorkspaceMutation: mutate.edit
    let edit_result = client1
        .call::<Edit>(EditParams {
            path: "src/dog04.txt".into(),
            expected_hash: file_hash,
            edits: vec![EditOperation::ReplaceText {
                old: "hello dog04".into(),
                new: "hello dog04 patched".into(),
                occurrence: None,
            }],
        })
        .await
        .expect("mutate.edit");
    assert_eq!(
        edit_result.changed_files.len(),
        1,
        "edit must change 1 file"
    );

    // Row 4 — ExternalObservation: fetch.web (localhost echo server)
    let fetch_result = client1
        .call::<Fetch>(FetchParams {
            url: fetch_url.clone(),
            headers: Default::default(),
        })
        .await
        .expect("fetch.web");
    assert_eq!(fetch_result.status, 200);
    assert_eq!(
        fetch_result.body, "dog04-fetch-body",
        "fetch.web body must match echo server"
    );

    // Row 4 — the migrated Component Tool. Since #29 `session.todo_write` is
    // Cell-governed rather than a legacy impact class, so this row is what proves
    // the two vocabularies coexist in one turn.
    let todo_result: serde_json::Value = client1
        .call_named(
            "session.todo_write",
            serde_json::json!({
                "todos": [{"content": "dog04 impact matrix verified", "status": "completed"}]
            }),
        )
        .await
        .expect("session.todo_write");
    assert_eq!(
        todo_result["todos"].as_array().map(|t| t.len()),
        Some(1),
        "the Component echoes the validated plan"
    );

    // Shut down first server.
    drop(client1);
    server_task1.abort();
    tokio::task::yield_now().await;

    // ── Step 2: assert audit rows ──────────────────────────────────────

    // All rows share the fixed turn_id.
    let all_rows = audit
        .query_rows(&lancet_native::AuditQueryFilters {
            session_id: Some(FIXED_SESSION_ID.to_string()),
            ..Default::default()
        })
        .expect("query audit rows");

    assert!(
        !all_rows.is_empty(),
        "audit store must have rows after four impact classes"
    );

    for row in &all_rows {
        assert_eq!(
            row.turn_id, FIXED_TURN_ID,
            "all rows must share turn_id={FIXED_TURN_ID}"
        );
        // Each row is labeled by the vocabulary that actually authorized it: the
        // three native methods by the policy lifecycle, the Component by Cell
        // admission. Both are policy-enforced.
        let expected_stage = if row.method == TODO_WRITE {
            "cell_component"
        } else {
            SEMANTIC_STAGE_NATIVE
        };
        assert_eq!(
            row.semantic_stage, expected_stage,
            "row {} ({}) has the wrong semantic stage",
            row.audit_id, row.method
        );
        assert!(
            row.policy_enforced,
            "row {} ({}) must be policy_enforced",
            row.audit_id, row.method
        );
    }

    // The four representative impact-class rows.
    let impact_rows = &all_rows;

    assert_eq!(
        impact_rows.len(),
        4,
        "must have exactly 4 rows, got {}: {:?}",
        impact_rows.len(),
        impact_rows.iter().map(|r| &r.method).collect::<Vec<_>>()
    );

    // Verify the four impact kinds are present.
    let methods_by_impact: std::collections::HashMap<&str, &str> = impact_rows
        .iter()
        .map(|r| (r.impact_kind.as_str(), r.method.as_str()))
        .collect();

    for (expected_impact, expected_method) in [
        ("workspace_observation", "observe.workspace"),
        ("workspace_mutation", "mutate.edit"),
        ("external_observation", "fetch.web"),
        // The Component row carries no legacy impact kind — its authorization
        // vocabulary is the Cell (#28 B6).
        ("unknown", TODO_WRITE),
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
    for row in impact_rows {
        if row.method == TODO_WRITE {
            // A Component row's metadata points at the evidence table; it has no
            // legacy `required_capability`, and the todo text never reaches it.
            let meta = row.metadata_json.as_deref().unwrap_or("{}");
            assert!(
                !meta.contains("dog04 impact matrix verified"),
                "audit metadata must not contain raw todo content"
            );
            let meta_val: serde_json::Value = serde_json::from_str(meta).unwrap_or_default();
            let evidence_id = meta_val["evidence_id"]
                .as_i64()
                .expect("a Component row points at its evidence");
            let evidence = audit
                .call_evidence(lancet_native::EvidenceId(evidence_id))
                .expect("evidence read")
                .expect("evidence row exists");
            assert_eq!(evidence.tool, TODO_WRITE);
            assert_eq!(evidence.effects.effects.len(), 1);
            continue;
        }
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

    // ── Step 3: operator replay_audit PROV assertions ───────────────────

    let replay_result = replay_audit(AuditReplayRequest::new(
        canonical.as_path(),
        FIXED_SESSION_ID.to_string(),
        FIXED_TURN_ID.to_string(),
    ))
    .await
    .expect("replay_audit");

    // The trace covers all 5 impact rows.
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

    // Verify the three legacy impact classes appear in the trace. The Component
    // row is not among them: its authorization vocabulary is the Cell, so the
    // projection marks its `impact_kind` unattributed rather than coercing the
    // `unknown` column into a concrete kind (#28 B6).
    let activity_impact_kinds: Vec<&ImpactKind> =
        activities.iter().map(|a| &a.impact_kind).collect();
    for expected in [
        ImpactKind::WorkspaceObservation,
        ImpactKind::WorkspaceMutation,
        ImpactKind::ExternalObservation,
    ] {
        assert!(
            activity_impact_kinds.contains(&&expected),
            "replay activities must include {:?}",
            expected
        );
    }
    let component_activity = activities
        .iter()
        .find(|a| a.method == TODO_WRITE)
        .expect("the Component call must appear in the trace");
    assert_eq!(
        component_activity.attributes.get("impact_kind_attributed"),
        Some(&serde_json::Value::Bool(false)),
        "a Cell-governed activity must state that its impact kind is a placeholder"
    );

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
