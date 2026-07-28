// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! #27 (Codeberg #5) slice D.6: end-to-end Component dispatch round-trip
//! through the existing native CEP server lifecycle.
//!
//! These tests prove the D.3 dispatch split works: a no-op Component Tool
//! (`noop.echo`) round-trips through the same `CepServer`/audit lifecycle
//! as the 5 production native handlers, producing Cell-attributed audit
//! evidence. The 5 native handlers continue to work unchanged alongside
//! the Component path.
//!
//! The fixed no-op fixture comes from `lancet_runtime::ComponentRuntime::noop_fixture()`
//! (`test-utils` feature). The caller cannot inject bytes, registrations,
//! handlers, paths, or an approval set.
//!
//! #28 B11: the same lifecycle also carries the Cell admission end-to-end
//! scenarios — allow, partial deny, and full deny — with their audit evidence.

use std::collections::BTreeSet;
use std::sync::Arc;

use lancet_authority::CepTurnContext;
use lancet_native::{
    AuditQueryFilters, CepAuditStore, CepServerNativeExt, CepTransport, EvidenceId, EvidenceLookup,
    EvidenceOutcome, MethodRegistry, ServerRunConfig, StoredCallEvidence,
    register_production_method_registry,
};
use lancet_protocol::cell::CtxOp;
use lancet_protocol::effect_types::ImpactKind;
use lancet_protocol::evidence::{EffectSource, ObservedTarget};
use lancet_protocol::{CapabilityAdmissionPolicy, CepErrorCode, Trust};
use lancet_runtime::ComponentRuntime;

/// The noop fixture's admission policy: the tool is enabled, and `deny`
/// subtracts capability names by the profile's stage-2 rule.
fn admission(deny: &[&str]) -> CapabilityAdmissionPolicy {
    CapabilityAdmissionPolicy {
        enabled_tools: BTreeSet::from(["noop.echo".to_string()]),
        denied_capabilities: deny.iter().map(|name| (*name).to_string()).collect(),
        max_boundary: Trust::In,
    }
}

/// Spawn a native CEP server with the no-op Component fixture composed in,
/// plus the production native method registry. Returns the client, audit
/// store, and the server task handle.
async fn spawn_native_with_noop() -> (
    lancet_native::CepClient,
    Arc<CepAuditStore>,
    tokio::task::JoinHandle<()>,
) {
    spawn_native_with_admission(admission(&[])).await
}

/// As above, under an explicit admission policy. The fail-closed default
/// admits no tool at all, so every Component scenario names its policy.
async fn spawn_native_with_admission(
    policy: CapabilityAdmissionPolicy,
) -> (
    lancet_native::CepClient,
    Arc<CepAuditStore>,
    tokio::task::JoinHandle<()>,
) {
    spawn_native_with_policies(policy, BTreeSet::new(), None).await
}

/// One server run carrying **both** vocabularies' policies: the Cell
/// admission policy and the legacy `[lancet.effects]` deny list.
async fn spawn_native_with_policies(
    policy: CapabilityAdmissionPolicy,
    denied_effect_kinds: BTreeSet<ImpactKind>,
    project_root: Option<&std::path::Path>,
) -> (
    lancet_native::CepClient,
    Arc<CepAuditStore>,
    tokio::task::JoinHandle<()>,
) {
    let components =
        Arc::new(ComponentRuntime::noop_fixture().expect("closed no-op test composition"));
    let mut config = ServerRunConfig::in_memory_for_tests().with_capability_admission(policy);
    // Rebuilding the turn policy for a real project root must not drop the Cell
    // admission policy `with_capability_admission` just installed — that field
    // lives on the same struct.
    let admission = config.cep_context.policy.capability_admission.clone();
    let mut turn_policy = match project_root {
        Some(root) => lancet_authority::policy::CepPolicyConfig::defaults_for_project_root(root),
        None => config.cep_context.policy.clone(),
    };
    turn_policy.capability_admission = admission;
    turn_policy.denied_effect_kinds = denied_effect_kinds;
    config.cep_context = Arc::new(CepTurnContext::from_policy(turn_policy));
    config.components = Some(components);

    let mut registry = MethodRegistry::new();
    register_production_method_registry(&mut registry).unwrap();

    let (client, server) = CepTransport::pair(64);
    let audit = Arc::clone(&config.audit);
    let task = tokio::spawn(async move {
        let _ = server.run_native(registry, config).await;
    });
    tokio::task::yield_now().await;
    (client, audit, task)
}

/// The single `noop.echo` audit row.
async fn noop_row(audit: &CepAuditStore) -> lancet_native::audit::AuditRow {
    audit.await_pending().await;
    let mut rows = audit
        .query_rows(&AuditQueryFilters {
            method: Some("noop.echo".to_string()),
            ..Default::default()
        })
        .expect("audit query");
    assert_eq!(rows.len(), 1, "exactly one noop.echo audit row");
    rows.remove(0)
}

/// The single `noop.echo` audit row's metadata, parsed.
async fn noop_metadata(audit: &CepAuditStore) -> serde_json::Value {
    let row = noop_row(audit).await;
    let metadata = row
        .metadata_json
        .as_deref()
        .expect("a Component call always finalizes with evidence metadata");
    serde_json::from_str(metadata).expect("metadata is JSON")
}

/// The `noop.echo` call's Cell evidence, read from the versioned evidence table
/// through the `evidence_id` its audit row points at (#112).
///
/// Going through the pointer is the point: it proves the row resolves to real
/// evidence rather than that the store happens to hold a matching record.
async fn noop_evidence(audit: &CepAuditStore) -> StoredCallEvidence {
    let metadata = noop_metadata(audit).await;
    let evidence_id = metadata["evidence_id"]
        .as_i64()
        .unwrap_or_else(|| panic!("a Component row must point at its evidence: {metadata}"));
    audit
        .call_evidence(EvidenceId(evidence_id))
        .expect("evidence read")
        .expect("the pointer must resolve to a persisted evidence row")
}

/// D.6 tracer + #28 B11 allow path: `noop.echo` round-trips through CEP +
/// audit with both capabilities admitted, so one call produces two effects
/// (the gated `emit-event` operation and the result) and the audit row keeps
/// "why authorized" apart from "what happened".
#[tokio::test]
async fn embedded_noop_round_trips_through_cep_and_audit() {
    let (client, audit, _task) = spawn_native_with_noop().await;

    let value: serde_json::Value = client
        .call_named("noop.echo", serde_json::json!({ "message": "ping" }))
        .await
        .expect("component round trip");
    assert_eq!(value, serde_json::json!({ "echoed": "ping" }));

    audit.await_pending().await;
    let rows = audit
        .query_rows(&AuditQueryFilters {
            method: Some("noop.echo".to_string()),
            ..Default::default()
        })
        .expect("audit query");
    assert_eq!(rows.len(), 1, "exactly one noop.echo audit row");
    let row = &rows[0];
    let result = row.result_or_error.as_deref().unwrap_or("");
    assert!(
        result.starts_with("success:"),
        "audit outcome must be success, got: {result}"
    );

    let metadata: serde_json::Value =
        serde_json::from_str(row.metadata_json.as_deref().expect("evidence metadata"))
            .expect("metadata is JSON");
    // C8: a multi-effect call keeps every cell, in seq order. This stays a
    // query key on the row (#112).
    assert_eq!(
        metadata["component_cells"],
        serde_json::json!(["mutate:none:session:in", "alloc:none:session:in"])
    );
    // #112 criterion 3: the records are no longer in the blob.
    for key in [
        "admission_evidence",
        "observed_effects",
        "denied_operations",
    ] {
        assert!(
            metadata.get(key).is_none(),
            "{key} belongs to the evidence table, not the audit row: {metadata}"
        );
    }

    let evidence = noop_evidence(&audit).await;
    assert_eq!(evidence.tool, "noop.echo");
    assert_eq!(evidence.turn_id, "test-turn");
    assert_eq!(evidence.outcome, EvidenceOutcome::Completed);
    // B5/A2: approved capability names are admission evidence only.
    assert_eq!(
        evidence.admission.approved_capabilities,
        vec!["session.events".to_string(), "session.result".to_string()]
    );
    assert_eq!(evidence.admission.max_boundary, Trust::In);
    let effects = &evidence.effects.effects;
    assert_eq!(effects.len(), 2, "emit-event operation + result");
    assert_eq!(effects[0].source, EffectSource::Operation);
    assert_eq!(
        effects[0].target,
        ObservedTarget::Session {
            turn_id: "test-turn".into()
        }
    );
    assert_eq!(effects[0].context_measure, None);
    assert_eq!(effects[1].source, EffectSource::Result);
    // D12: the host measures the canonical result bytes itself.
    assert_eq!(
        effects[1].context_measure,
        Some(r#"{"echoed":"ping"}"#.len() as u64)
    );
    assert!(
        evidence.denied.denied.is_empty(),
        "nothing was refused on the allow path: {:?}",
        evidence.denied
    );

    // #112 criterion 1: the same evidence is reachable by turn and by call.
    let by_turn = audit.call_evidence_for_turn("test-turn").expect("by turn");
    assert_eq!(by_turn.len(), 1);
    assert_eq!(by_turn[0].evidence_id, evidence.evidence_id);
    let by_call = audit
        .call_evidence_for_call(&row.session_id, "test-turn", row.call_id)
        .expect("by call");
    assert_eq!(by_call.len(), 1);
    assert_eq!(by_call[0].evidence_id, evidence.evidence_id);
}

/// B11 partial deny: subtracting `session.events` leaves the tool callable,
/// but its `emit-event` operation is refused. The call still succeeds, one
/// effect (the result) is recorded, and the refusal is recorded as evidence.
#[tokio::test]
async fn partial_admission_denies_only_the_uncovered_operation() {
    let (client, audit, _task) = spawn_native_with_admission(admission(&["session.events"])).await;

    let events = client.events();
    let value: serde_json::Value = client
        .call_named("noop.echo", serde_json::json!({ "message": "partial" }))
        .await
        .expect("the tool stays callable with one capability");
    assert_eq!(value, serde_json::json!({ "echoed": "partial" }));

    // B3: the host facility is never reached, so no tool event is emitted.
    let mut guard = events.lock().await;
    while let Ok(event) = guard.try_recv() {
        assert_ne!(
            event.method, "tool.event",
            "a denied operation must not reach the event sink"
        );
    }
    drop(guard);

    let metadata = noop_metadata(&audit).await;
    assert_eq!(
        metadata["component_cells"],
        serde_json::json!(["alloc:none:session:in"])
    );

    let evidence = noop_evidence(&audit).await;
    assert_eq!(
        evidence.admission.approved_capabilities,
        vec!["session.result".to_string()]
    );
    assert_eq!(
        evidence.admission.denied_capabilities,
        vec!["session.events".to_string()]
    );
    // The call succeeded even though one operation was refused.
    assert_eq!(evidence.outcome, EvidenceOutcome::Completed);
    assert_eq!(evidence.denied.version, 1);
    let denied = &evidence.denied.denied;
    assert_eq!(denied.len(), 1);
    assert_eq!(denied[0].cell.ctx_op, CtxOp::Mutate);
    assert!(
        denied[0]
            .reason
            .contains("not covered by an admitted capability"),
        "denial reason must name the uncovered cell: {}",
        denied[0].reason
    );
}

/// B11 full deny, stage 1: a tool outside `[tools].enabled` has no capability
/// at all. Wasmtime is never entered, the client sees `CapabilityNotGranted`,
/// and the audit row still carries the admission evidence that explains it.
#[tokio::test]
async fn unenabled_tool_is_denied_before_the_runtime_is_entered() {
    let (client, audit, _task) =
        spawn_native_with_admission(CapabilityAdmissionPolicy::default()).await;

    let events = client.events();
    let err = client
        .call_raw("noop.echo", serde_json::json!({ "message": "denied" }))
        .await
        .expect_err("the fail-closed default enables no tool");
    assert_eq!(
        err.code,
        lancet_protocol::CepErrorCode::CapabilityNotGranted
    );
    assert!(
        err.message.contains("noop.echo") && err.message.contains("not enabled"),
        "the error must name the tool and the refusing stage: {}",
        err.message
    );

    let mut guard = events.lock().await;
    while let Ok(event) = guard.try_recv() {
        assert_ne!(
            event.method, "tool.event",
            "a denied call must never instantiate the Component"
        );
    }
    drop(guard);

    let metadata = noop_metadata(&audit).await;
    assert!(
        metadata.get("component_cells").is_none(),
        "a call that never ran produced no effect: {metadata}"
    );

    // #112 criterion 5: the refusal is evidence, with its own outcome label.
    let evidence = noop_evidence(&audit).await;
    assert_eq!(evidence.tool, "noop.echo");
    assert_eq!(evidence.outcome, EvidenceOutcome::AdmissionDenied);
    assert!(evidence.admission.approved_capabilities.is_empty());
    assert!(evidence.effects.effects.is_empty());
}

/// B11 full deny, effect ceiling: with `session.result` subtracted the tool is
/// still admitted for its event capability, but its declared result cell is
/// outside the admitted set. The result fails closed with `EffectViolation`
/// while the operation effect already recorded survives into audit (B5).
#[tokio::test]
async fn denied_result_capability_fails_the_call_and_keeps_operation_evidence() {
    let (client, audit, _task) = spawn_native_with_admission(admission(&["session.result"])).await;

    let err = client
        .call_raw("noop.echo", serde_json::json!({ "message": "ceiling" }))
        .await
        .expect_err("an unadmitted result cell fails the call");
    assert_eq!(err.code, lancet_protocol::CepErrorCode::EffectViolation);

    let metadata = noop_metadata(&audit).await;
    assert_eq!(
        metadata["component_cells"],
        serde_json::json!(["mutate:none:session:in"]),
        "the emit-event effect survives the result-cell violation"
    );

    // #112 criterion 5: a failed call retains the operation effects it already
    // recorded, and is labeled as a failure.
    let evidence = noop_evidence(&audit).await;
    assert_eq!(
        evidence.admission.approved_capabilities,
        vec!["session.events".to_string()]
    );
    assert_eq!(evidence.outcome, EvidenceOutcome::Failed);
    assert_eq!(evidence.effects.effects.len(), 1);
    assert_eq!(evidence.effects.effects[0].source, EffectSource::Operation);
}

/// #28 b8: a Component row is labeled by the vocabulary that actually governed
/// it. The legacy catalog lookup would call `noop.echo` `unrouted` and claim no
/// policy ran, and its refusal `method_not_found` — the tool is in the closed
/// Component catalog and Cell admission is what decided.
#[tokio::test]
async fn component_rows_are_labeled_cell_governed_not_unrouted() {
    let (client, audit, _task) = spawn_native_with_noop().await;
    let _: serde_json::Value = client
        .call_named("noop.echo", serde_json::json!({ "message": "labels" }))
        .await
        .expect("component round trip");

    let row = noop_row(&audit).await;
    assert_eq!(row.semantic_stage, "cell_component");
    assert_eq!(row.guard_stage, "cell_admission");
    assert!(
        row.policy_enforced,
        "Cell admission ran and allowed the call"
    );
    assert_eq!(row.semantic_status.as_deref(), Some("component_handler"));

    // An admission denial is a policy denial, not a missing method.
    let (client, audit, _task) =
        spawn_native_with_admission(CapabilityAdmissionPolicy::default()).await;
    client
        .call_raw("noop.echo", serde_json::json!({ "message": "denied" }))
        .await
        .expect_err("the fail-closed default enables no tool");
    let denied = noop_row(&audit).await;
    assert_eq!(denied.semantic_status.as_deref(), Some("component_denied"));
    assert!(
        denied.policy_enforced,
        "the denial is itself the policy decision"
    );

    // Operator "denied" queries still reach it.
    let denied_rows = audit
        .query_rows(&AuditQueryFilters {
            verdict: Some("denied".into()),
            ..Default::default()
        })
        .expect("audit query");
    assert_eq!(denied_rows.len(), 1);
    assert_eq!(denied_rows[0].method, "noop.echo");
}

/// #28 b22 / A2 / B11: a real partial-deny call carries its denial all the way
/// into the PROV activity attributes and the v2 attestation predicate — no
/// hand-built audit row anywhere in this path.
#[tokio::test]
async fn real_denial_reaches_prov_attributes_and_the_v2_predicate() {
    use lancet_native::build_in_toto_statement_with_evidence;
    use lancet_native::provenance::project_rows_with_evidence;
    use lancet_protocol::provenance::CEP_TURN_ATTESTATION_PREDICATE_TYPE_V2;

    let (client, audit, _task) = spawn_native_with_admission(admission(&["session.events"])).await;
    let _: serde_json::Value = client
        .call_named("noop.echo", serde_json::json!({ "message": "denied-op" }))
        .await
        .expect("the tool stays callable with one capability");
    audit.await_pending().await;

    let rows = audit
        .query_rows(&AuditQueryFilters {
            turn_id: Some("test-turn".to_string()),
            ..Default::default()
        })
        .expect("audit query");

    // #112: the projection resolves the row's `evidence_id` through the store.
    let resolve = |id: EvidenceId| audit.call_evidence(id).ok().flatten();
    let lookup = EvidenceLookup::new(&resolve);
    let attributes = &project_rows_with_evidence(&rows, &lookup).activities[0].attributes;
    let denied = attributes["denied_operations"]["denied"]
        .as_array()
        .expect("the denial reaches the PROV activity attributes");
    assert_eq!(denied.len(), 1);
    assert_eq!(denied[0]["cell"]["ctx_op"], "mutate");
    assert_eq!(denied[0]["target"]["turn_id"], "test-turn");
    let reason = denied[0]["reason"].as_str().expect("denial reason");
    assert!(
        reason.contains("not covered by an admitted capability"),
        "the projected reason must be the one the host recorded: {reason}"
    );

    let statement =
        build_in_toto_statement_with_evidence("test-turn", &rows, "lattice-test/1", &lookup);
    assert_eq!(
        statement.predicate_type,
        CEP_TURN_ATTESTATION_PREDICATE_TYPE_V2
    );
    let evidence = &statement.predicate["evidence"][0];
    assert_eq!(evidence["denied_operations"]["denied"][0]["reason"], reason);
    assert_eq!(
        evidence["admission"]["denied_capabilities"],
        serde_json::json!(["session.events"]),
        "'why authorized' stays a field of its own beside the denial"
    );
}

/// C2 at the server-construction seam: a deny entry naming no composed
/// capability is a startup error, so the run never begins.
#[tokio::test]
async fn unknown_deny_name_fails_server_construction() {
    let components =
        Arc::new(ComponentRuntime::noop_fixture().expect("closed no-op test composition"));
    let mut config = ServerRunConfig::in_memory_for_tests()
        .with_capability_admission(admission(&["session.tyop"]));
    config.components = Some(components);

    let (_client, server) = CepTransport::pair(64);
    let err = server
        .run_native(MethodRegistry::new(), config)
        .await
        .expect_err("an unknown deny name must refuse startup");
    assert!(
        err.message.contains("session.tyop"),
        "the startup error must name the unknown deny entry: {}",
        err.message
    );
}

/// D.6: a native method (`observe.workspace`) still works alongside the
/// Component path — the dispatch split doesn't break the remaining handlers.
#[tokio::test]
async fn native_method_still_works_alongside_component() {
    use lancet_catalog::methods::workspace::{ObserveWorkspaceParams, ReadFileParams};

    let dir = tempfile::TempDir::new().expect("temp project");
    std::fs::write(dir.path().join("probe.txt"), "coexist\n").expect("seed file");
    let (client, _audit, _task) = spawn_native_with_project_root(dir.path()).await;

    let result: serde_json::Value = client
        .call_named(
            "observe.workspace",
            ObserveWorkspaceParams::ReadFile(ReadFileParams {
                path: "probe.txt".into(),
                offset: None,
                limit: None,
                line_numbers: None,
            }),
        )
        .await
        .expect("native method still works");

    assert!(
        result.to_string().contains("coexist"),
        "the native read must return the file content: {result}"
    );
}

/// #29.6: there is no native `session.todo_write` handler and no fallback.
///
/// This server run has the production native registry but a Component catalog
/// that holds only the no-op fixture, so the migrated method resolves nowhere.
/// It must fail as an unrouted method — not quietly succeed through a native
/// handler that survived the migration.
#[tokio::test]
async fn migrated_todo_write_has_no_native_fallback() {
    let (client, audit, _task) = spawn_native_with_noop().await;

    let err = client
        .call_raw("session.todo_write", todo_params())
        .await
        .expect_err("no native handler may serve the migrated method");
    // `method_not_found` is carried on the wire as JSON-RPC -32601; the
    // structured code it maps to is `SchemaMismatch`.
    assert_eq!(
        lancet_protocol::error::to_jsonrpc_error(&err).0,
        -32601,
        "a migrated method with no embedded Component must be unrouted, not served: {err:?}"
    );

    audit.await_pending().await;
    let rows = audit
        .query_rows(&AuditQueryFilters {
            method: Some("session.todo_write".to_string()),
            ..Default::default()
        })
        .expect("audit query");
    assert_eq!(rows.len(), 1, "the refusal is still audited");
    assert_eq!(
        rows[0].semantic_stage, "unrouted",
        "no native handler and no Component: the row must say unrouted"
    );
    assert!(
        !rows[0].policy_enforced,
        "an unrouted call must never claim policy enforcement"
    );
}

/// Every legacy `ImpactKind`. Cross-checked against the native catalog by the
/// isolation tests, so a narrowed fixture cannot quietly leave a native method
/// authorized and make the Component leg look isolated when it is not.
fn all_impact_kinds() -> BTreeSet<ImpactKind> {
    BTreeSet::from([
        ImpactKind::WorkspaceObservation,
        ImpactKind::WorkspaceMutation,
        ImpactKind::CommandExecution,
        ImpactKind::ExternalObservation,
        ImpactKind::SessionStateMutation,
        ImpactKind::AuditObservation,
    ])
}

fn todo_params() -> serde_json::Value {
    serde_json::json!({"todos": [{"content": "isolation", "status": "pending"}]})
}

/// Params for a legal `observe.workspace` read of the seeded `probe.txt`. The
/// vocabulary-isolation tests need a *native* method, and this is the cheapest
/// one that reaches its handler.
fn read_probe_params() -> serde_json::Value {
    serde_json::json!({"mode": "read_file", "path": "probe.txt"})
}

/// As [`spawn_native_with_noop`], with the Local Workspace rooted at
/// `project_root` so a native observation has a real file to read.
async fn spawn_native_with_project_root(
    project_root: &std::path::Path,
) -> (
    lancet_native::CepClient,
    Arc<CepAuditStore>,
    tokio::task::JoinHandle<()>,
) {
    let components =
        Arc::new(ComponentRuntime::noop_fixture().expect("closed no-op test composition"));
    let mut config =
        ServerRunConfig::in_memory_for_tests().with_capability_admission(admission(&[]));
    config.cep_context = Arc::new(CepTurnContext::defaults_for_project_root(project_root));
    config.components = Some(components);

    let mut registry = MethodRegistry::new();
    register_production_method_registry(&mut registry).unwrap();

    let (client, server) = CepTransport::pair(64);
    let audit = Arc::clone(&config.audit);
    let task = tokio::spawn(async move {
        let _ = server.run_native(registry, config).await;
    });
    tokio::task::yield_now().await;
    (client, audit, task)
}

/// B8/B9/A4: the two authorization vocabularies never cross-authorize.
///
/// One server run, one turn policy: every legacy `ImpactKind` denied by
/// `[lancet.effects]`, every Cell capability admitted. The native method is
/// refused by that deny list and the Component call still succeeds in the
/// same run — proving the legacy vocabulary is not a second authority over
/// Cell admission. A refactor that routed Component dispatch through the
/// `lancet_catalog::catalog()` lookup would kill the Component leg here.
#[tokio::test]
async fn legacy_effect_deny_refuses_native_methods_and_never_component_calls() {
    let denied = all_impact_kinds();
    assert!(
        lancet_catalog::catalog().iter().all(|descriptor| descriptor
            .effect
            .kinds
            .0
            .iter()
            .all(|k| denied.contains(k))),
        "the fixture must deny every kind the native catalog declares"
    );

    let dir = tempfile::TempDir::new().expect("temp project");
    std::fs::write(dir.path().join("probe.txt"), "x\n").expect("seed file");
    let (client, audit, _task) =
        spawn_native_with_policies(admission(&[]), denied, Some(dir.path())).await;

    // The native leg is `observe.workspace`: `session.todo_write` is a Component
    // Tool since #29, so using it here would test the Component path twice and
    // prove nothing about the legacy vocabulary's reach.
    let err = client
        .call_raw("observe.workspace", read_probe_params())
        .await
        .expect_err("every declared impact kind is denied");
    assert_eq!(err.code, CepErrorCode::EffectViolation);

    let value = client
        .call_raw("noop.echo", serde_json::json!({"message": "isolated"}))
        .await
        .expect("the legacy deny list has no authority over a Cell-admitted Tool");
    assert_eq!(value, serde_json::json!({"echoed": "isolated"}));

    // The Component call still has its own evidence: it went through Cell
    // admission, not through a legacy allowance.
    let evidence = noop_evidence(&audit).await;
    assert_eq!(
        evidence.admission.approved_capabilities,
        vec!["session.events".to_string(), "session.result".to_string()]
    );
    audit.await_pending().await;
}

/// The converse leg: a Cell admission policy that admits nothing refuses the
/// Component Tool and leaves the native methods untouched. `[lancet
/// .capabilities]` is not a second authority over the legacy path either.
#[tokio::test]
async fn cell_admission_denial_never_refuses_a_native_method() {
    let dir = tempfile::TempDir::new().expect("temp project");
    std::fs::write(dir.path().join("probe.txt"), "x\n").expect("seed file");
    let (client, _audit, _task) = spawn_native_with_policies(
        CapabilityAdmissionPolicy::default(),
        BTreeSet::new(),
        Some(dir.path()),
    )
    .await;

    let err = client
        .call_raw("noop.echo", serde_json::json!({"message": "refused"}))
        .await
        .expect_err("no tool is enabled");
    assert_eq!(err.code, CepErrorCode::CapabilityNotGranted);

    let result = client
        .call_raw("observe.workspace", read_probe_params())
        .await
        .expect("Cell admission has no authority over a native method");
    assert!(
        result.to_string().contains('x'),
        "the native read must return content: {result}"
    );
}

/// D.6: the Component call emits events through the CEP event stream.
/// The no-op guest calls `host_control::emit_event("{\"phase\":\"probed\"}")`,
/// which the host forwards as a `tool.event` CEP event. The event must carry
/// the guest's `phase: probed` payload and a `call_id` consistent with the
/// CEP call started/finished events.
#[tokio::test]
async fn component_call_emits_events_with_consistent_call_id() {
    let (client, _audit, _task) = spawn_native_with_noop().await;

    let events = client.events();
    let _value = client
        .call_raw("noop.echo", serde_json::json!({ "message": "events" }))
        .await
        .expect("call succeeds");

    // Drain events. Expect: cep.call_started, tool.event (phase=probed),
    // cep.call_finished — all with the same call_id.
    let mut started_id: Option<u64> = None;
    let mut found_probed = false;
    let mut finished_id: Option<u64> = None;
    let mut guard = events.lock().await;
    while let Ok(event) = guard.try_recv() {
        let cid = event.params.get("call_id").and_then(|v| v.as_u64());
        match event.method.as_str() {
            "cep.call_started" => started_id = cid,
            "cep.call_finished" => finished_id = cid,
            "tool.event" => {
                // The guest's `{"phase":"probed"}` payload is forwarded by
                // `emit_sync`, which adds `call_id` to the object.
                let phase = event.params.get("phase").and_then(|v| v.as_str());
                assert_eq!(
                    phase,
                    Some("probed"),
                    "tool.event must carry the guest's phase=probed payload"
                );
                found_probed = true;
            }
            _ => {}
        }
    }
    assert!(found_probed, "expected a tool.event with phase=probed");
    let started_id = started_id.expect("cep.call_started event");
    let finished_id = finished_id.expect("cep.call_finished event");
    assert_eq!(
        started_id, finished_id,
        "started and finished call ids must match"
    );
}

/// #70 (Codeberg #48): a cancelled Component call converges on `CallCancelled`
/// end to end: the client receives the server-mapped structured error, the
/// audit finalizer records `AuditOutcome::Cancelled`, and the server survives
/// to serve the next call. Cancellation state (never a guest error string) is
/// the classification authority at every observation point.
#[tokio::test]
async fn cancelled_component_call_returns_call_cancelled_and_audits_cancelled() {
    let (client, audit, _task) = spawn_native_with_noop().await;

    // Cancel before awaiting: the server's pre-call check observes the
    // already-set watch, so the outcome is deterministic.
    let (call, control) = client.start_call_raw(
        "noop.echo".to_string(),
        serde_json::json!({ "message": "cancel" }),
    );
    control.cancel();

    let err = call.await.expect_err("cancelled call must fail");
    assert_eq!(
        err.code,
        lancet_protocol::CepErrorCode::CallCancelled,
        "client must receive the server-mapped CallCancelled"
    );

    // The audit finalizer records the cancellation as `cancelled`. The
    // finalize runs before the server responds, so the row is durable by
    // the time the client sees the error.
    audit.await_pending().await;
    let rows = audit
        .query_rows(&AuditQueryFilters {
            method: Some("noop.echo".to_string()),
            ..Default::default()
        })
        .expect("audit query");
    assert_eq!(rows.len(), 1, "exactly one noop.echo audit row");
    assert_eq!(
        rows[0].result_or_error.as_deref(),
        Some("cancelled"),
        "audit outcome must be cancelled"
    );

    // A subsequent call works — the server survived.
    let value: serde_json::Value = client
        .call_named(
            "noop.echo",
            serde_json::json!({ "message": "after-cancel" }),
        )
        .await
        .expect("server survives cancellation");
    assert_eq!(value, serde_json::json!({ "echoed": "after-cancel" }));
}

/// #28 b2 / A2: a cancellation ends the call, it does not unmake the evidence.
/// The cancelled row keeps the `AdmissionEvidence` that admission already
/// produced, so "why was this Tool allowed to start" survives the outcome.
#[tokio::test]
async fn cancelled_component_call_keeps_its_admission_evidence() {
    let (client, audit, _task) = spawn_native_with_noop().await;

    let (call, control) = client.start_call_raw(
        "noop.echo".to_string(),
        serde_json::json!({ "message": "cancel" }),
    );
    control.cancel();
    let err = call.await.expect_err("cancelled call must fail");
    assert_eq!(err.code, lancet_protocol::CepErrorCode::CallCancelled);

    let row = noop_row(&audit).await;
    assert_eq!(row.result_or_error.as_deref(), Some("cancelled"));

    let evidence = noop_evidence(&audit).await;
    assert_eq!(evidence.admission.tool, "noop.echo");
    assert_eq!(
        evidence.admission.approved_capabilities,
        vec!["session.events".to_string(), "session.result".to_string()]
    );
    assert_eq!(evidence.outcome, EvidenceOutcome::Cancelled);
    // Pre-cancelled: Wasmtime is never entered, so the ledger is empty — but
    // the record itself is present and would carry any effect that had been.
    assert!(evidence.effects.effects.is_empty());
}

/// #70 (Codeberg #48): dropping the call future mid-flight funnels through the
/// same server-side cancellation mapping (CancelGuard semantics unchanged): the
/// audit row records `cancelled` and the server survives.
#[tokio::test]
async fn dropped_component_call_audits_cancelled_and_server_survives() {
    let (client, audit, _task) = spawn_native_with_noop().await;

    let (call, control) = client.start_call_raw(
        "noop.echo".to_string(),
        serde_json::json!({ "message": "cancel" }),
    );
    control.cancel();
    let handle = tokio::spawn(call);
    // Let the spawned call send its request, then drop it mid-flight.
    tokio::task::yield_now().await;
    handle.abort();
    let _ = handle.await;

    // A subsequent call works — the server survived.
    let value: serde_json::Value = client
        .call_named(
            "noop.echo",
            serde_json::json!({ "message": "after-cancel" }),
        )
        .await
        .expect("server survives cancellation");
    assert_eq!(value, serde_json::json!({ "echoed": "after-cancel" }));

    audit.await_pending().await;
    let rows = audit
        .query_rows(&AuditQueryFilters::default())
        .expect("audit");
    assert!(
        rows.iter()
            .any(|r| r.result_or_error.as_deref() == Some("cancelled")),
        "audit must record the dropped call as cancelled"
    );
    assert!(
        rows.iter().any(|r| {
            r.result_or_error
                .as_deref()
                .is_some_and(|s| s.starts_with("success:"))
        }),
        "audit must have a success row after cancel + retry"
    );
}

/// D.6: after a Component call error (method_not_found for a non-existent
/// component method), the server survives and a subsequent call works.
#[tokio::test]
async fn server_survives_after_component_method_not_found() {
    let (client, _audit, _task) = spawn_native_with_noop().await;

    // Call a method that doesn't exist in either the Component catalog or
    // the native registry.
    let err = client
        .call_raw("no.such.tool", serde_json::json!({}))
        .await
        .expect_err("unknown method must error");
    assert_eq!(
        lancet_protocol::error::to_jsonrpc_error(&err).0,
        lancet_protocol::error::jsonrpc::METHOD_NOT_FOUND
    );

    // A subsequent no-op call still works.
    let value: serde_json::Value = client
        .call_named("noop.echo", serde_json::json!({ "message": "survived" }))
        .await
        .expect("call after error");
    assert_eq!(value, serde_json::json!({ "echoed": "survived" }));
}

/// D.6: params schema mismatch on a Component Tool is rejected before
/// entering Wasmtime (the runtime validates params against the descriptor
/// JSON Schema). The server survives.
#[tokio::test]
async fn component_params_schema_mismatch_rejected() {
    let (client, _audit, _task) = spawn_native_with_noop().await;

    // Missing required "message" field.
    let err = client
        .call_raw("noop.echo", serde_json::json!({ "wrong": "field" }))
        .await
        .expect_err("schema mismatch must error");
    assert_eq!(err.code, lancet_protocol::CepErrorCode::SchemaMismatch);

    // Subsequent valid call works.
    let value: serde_json::Value = client
        .call_named("noop.echo", serde_json::json!({ "message": "ok" }))
        .await
        .expect("call after schema error");
    assert_eq!(value, serde_json::json!({ "echoed": "ok" }));
}
