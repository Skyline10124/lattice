// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Production dispatch-surface matrix: one representative call per shipped
//! authorization vocabulary, through the production method registry
//! (`register_production_method_registry`) plus the embedded Component catalog.
//!
//! The retired legacy backend is not present; every call goes through a native
//! handler or an embedded Component.
//!
//! Two vocabularies coexist in one turn, and the matrix names both:
//! - Native methods carry a legacy `ImpactKind` (`external_observation` for
//!   `fetch.web`) and the `cep_native` semantic stage.
//! - Component Tools are authorized by Cell admission, carry no legacy impact
//!   kind (the audit column reads `"unknown"`), and are staged `cell_component`.
//!   Since #29 that is `session.todo_write`; since #30 `observe.workspace`;
//!   since #31 `mutate.create`/`mutate.edit`. No row here produces
//!   `workspace_observation` or a native `workspace_mutation` impact label.
//!
//! For each row:
//! 1. Execute the representative method via [`CepClient`].
//! 2. Assert the audit row: `impact_kind`, `semantic_stage`,
//!    `policy_enforced = true`, redacted metadata (no secret bodies).
//! 3. Verify that all rows share one `turn_id` before calling operator `replay_audit`.
//! 4. Assert PROV `activities` order and `wasInformedBy` chain from replay.

use std::sync::Arc;

use lancet_api::audit_db_path_for_project;
use lancet_api::{AuditReplayRequest, replay_audit};
use lancet_authority::policy::default::{DefaultPolicyHook, PolicyConfig, WorkspacePolicy};
use lancet_authority::{CepPolicyConfig, CepTurnContext};
use lancet_catalog::methods::external::{Fetch, FetchParams};
use lancet_native::{
    CepAuditStore, CepServerNativeExt, CepTransport, MethodRegistry, SEMANTIC_STAGE_NATIVE,
    ServerRunConfig, register_production_method_registry,
};
use lancet_protocol::effect_types::ImpactKind;

use tempfile::TempDir;

/// The migrated Component Tool (#29). Not a catalog constant: its descriptor
/// authority is `tools/todo-write/manifest.toml`.
const TODO_WRITE: &str = "session.todo_write";

/// The second migrated Component Tool (#30), whose descriptor authority is
/// `tools/observe-workspace/manifest.toml`. A literal for the same reason.
const OBSERVE_WORKSPACE: &str = "observe.workspace";

/// The Component Tools this test drives. Membership is what decides a row's
/// expected semantic stage and impact kind, so the two are never asserted from
/// a hardcoded method name.
const MUTATE_CREATE: &str = "mutate.create";
const MUTATE_EDIT: &str = "mutate.edit";
const COMPONENT_TOOLS: [&str; 4] = [OBSERVE_WORKSPACE, MUTATE_CREATE, MUTATE_EDIT, TODO_WRITE];

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
    // #30 made that grant load-bearing for observation too: Cell admission is
    // fail-closed, so `observe.workspace` missing from this set would be refused
    // with `capability_not_granted` before the workspace host is ever reached.
    config.components = Some(Arc::new(
        lancet_runtime::ComponentRuntime::from_embedded().expect("embedded composition"),
    ));
    policy.capability_admission = lancet_protocol::evidence::CapabilityAdmissionPolicy {
        enabled_tools: COMPONENT_TOOLS.iter().map(|n| (*n).to_string()).collect(),
        denied_capabilities: Default::default(),
        max_boundary: lancet_protocol::cell::Trust::In,
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

// ── The matrix test ────────────────────────────────────────────────────

/// The `five_production_classes` in the name predates the Tool migrations: the
/// matrix is four rows now — one native impact class (`fetch.web`) and three
/// Cell-governed Components (#29–#31). The name is kept because
/// `docs/TEST-TRIAGE.md` and `docs/test-triage-decisions.json` index this test
/// by it.
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

    // Row 1 — the observation Component: observe.workspace read_file. Called
    // untyped since #30 deleted the catalog types along with the native handler;
    // the wire shape is unchanged, which is what makes the migration invisible
    // to everything downstream of this call.
    let read_result: serde_json::Value = client1
        .call_raw(
            OBSERVE_WORKSPACE,
            serde_json::json!({ "mode": "read_file", "path": "src/dog04.txt" }),
        )
        .await
        .expect("observe.workspace read_file");
    assert_eq!(read_result["mode"], "read_file");
    let read_content = read_result["result"]["content"]
        .as_str()
        .expect("read_file returns content");
    assert!(
        read_content.contains("hello dog04"),
        "read_file returned unexpected content: {read_content:?}"
    );

    // Row 2 — mutate.edit Component (#31): Cell-governed write, no legacy ImpactKind.
    let edit_result = client1
        .call_raw("mutate.edit", serde_json::json!({"path": "src/dog04.txt", "expected_hash": file_hash, "edits": [{"op": "replace_text", "old": "hello dog04", "new": "hello dog04 patched"}]}))
        .await
        .expect("mutate.edit");
    assert_eq!(
        edit_result["changed_files"]
            .as_array()
            .expect("changed_files")
            .len(),
        1,
        "edit must change 1 file"
    );

    // Row 3 — ExternalObservation: fetch.web (localhost echo server)
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

    // Row 4 — session.todo_write Component (#29). Together with observe/mutate
    // Components and the one native fetch row, this proves the two vocabularies
    // coexist in one turn.
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
        // remaining native method by the policy lifecycle, the Components by Cell
        // admission. Both are policy-enforced.
        let expected_stage = if COMPONENT_TOOLS.contains(&row.method.as_str()) {
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

    // Keyed by method, not by impact kind: since #30 two of the four rows are
    // Component Tools and *both* record `"unknown"`, so an impact-keyed map
    // would silently collapse them and assert against whichever one landed last.
    let impact_by_method: std::collections::HashMap<&str, &str> = impact_rows
        .iter()
        .map(|r| (r.method.as_str(), r.impact_kind.as_str()))
        .collect();

    for (expected_method, expected_impact) in [
        ("fetch.web", "external_observation"),
        // A Component row carries no legacy impact kind — its authorization
        // vocabulary is the Cell (#28 B6). #30/#31 removed these from
        // `lancet_catalog::catalog()`, so `impact_kind_for_method` returns None.
        ("mutate.edit", "unknown"),
        (OBSERVE_WORKSPACE, "unknown"),
        (TODO_WRITE, "unknown"),
    ] {
        let impact = impact_by_method.get(expected_method).copied().unwrap_or("");
        assert_eq!(
            impact, expected_impact,
            "method={expected_method} must record impact_kind={expected_impact}"
        );
    }
    // There is deliberately no `workspace_observation` row. `observe.workspace`
    // was the only method that ever declared that kind, and since #30 it is a
    // Component; no native method declares it now, so a representative would
    // have to be invented and the matrix would stop describing production.
    assert!(
        !impact_by_method
            .values()
            .any(|impact| *impact == "workspace_observation"),
        "no shipped method declares workspace_observation any more: {impact_by_method:?}"
    );
    assert!(
        !impact_by_method
            .values()
            .any(|impact| *impact == "workspace_mutation"),
        "mutate.* are Components now — audit impact_kind must not claim native WorkspaceMutation: {impact_by_method:?}"
    );

    // Verify metadata fields for audit rows.
    for row in impact_rows {
        if COMPONENT_TOOLS.contains(&row.method.as_str()) {
            // A Component row's metadata points at the evidence table; it has no
            // legacy `required_capability`, and neither the todo text nor the
            // observed file content ever reaches it.
            let meta = row.metadata_json.as_deref().unwrap_or("{}");
            assert!(
                !meta.contains("dog04 impact matrix verified"),
                "audit metadata must not contain raw todo content"
            );
            assert!(
                !meta.contains("hello dog04"),
                "audit metadata must not contain observed file content"
            );
            let meta_val: serde_json::Value = serde_json::from_str(meta).unwrap_or_default();
            let evidence_id = meta_val["evidence_id"]
                .as_i64()
                .expect("a Component row points at its evidence");
            let evidence = audit
                .call_evidence(lancet_native::EvidenceId(evidence_id))
                .expect("evidence read")
                .expect("evidence row exists");
            assert_eq!(evidence.tool, row.method);
            // `session.todo_write` records only its result cell.
            // `observe.workspace` and `mutate.*` also record the host-side
            // operation cell, so they carry one more effect.
            let expected_effects = if row.method == TODO_WRITE { 1 } else { 2 };
            assert_eq!(
                evidence.effects.effects.len(),
                expected_effects,
                "unexpected effect count for {}: {:?}",
                row.method,
                evidence.effects.effects
            );
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

    // Only native rows can attribute a legacy ImpactKind in the PROV projection.
    // `WorkspaceObservation` left in #30; `WorkspaceMutation` left in #31 with
    // the native mutate handlers. The Component rows still carry an
    // `impact_kind` field, but it is a mandatory-field placeholder — asserted
    // via `impact_kind_attributed=false` below, not via this list.
    let activity_impact_kinds: Vec<&ImpactKind> =
        activities.iter().map(|a| &a.impact_kind).collect();
    assert!(
        activity_impact_kinds.contains(&&ImpactKind::ExternalObservation),
        "replay activities must include ExternalObservation"
    );
    assert!(
        !activity_impact_kinds.contains(&&ImpactKind::WorkspaceMutation),
        "mutate.* Components must not attribute WorkspaceMutation in PROV: {activity_impact_kinds:?}"
    );
    // Only the Components this matrix actually drives (mutate.create is enabled
    // for admission but not called here).
    for component in [OBSERVE_WORKSPACE, MUTATE_EDIT, TODO_WRITE] {
        let component_activity = activities
            .iter()
            .find(|a| a.method == component)
            .unwrap_or_else(|| panic!("the Component call {component} must appear in the trace"));
        assert_eq!(
            component_activity.attributes.get("impact_kind_attributed"),
            Some(&serde_json::Value::Bool(false)),
            "a Cell-governed activity must state that its impact kind is a placeholder: {component}"
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
