// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Operator read contract tests (query/tail/replay against a seeded store).

use super::*;
use crate::audit::{AuditOutcome, CallAuditMetadata, CepAuditContext};
use crate::evidence::{CallEvidence, EvidenceOutcome};
use crate::store_path::audit_db_path_for_project_in;
use crate::test_support::ScopedXdgStateHome;
use lancet_protocol::cell::Trust;
use lancet_protocol::evidence::{
    ADMISSION_EVIDENCE_VERSION, AdmissionEvidence, DENIED_OPERATIONS_RECORD_VERSION,
    DeniedOperationsRecord, OBSERVED_EFFECTS_RECORD_VERSION, ObservedEffectsRecord,
};

/// The native method that deliberately has no Cell evidence producer.
const NATIVE_FETCH: &str = "fetch.web";
/// A Component method whose evidence is recorded by the #112 evidence tables.
const COMPONENT_TODO_WRITE: &str = "session.todo_write";
use rusqlite::Connection;
use tempfile::TempDir;

/// Env-isolated project fixture: `XDG_STATE_HOME` redirected to a temp state
/// root (via the shared [`ScopedXdgStateHome`] guard) plus a distinct temp
/// project directory the audit store is keyed by.
struct IsolatedProjectAudit {
    xdg: ScopedXdgStateHome,
    project: TempDir,
}

impl IsolatedProjectAudit {
    fn setup() -> Self {
        Self {
            xdg: ScopedXdgStateHome::new(),
            project: TempDir::new().expect("project tempdir"),
        }
    }

    fn state_path(&self) -> &Path {
        self.xdg.path()
    }

    fn project_path(&self) -> &Path {
        self.project.path()
    }
}

fn seed_audit_db(state: &Path, project: &Path, session_id: &str, count: usize) {
    let db_path =
        audit_db_path_for_project_in(state, project).expect("resolve external audit path");
    std::fs::create_dir_all(db_path.parent().unwrap()).unwrap();
    let audit = CepAuditStore::open(&db_path).expect("open audit db");
    for i in 0..count {
        let ctx = CepAuditContext {
            turn_id: "turn-op".into(),
            session_id: session_id.into(),
            model_id: "cli-model".into(),
            runtime_version: "test".into(),
        };
        let start = audit
            .start_call(
                &ctx,
                i as u64 + 1,
                NATIVE_FETCH,
                &CallAuditMetadata::native_policy_enforced(Some("started")),
            )
            .unwrap();
        audit
            .finalize_call(
                start,
                AuditOutcome::Success {
                    summary: format!("read ok {i}"),
                },
                Some("native_handler"),
                Some(r#"{"required_capability":"fetch_url"}"#),
            )
            .unwrap();
    }
}

fn component_evidence() -> (
    AdmissionEvidence,
    ObservedEffectsRecord,
    DeniedOperationsRecord,
) {
    (
        AdmissionEvidence {
            version: ADMISSION_EVIDENCE_VERSION,
            tool: COMPONENT_TODO_WRITE.to_string(),
            approved_capabilities: vec!["session.write".to_string()],
            denied_capabilities: vec![],
            boundary_excluded: vec![],
            max_boundary: Trust::In,
        },
        ObservedEffectsRecord {
            version: OBSERVED_EFFECTS_RECORD_VERSION,
            effects: vec![],
        },
        DeniedOperationsRecord {
            version: DENIED_OPERATIONS_RECORD_VERSION,
            denied: vec![],
        },
    )
}

fn seed_component_row(audit: &CepAuditStore, session_id: &str, turn_id: &str, call_id: u64) -> i64 {
    let (admission, effects, denied) = component_evidence();
    let evidence_id = audit
        .record_call_evidence(&CallEvidence {
            turn_id,
            session_id,
            call_id,
            tool: COMPONENT_TODO_WRITE,
            outcome: EvidenceOutcome::Completed,
            admission: &admission,
            effects: Some(&effects),
            denied: Some(&denied),
        })
        .expect("record Component evidence");
    let ctx = CepAuditContext {
        turn_id: turn_id.to_string(),
        session_id: session_id.to_string(),
        model_id: "component-model".to_string(),
        runtime_version: "test".to_string(),
    };
    let started = audit
        .start_call(
            &ctx,
            call_id,
            COMPONENT_TODO_WRITE,
            &CallAuditMetadata::for_component_dispatch(),
        )
        .expect("start Component audit row");
    let metadata = serde_json::json!({
        "evidence_id": evidence_id.0,
        "required_capability": "mutate_session_state",
    })
    .to_string();
    audit
        .finalize_call(
            started,
            AuditOutcome::Success {
                summary: "component completed".to_string(),
            },
            Some("component_handler"),
            Some(&metadata),
        )
        .expect("finalize Component audit row");
    started.audit_id
}

/// The public DTOs use a fixed JSON shape rather than an ad-hoc metadata
/// fragment.  Keep representative native, Component, tail, and replay values
/// as literal fixtures so changing a field name or `null` behavior is visible
/// in this crate before it reaches the CLI.
#[test]
fn operator_dto_json_fixtures_round_trip() {
    let evidence = serde_json::json!({
        "outcome": "completed",
        "admission": {
            "version": ADMISSION_EVIDENCE_VERSION,
            "tool": COMPONENT_TODO_WRITE,
            "approved_capabilities": ["session.write"],
            "denied_capabilities": [],
            "boundary_excluded": [],
            "max_boundary": "in"
        },
        "effects": {
            "version": OBSERVED_EFFECTS_RECORD_VERSION,
            "effects": []
        },
        "denied": {
            "version": DENIED_OPERATIONS_RECORD_VERSION,
            "denied": []
        }
    });
    let query_fixture = serde_json::json!({
        "session_id": "session-fixture",
        "row_count": 2,
        "rows": [
            {
                "audit_id": 7,
                "call_id": 41,
                "method": NATIVE_FETCH,
                "started_at": "100",
                "ended_at": "110",
                "result_or_error": "success:native",
                "impact_kind": "external_observation",
                "turn_id": "turn-fixture",
                "session_id": "session-fixture",
                "semantic_stage": "cep_native",
                "semantic_status": "native_handler",
                "guard_stage": "cep_policy_lifecycle",
                "policy_enforced": true,
                "model_id": "fixture-model",
                "runtime_version": "fixture-runtime",
                "evidence": null
            },
            {
                "audit_id": 8,
                "call_id": 42,
                "method": COMPONENT_TODO_WRITE,
                "started_at": "111",
                "ended_at": "112",
                "result_or_error": "success:component",
                "impact_kind": "unknown",
                "turn_id": "turn-fixture",
                "session_id": "session-fixture",
                "semantic_stage": "cell_component",
                "semantic_status": "component_handler",
                "guard_stage": "cell_admission",
                "policy_enforced": true,
                "model_id": "fixture-model",
                "runtime_version": "fixture-runtime",
                "evidence": evidence
            }
        ]
    });
    let query: AuditQueryOutput =
        serde_json::from_value(query_fixture.clone()).expect("query fixture decodes");
    assert_eq!(
        serde_json::to_value(&query).expect("query fixture encodes"),
        query_fixture
    );

    let tail_fixture = serde_json::json!({
        "session_id": "session-fixture",
        "rows": [{
            "audit_id": 8,
            "method": COMPONENT_TODO_WRITE,
            "impact_kind": "unknown",
            "summary": "success:component",
            "evidence": serde_json::to_value(query.rows[1].evidence.clone()).expect("evidence value")
        }],
        "last_audit_id": 8,
        "done": true,
        "batches": 1
    });
    let tail: AuditTailOutput =
        serde_json::from_value(tail_fixture.clone()).expect("tail fixture decodes");
    assert_eq!(
        serde_json::to_value(&tail).expect("tail fixture encodes"),
        tail_fixture
    );

    let replay_fixture = serde_json::json!({
        "trace": {
            "turn_id": "turn-fixture",
            "calls": [{
                "audit_id": 8,
                "call_id": "42",
                "method": COMPONENT_TODO_WRITE,
                "impact_kind": "unknown",
                "outcome_summary": "success:component",
                "evidence": serde_json::to_value(query.rows[1].evidence.clone()).expect("evidence value")
            }]
        }
    });
    let replay: AuditReplayResult =
        serde_json::from_value(replay_fixture.clone()).expect("replay fixture decodes");
    assert_eq!(
        serde_json::to_value(&replay).expect("replay fixture encodes"),
        replay_fixture
    );
}

async fn assert_component_evidence_read_error(mutate: impl FnOnce(&Connection), expected: &str) {
    let iso = IsolatedProjectAudit::setup();
    let session = "sess-bad-evidence";
    let db_path =
        audit_db_path_for_project_in(iso.state_path(), iso.project_path()).expect("audit path");
    std::fs::create_dir_all(db_path.parent().expect("audit parent")).expect("audit parent");
    let audit = CepAuditStore::open(&db_path).expect("open audit store");
    seed_component_row(&audit, session, "turn-bad-evidence", 9);
    drop(audit);

    let conn = Connection::open(&db_path).expect("open raw audit db");
    mutate(&conn);
    drop(conn);

    let mut query_request = AuditRequest::new(iso.project_path());
    query_request.session_id = session.to_string();
    let query_error = query_audit(query_request)
        .await
        .expect_err("bad evidence must fail query");
    assert!(
        matches!(&query_error, OperatorAuditError::Internal(message) if message.contains(expected)),
        "unexpected query error: {query_error}"
    );

    let mut tail_request = AuditRequest::new(iso.project_path());
    tail_request.session_id = session.to_string();
    let tail_error = tail_audit(tail_request)
        .await
        .expect_err("bad evidence must fail tail");
    assert!(
        matches!(&tail_error, OperatorAuditError::Internal(message) if message.contains(expected)),
        "unexpected tail error: {tail_error}"
    );

    let replay_error = replay_audit(AuditReplayRequest::new(
        iso.project_path(),
        session,
        "turn-bad-evidence",
    ))
    .await
    .expect_err("bad evidence must fail replay");
    assert!(
        matches!(&replay_error, OperatorAuditError::Internal(message) if message.contains(expected)),
        "unexpected replay error: {replay_error}"
    );
}

#[tokio::test]
async fn component_evidence_pointer_failures_are_operator_read_errors() {
    assert_component_evidence_read_error(
        |conn| {
            conn.execute(
                "UPDATE cep_audit_entries SET metadata_json = ?1",
                rusqlite::params![r#"{"evidence_id":999999}"#],
            )
            .expect("replace evidence pointer");
        },
        "referenced Cell evidence record is missing",
    )
    .await;

    assert_component_evidence_read_error(
        |conn| {
            conn.execute("UPDATE evidence_calls SET admission_version = 999", [])
                .expect("make evidence version unsupported");
        },
        "reports version 999",
    )
    .await;

    assert_component_evidence_read_error(
        |conn| {
            conn.execute(
                "UPDATE evidence_calls SET admission_json = ?1",
                rusqlite::params!["not-json"],
            )
            .expect("corrupt evidence json");
        },
        "admission record",
    )
    .await;

    assert_component_evidence_read_error(
        |conn| {
            conn.execute(
                "UPDATE evidence_calls SET tool = ?1",
                rusqlite::params!["different.tool"],
            )
            .expect("mismatch evidence tool");
        },
        "does not match the audit row",
    )
    .await;
}

#[tokio::test]
async fn component_row_without_evidence_pointer_remains_visible_on_every_operator_read() {
    let iso = IsolatedProjectAudit::setup();
    let session = "sess-component-without-evidence";
    let turn = "turn-component-without-evidence";
    let db_path =
        audit_db_path_for_project_in(iso.state_path(), iso.project_path()).expect("audit path");
    std::fs::create_dir_all(db_path.parent().expect("audit parent")).expect("audit parent");
    let audit = CepAuditStore::open(&db_path).expect("open audit store");
    let ctx = CepAuditContext {
        turn_id: turn.to_string(),
        session_id: session.to_string(),
        model_id: "component-model".to_string(),
        runtime_version: "test".to_string(),
    };
    let started = audit
        .start_call(
            &ctx,
            1,
            COMPONENT_TODO_WRITE,
            &CallAuditMetadata::for_component_dispatch(),
        )
        .expect("start Component audit row");
    audit
        .finalize_call(
            started,
            AuditOutcome::Success {
                summary: "component completed without evidence".to_string(),
            },
            Some("component_handler"),
            None,
        )
        .expect("finalize Component audit row without pointer");
    drop(audit);

    let mut query_request = AuditRequest::new(iso.project_path());
    query_request.session_id = session.to_string();
    let query = query_audit(query_request)
        .await
        .expect("query keeps pointerless Component row");
    assert_eq!(query.rows.len(), 1);
    assert_eq!(query.rows[0].method, COMPONENT_TODO_WRITE);
    assert_eq!(query.rows[0].evidence, None);

    let mut tail_request = AuditRequest::new(iso.project_path());
    tail_request.session_id = session.to_string();
    let tail = tail_audit(tail_request)
        .await
        .expect("tail keeps pointerless Component row");
    assert_eq!(tail.rows.len(), 1);
    assert_eq!(tail.rows[0].method, COMPONENT_TODO_WRITE);
    assert_eq!(tail.rows[0].evidence, None);

    let replay = replay_audit(AuditReplayRequest::new(iso.project_path(), session, turn))
        .await
        .expect("replay keeps pointerless Component row");
    assert_eq!(replay.trace.calls.len(), 1);
    assert_eq!(replay.trace.calls[0].method, COMPONENT_TODO_WRITE);
    assert_eq!(replay.trace.calls[0].evidence, None);
}

#[tokio::test]
async fn native_rows_keep_null_evidence_when_legacy_summary_metadata_is_malformed() {
    let iso = IsolatedProjectAudit::setup();
    let session = "sess-native-malformed-metadata";
    seed_audit_db(iso.state_path(), iso.project_path(), session, 1);
    let db_path =
        audit_db_path_for_project_in(iso.state_path(), iso.project_path()).expect("audit path");
    let conn = Connection::open(&db_path).expect("open raw audit db");
    conn.execute(
        "UPDATE cep_audit_entries SET metadata_json = ?1",
        rusqlite::params!["not-json"],
    )
    .expect("corrupt native summary metadata");
    drop(conn);

    let mut request = AuditRequest::new(iso.project_path());
    request.session_id = session.to_string();
    let output = query_audit(request)
        .await
        .expect("native query remains readable");
    assert_eq!(output.row_count, 1);
    assert_eq!(output.rows[0].method, NATIVE_FETCH);
    assert!(output.rows[0].evidence.is_none());
}

#[tokio::test]
async fn a_matching_pointer_materializes_existing_evidence_regardless_of_row_stage() {
    let iso = IsolatedProjectAudit::setup();
    let session = "sess-native-evidence-pointer";
    seed_audit_db(iso.state_path(), iso.project_path(), session, 1);
    let db_path =
        audit_db_path_for_project_in(iso.state_path(), iso.project_path()).expect("audit path");
    let audit = CepAuditStore::open(&db_path).expect("open audit store");
    let admission = AdmissionEvidence {
        version: ADMISSION_EVIDENCE_VERSION,
        tool: NATIVE_FETCH.to_string(),
        approved_capabilities: vec!["fetch_url".to_string()],
        denied_capabilities: vec![],
        boundary_excluded: vec![],
        max_boundary: Trust::In,
    };
    let effects = ObservedEffectsRecord {
        version: OBSERVED_EFFECTS_RECORD_VERSION,
        effects: vec![],
    };
    let denied = DeniedOperationsRecord {
        version: DENIED_OPERATIONS_RECORD_VERSION,
        denied: vec![],
    };
    let evidence_id = audit
        .record_call_evidence(&CallEvidence {
            turn_id: "turn-op",
            session_id: session,
            call_id: 1,
            tool: NATIVE_FETCH,
            outcome: EvidenceOutcome::Completed,
            admission: &admission,
            effects: Some(&effects),
            denied: Some(&denied),
        })
        .expect("record existing evidence");
    drop(audit);
    let conn = Connection::open(&db_path).expect("open raw audit db");
    conn.execute(
        "UPDATE cep_audit_entries SET metadata_json = ?1",
        rusqlite::params![serde_json::json!({"evidence_id": evidence_id.0}).to_string()],
    )
    .expect("attach matching evidence pointer");
    drop(conn);

    let mut request = AuditRequest::new(iso.project_path());
    request.session_id = session.to_string();
    let output = query_audit(request)
        .await
        .expect("matching pointer materializes existing evidence");
    let evidence = output.rows[0].evidence.as_ref().expect("operator evidence");
    assert_eq!(
        evidence.admission.tool, NATIVE_FETCH,
        "the adapter attaches an existing matching record without creating a native producer"
    );
}

#[tokio::test]
async fn query_audit_reads_store_without_cep_server() {
    let iso = IsolatedProjectAudit::setup();
    let session = "sess-direct-query";
    seed_audit_db(iso.state_path(), iso.project_path(), session, 2);
    let mut request = AuditRequest::new(iso.project_path());
    request.session_id = session.to_string();
    let output = query_audit(request).await.expect("query");
    assert_eq!(output.session_id, session);
    assert_eq!(output.row_count, 2);
    assert_eq!(output.rows.len(), 2);
    assert!(
        output.rows.iter().all(|row| row.evidence.is_none()),
        "native rows without an evidence_id stay in the operator row set with evidence=null"
    );
}

/// Cursor state on an empty result: `emit_rows.last()` is `None`, so
/// `last_audit_id` must fall back to the requested `since` rather than resetting
/// to 0. Pins the `unwrap_or(since)` fallback that the #39 rewrite from the
/// `AuditTailResult` struct to plain locals could silently drop — an operator
/// resuming with `--since` relies on it.
#[tokio::test]
async fn tail_audit_empty_result_keeps_requested_cursor() {
    let iso = IsolatedProjectAudit::setup();
    let session = "sess-tail-empty";
    // Store exists (one row) but the tail starts past it, so the batch is empty.
    seed_audit_db(iso.state_path(), iso.project_path(), session, 1);

    let mut request = AuditRequest::new(iso.project_path());
    request.session_id = session.to_string();
    request.since_audit_id = Some(42);
    let output = tail_audit(request).await.expect("tail past the last row");
    assert!(output.rows.is_empty(), "no rows exist above audit_id 42");
    assert_eq!(output.batches, 1, "an empty batch must not loop");
    assert!(output.done);
    assert_eq!(
        output.last_audit_id, 42,
        "empty batch must preserve the requested cursor, not reset to 0"
    );
}

/// The SQL store's lower bound is inclusive, so the operator adapter advances
/// an ordinary tail cursor by one. At `i64::MAX` there is no successor: the
/// terminal cursor must therefore produce an empty completed tail rather than
/// repeat its last row.
#[tokio::test]
async fn tail_audit_max_cursor_is_exclusive() {
    let iso = IsolatedProjectAudit::setup();
    let session = "sess-tail-max";
    seed_audit_db(iso.state_path(), iso.project_path(), session, 1);
    let db_path =
        audit_db_path_for_project_in(iso.state_path(), iso.project_path()).expect("audit path");
    let conn = Connection::open(&db_path).expect("open raw audit db");
    conn.execute(
        "UPDATE cep_audit_entries SET audit_id = ?1",
        rusqlite::params![i64::MAX],
    )
    .expect("move fixture row to maximum cursor");
    drop(conn);

    let mut request = AuditRequest::new(iso.project_path());
    request.session_id = session.to_string();
    request.since_audit_id = Some(i64::MAX);
    let output = tail_audit(request)
        .await
        .expect("tail after maximum cursor");
    assert!(
        output.rows.is_empty(),
        "maximum cursor must not repeat its row"
    );
    assert_eq!(output.last_audit_id, i64::MAX);
    assert!(output.done);
    assert_eq!(output.batches, 1);
}

/// Exactly `AUDIT_TAIL_MAX_ROWS` rows must drain in ONE batch. The loop
/// over-fetches `AUDIT_TAIL_MAX_ROWS + 1` and treats `batch.len() > MAX` as
/// "capped", so an off-by-one there (`>=`) would spend a second, pointless
/// round trip. The 1001-row test above cannot catch that.
#[tokio::test]
async fn tail_audit_exactly_max_rows_is_a_single_batch() {
    let iso = IsolatedProjectAudit::setup();
    let session = "sess-tail-exact";
    seed_audit_db(
        iso.state_path(),
        iso.project_path(),
        session,
        AUDIT_TAIL_MAX_ROWS,
    );

    let mut request = AuditRequest::new(iso.project_path());
    request.session_id = session.to_string();
    request.since_audit_id = Some(0);
    let output = tail_audit(request).await.expect("tail exactly max rows");
    assert_eq!(output.rows.len(), AUDIT_TAIL_MAX_ROWS);
    assert_eq!(
        output.batches, 1,
        "exactly MAX rows must not trigger a second batch"
    );
    assert!(output.done);
    assert_eq!(output.last_audit_id, AUDIT_TAIL_MAX_ROWS as i64);
}

#[tokio::test]
async fn tail_audit_paginates_large_result_without_cep_server() {
    // Seed 1001 rows: the first batch is capped at AUDIT_TAIL_MAX_ROWS (1000),
    // so tail_audit must run a second batch to drain the remaining 1 row.
    // This exercises the real cross-batch cursor-advance path (done=false →
    // since = last_audit_id → next query_rows → done=true), not just a
    // single-batch return.
    let iso = IsolatedProjectAudit::setup();
    let session = "sess-direct-tail";
    seed_audit_db(
        iso.state_path(),
        iso.project_path(),
        session,
        AUDIT_TAIL_MAX_ROWS + 1,
    );
    let mut request = AuditRequest::new(iso.project_path());
    request.session_id = session.to_string();
    request.since_audit_id = Some(0);
    let output = tail_audit(request).await.expect("tail");
    assert_eq!(
        output.rows.len(),
        AUDIT_TAIL_MAX_ROWS + 1,
        "all 1001 rows should be emitted across batches"
    );
    assert_eq!(
        output.batches, 2,
        "expected exactly two batches (capped + remainder)"
    );
    assert!(output.done, "tail should be fully drained");
    assert_eq!(
        output.last_audit_id,
        (AUDIT_TAIL_MAX_ROWS + 1) as i64,
        "last audit_id should be the final seeded row"
    );
    // No duplicate audit_ids across the whole multi-batch result.
    let mut seen = std::collections::HashSet::new();
    for row in &output.rows {
        let id = row.audit_id;
        assert!(seen.insert(id), "duplicate audit_id across batches: {id}");
    }
}

#[tokio::test]
async fn query_audit_errors_when_store_missing() {
    let iso = IsolatedProjectAudit::setup();
    let request = AuditRequest::new(iso.project_path());
    let err = query_audit(request).await.expect_err("missing store");
    assert!(matches!(err, OperatorAuditError::Config(_)));
}

#[tokio::test]
async fn replay_audit_returns_trace_for_turn() {
    let iso = IsolatedProjectAudit::setup();
    let session = "sess-replay";
    seed_audit_db(iso.state_path(), iso.project_path(), session, 1);
    let request = AuditReplayRequest::new(iso.project_path(), session, "turn-op");
    let replay = replay_audit(request).await.expect("replay");
    assert_eq!(replay.trace.turn_id, "turn-op");
    assert!(!replay.trace.calls.is_empty());
    assert!(
        replay
            .trace
            .calls
            .iter()
            .all(|call| call.evidence.is_none()),
        "native rows without pointers remain replay calls with evidence=null"
    );
}

#[tokio::test]
async fn replay_audit_returns_every_finalized_row_past_the_query_cap() {
    let iso = IsolatedProjectAudit::setup();
    let session = "sess-replay-over-query-cap";
    let count = crate::audit::AUDIT_QUERY_MAX_ROWS + 1;
    seed_audit_db(iso.state_path(), iso.project_path(), session, count);

    let mut first_page_request = AuditRequest::new(iso.project_path());
    first_page_request.session_id = session.to_string();
    let first_page = query_audit(first_page_request)
        .await
        .expect("bounded first query page");
    assert_eq!(first_page.rows.len(), crate::audit::AUDIT_QUERY_MAX_ROWS);
    assert_eq!(first_page.rows.first().expect("first row").call_id, 1);
    assert_eq!(
        first_page.rows.last().expect("last capped row").call_id,
        crate::audit::AUDIT_QUERY_MAX_ROWS as u64
    );

    let mut final_page_request = AuditRequest::new(iso.project_path());
    final_page_request.session_id = session.to_string();
    final_page_request.since_audit_id = Some(count as i64);
    let final_page = query_audit(final_page_request)
        .await
        .expect("query page at inclusive cursor");
    assert_eq!(final_page.row_count, 1);
    assert_eq!(final_page.rows[0].call_id, count as u64);

    let replay = replay_audit(AuditReplayRequest::new(
        iso.project_path(),
        session,
        "turn-op",
    ))
    .await
    .expect("replay every finalized row");

    assert_eq!(
        replay.trace.calls.len(),
        count,
        "turn replay must not inherit the bounded query API cap"
    );
    assert_eq!(replay.trace.calls.first().expect("first call").call_id, "1");
    assert_eq!(
        replay.trace.calls.last().expect("last call").call_id,
        count.to_string()
    );
}

/// The operator adapter has one finalized-row collection for native and
/// Component calls. Evidence is an optional attachment, never a membership or
/// pagination predicate.
#[tokio::test]
async fn operator_contract_matrix_preserves_native_component_and_mixed_rows() {
    let iso = IsolatedProjectAudit::setup();
    let session = "sess-ncm";
    seed_audit_db(iso.state_path(), iso.project_path(), session, 2);
    let db_path =
        audit_db_path_for_project_in(iso.state_path(), iso.project_path()).expect("audit path");
    let audit = CepAuditStore::open(&db_path).expect("open seeded audit store");
    let component_audit_id = seed_component_row(&audit, session, "turn-op", 99);

    let mut all = AuditRequest::new(iso.project_path());
    all.session_id = session.to_string();
    let query = query_audit(all).await.expect("mixed query");
    assert_eq!(query.row_count, 3);
    assert_eq!(
        query
            .rows
            .iter()
            .map(|row| row.audit_id)
            .collect::<Vec<_>>(),
        vec![1, 2, component_audit_id],
        "query keeps the SQLite audit-id order"
    );
    assert_eq!(query.rows[0].method, NATIVE_FETCH);
    assert!(
        query.rows[0].evidence.is_none(),
        "N remains visible as null evidence"
    );
    assert_eq!(query.rows[2].method, COMPONENT_TODO_WRITE);
    assert_eq!(
        query.rows[2]
            .evidence
            .as_ref()
            .map(|evidence| evidence.outcome.as_str()),
        Some("completed"),
        "C materializes the existing Cell evidence"
    );
    let query_json = serde_json::to_value(&query).expect("query serde");
    assert!(query_json["rows"][0]["evidence"].is_null());
    assert_eq!(query_json["rows"][2]["evidence"]["outcome"], "completed");
    assert!(
        query_json["rows"][2].get("metadata_json").is_none(),
        "metadata_json is Adapter-internal and never serialized"
    );
    let query_round_trip: AuditQueryOutput =
        serde_json::from_value(query_json).expect("query DTO round trip");
    assert_eq!(query_round_trip, query);

    let mut native_method = AuditRequest::new(iso.project_path());
    native_method.session_id = session.to_string();
    native_method.method = Some(NATIVE_FETCH.to_string());
    assert_eq!(
        query_audit(native_method)
            .await
            .expect("native method filter")
            .rows
            .len(),
        2,
        "N participates in method filtering despite no evidence"
    );

    let mut native_capability = AuditRequest::new(iso.project_path());
    native_capability.session_id = session.to_string();
    native_capability.capability = Some("fetch_url".to_string());
    assert_eq!(
        query_audit(native_capability)
            .await
            .expect("native capability filter")
            .rows
            .len(),
        2,
        "N participates in capability filtering despite no evidence"
    );

    let mut native_impact = AuditRequest::new(iso.project_path());
    native_impact.session_id = session.to_string();
    native_impact.impact = Some("external_observation".to_string());
    assert_eq!(
        query_audit(native_impact)
            .await
            .expect("native impact filter")
            .rows
            .len(),
        2,
        "N participates in impact filtering despite no evidence"
    );

    let mut component_capability = AuditRequest::new(iso.project_path());
    component_capability.session_id = session.to_string();
    component_capability.capability = Some("mutate_session_state".to_string());
    let component_query = query_audit(component_capability)
        .await
        .expect("Component capability filter");
    assert_eq!(component_query.rows.len(), 1);
    assert_eq!(component_query.rows[0].method, COMPONENT_TODO_WRITE);
    assert!(component_query.rows[0].evidence.is_some());

    let mut component_impact = AuditRequest::new(iso.project_path());
    component_impact.session_id = session.to_string();
    component_impact.impact = Some("unknown".to_string());
    assert_eq!(
        query_audit(component_impact)
            .await
            .expect("Component persisted impact label filter")
            .rows
            .len(),
        1,
        "the adapter preserves and filters the Component row's persisted label"
    );

    let mut success = AuditRequest::new(iso.project_path());
    success.session_id = session.to_string();
    success.verdict = Some("success".to_string());
    assert_eq!(
        query_audit(success)
            .await
            .expect("success verdict filter")
            .rows
            .len(),
        3,
        "both N and C remain in the verdict-filtered collection"
    );

    let mut tail = AuditRequest::new(iso.project_path());
    tail.session_id = session.to_string();
    tail.since_audit_id = Some(0);
    let tail = tail_audit(tail).await.expect("mixed tail");
    assert_eq!(
        tail.rows.iter().map(|row| row.audit_id).collect::<Vec<_>>(),
        vec![1, 2, component_audit_id],
        "tail emits every finalized N/C row without a gap"
    );
    assert!(tail.rows[0].evidence.is_none());
    assert!(tail.rows[2].evidence.is_some());

    let mut continued = AuditRequest::new(iso.project_path());
    continued.session_id = session.to_string();
    continued.since_audit_id = Some(1);
    let continued = tail_audit(continued).await.expect("tail continuation");
    assert_eq!(
        continued
            .rows
            .iter()
            .map(|row| row.audit_id)
            .collect::<Vec<_>>(),
        vec![2, component_audit_id],
        "--since is an exclusive cursor with no repeat or skip"
    );

    let replay = replay_audit(AuditReplayRequest::new(
        iso.project_path(),
        session,
        "turn-op",
    ))
    .await
    .expect("mixed replay");
    assert_eq!(
        replay
            .trace
            .calls
            .iter()
            .map(|call| call.audit_id)
            .collect::<Vec<_>>(),
        vec![1, 2, component_audit_id],
        "replay preserves audit-id order across N/C calls"
    );
    assert!(replay.trace.calls[0].evidence.is_none());
    assert!(replay.trace.calls[2].evidence.is_some());
}

#[tokio::test]
async fn query_audit_excludes_unfinalized_rows_during_active_turn() {
    let iso = IsolatedProjectAudit::setup();
    let db_path = audit_db_path_for_project_in(iso.state_path(), iso.project_path())
        .expect("resolve external audit path");
    std::fs::create_dir_all(db_path.parent().unwrap()).unwrap();
    let audit = CepAuditStore::open(&db_path).unwrap();
    let session = "sess-active";
    let ctx = CepAuditContext {
        turn_id: "turn-active".into(),
        session_id: session.into(),
        model_id: "m".into(),
        runtime_version: "test".into(),
    };
    let _pending = audit
        .start_call(
            &ctx,
            1,
            NATIVE_FETCH,
            &CallAuditMetadata::native_policy_enforced(None),
        )
        .unwrap();

    let mut request = AuditRequest::new(iso.project_path());
    request.session_id = "sess-active".to_string();
    let output = query_audit(request).await.expect("query");
    assert_eq!(output.row_count, 0);
}

fn assert_operator_query_snapshot_consistent(output: &AuditQueryOutput) {
    assert_eq!(
        output.row_count,
        output.rows.len(),
        "row_count must match rows in one query snapshot"
    );
    let mut seen = std::collections::HashSet::new();
    for row in &output.rows {
        assert!(
            !row.ended_at.is_empty(),
            "operator query must only return finalized rows (ended_at set)"
        );
        assert!(
            seen.insert(row.audit_id),
            "duplicate audit id in snapshot: {}",
            row.audit_id
        );
    }
}

/// Concurrent operator reads while another task writes the same audit DB (WAL + finalized predicate).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn operator_audit_reads_concurrent_with_active_writer() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    let iso = IsolatedProjectAudit::setup();
    let session = "sess-concurrent";
    let db_path =
        audit_db_path_for_project_in(iso.state_path(), iso.project_path()).expect("audit path");
    std::fs::create_dir_all(db_path.parent().unwrap()).unwrap();
    let _ = CepAuditStore::open(&db_path).expect("init schema");

    let project = iso.project_path().to_path_buf();
    let session_id = session.to_string();
    let writer_path = db_path.clone();
    let writer_session = session_id.clone();

    let writer = tokio::spawn(async move {
        let store = CepAuditStore::open(&writer_path).expect("writer open");
        let ctx = CepAuditContext {
            turn_id: "turn-writer".into(),
            session_id: writer_session,
            model_id: "m".into(),
            runtime_version: "test".into(),
        };
        for round in 0..60u64 {
            let start = store
                .start_call(
                    &ctx,
                    round + 1,
                    NATIVE_FETCH,
                    &CallAuditMetadata::native_policy_enforced(Some("started")),
                )
                .expect("start_call");
            if round % 5 == 2 {
                tokio::time::sleep(Duration::from_millis(2)).await;
                continue;
            }
            store
                .finalize_call(
                    start,
                    AuditOutcome::Success {
                        summary: format!("ok {round}"),
                    },
                    Some("native_handler"),
                    None,
                )
                .expect("finalize_call");
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    });

    let query_ok = Arc::new(AtomicUsize::new(0));
    let mut readers = Vec::new();
    for _ in 0..4 {
        let project = project.clone();
        let session_id = session_id.clone();
        let query_ok = Arc::clone(&query_ok);
        readers.push(tokio::spawn(async move {
            for _ in 0..25 {
                let mut request = AuditRequest::new(&project);
                request.session_id = session_id.clone();
                let output = query_audit(request).await.expect("concurrent query_audit");
                assert_operator_query_snapshot_consistent(&output);
                query_ok.fetch_add(1, Ordering::Relaxed);
                tokio::task::yield_now().await;
            }
        }));
    }

    let tail_ok = Arc::new(AtomicUsize::new(0));
    let tail_project = project.clone();
    let tail_session = session_id.clone();
    let tail_ok_task = Arc::clone(&tail_ok);
    let tail_task = tokio::spawn(async move {
        for _ in 0..8 {
            let mut request = AuditRequest::new(&tail_project);
            request.session_id = tail_session.clone();
            request.since_audit_id = Some(0);
            let output = tail_audit(request).await.expect("concurrent tail_audit");
            let mut seen = std::collections::HashSet::new();
            for row in &output.rows {
                let id = row.audit_id;
                assert!(seen.insert(id), "duplicate audit_id in tail batch: {id}");
            }
            tail_ok_task.fetch_add(1, Ordering::Relaxed);
            tokio::task::yield_now().await;
        }
    });

    writer.await.expect("writer task");
    for reader in readers {
        reader.await.expect("reader task");
    }
    tail_task.await.expect("tail task");

    assert!(query_ok.load(Ordering::Relaxed) >= 100);
    assert!(tail_ok.load(Ordering::Relaxed) >= 8);

    let mut request = AuditRequest::new(iso.project_path());
    request.session_id = session.to_string();
    let final_output = query_audit(request).await.expect("final query");
    assert_operator_query_snapshot_consistent(&final_output);
    assert!(
        final_output.row_count >= 40,
        "expected most finalized writer rows, got {}",
        final_output.row_count
    );
}

/// Pre-`model_id` / `runtime_version` tables are upgraded via `init_schema` ALTERs.
#[tokio::test]
async fn query_audit_reads_legacy_table_missing_optional_columns() {
    let iso = IsolatedProjectAudit::setup();
    let db_path =
        audit_db_path_for_project_in(iso.state_path(), iso.project_path()).expect("audit path");
    std::fs::create_dir_all(db_path.parent().unwrap()).unwrap();

    {
        let conn = Connection::open(&db_path).expect("open legacy db");
        conn.execute_batch(
            "CREATE TABLE cep_audit_entries (
                audit_id INTEGER PRIMARY KEY AUTOINCREMENT,
                call_id INTEGER NOT NULL,
                method TEXT NOT NULL,
                started_at TEXT NOT NULL,
                ended_at TEXT,
                result_or_error TEXT,
                impact_kind TEXT NOT NULL,
                turn_id TEXT NOT NULL,
                session_id TEXT NOT NULL,
                semantic_stage TEXT NOT NULL,
                semantic_status TEXT,
                guard_stage TEXT NOT NULL,
                policy_enforced INTEGER NOT NULL,
                metadata_json TEXT
            );",
        )
        .expect("legacy ddl");
        conn.execute(
            "INSERT INTO cep_audit_entries (
                call_id, method, started_at, ended_at, result_or_error, impact_kind,
                turn_id, session_id, semantic_stage, semantic_status, guard_stage,
                policy_enforced, metadata_json
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            rusqlite::params![
                1_i64,
                NATIVE_FETCH,
                "1",
                "2",
                "success:legacy",
                "workspace_observation",
                "turn-legacy",
                "sess-legacy",
                "native",
                "native_handler",
                "native",
                1_i64,
                r#"{"required_capability":"read_path"}"#,
            ],
        )
        .expect("legacy row");
    }

    let mut request = AuditRequest::new(iso.project_path());
    request.session_id = "sess-legacy".to_string();
    let output = query_audit(request).await.expect("query legacy db");
    assert_eq!(output.row_count, 1);
    assert_eq!(output.rows.len(), 1);
}

/// A legacy DB containing only call rows still serves query, tail, and replay.
#[tokio::test]
async fn query_audit_reads_legacy_call_table() {
    let iso = IsolatedProjectAudit::setup();
    let db_path =
        audit_db_path_for_project_in(iso.state_path(), iso.project_path()).expect("audit path");
    std::fs::create_dir_all(db_path.parent().unwrap()).unwrap();

    // Build a store with only `cep_audit_entries`, then seed one finalized row
    // directly through the store API so the row shape stays canonical.
    {
        let conn = Connection::open(&db_path).expect("open legacy db");
        conn.execute_batch(
            "CREATE TABLE cep_audit_entries (
                audit_id INTEGER PRIMARY KEY AUTOINCREMENT,
                call_id INTEGER NOT NULL,
                method TEXT NOT NULL,
                started_at TEXT NOT NULL,
                ended_at TEXT,
                result_or_error TEXT,
                impact_kind TEXT NOT NULL,
                turn_id TEXT NOT NULL,
                session_id TEXT NOT NULL,
                semantic_stage TEXT NOT NULL,
                semantic_status TEXT,
                guard_stage TEXT NOT NULL,
                policy_enforced INTEGER NOT NULL,
                metadata_json TEXT,
                model_id TEXT,
                runtime_version TEXT
            );",
        )
        .expect("legacy ddl");
    }

    // Reopen through `CepAuditStore::open`; the current audit/evidence schema
    // is added without rewriting the existing call row.
    let audit = CepAuditStore::open(&db_path).expect("reopen legacy db");
    let ctx = CepAuditContext {
        turn_id: "turn-legacy".into(),
        session_id: "sess-legacy-no-att".into(),
        model_id: "cli-model".into(),
        runtime_version: "test".into(),
    };
    let start = audit
        .start_call(
            &ctx,
            1,
            NATIVE_FETCH,
            &CallAuditMetadata::native_policy_enforced(Some("started")),
        )
        .expect("start");
    audit
        .finalize_call(
            start,
            AuditOutcome::Success {
                summary: "legacy no-att".into(),
            },
            Some("native_handler"),
            Some(r#"{"required_capability":"read_path"}"#),
        )
        .expect("finalize");

    let mut request = AuditRequest::new(iso.project_path());
    request.session_id = "sess-legacy-no-att".to_string();
    let output = query_audit(request)
        .await
        .expect("query legacy call database");
    assert_eq!(output.row_count, 1);
    assert_eq!(output.rows.len(), 1);

    let replay = replay_audit(AuditReplayRequest::new(
        iso.project_path(),
        "sess-legacy-no-att",
        "turn-legacy",
    ))
    .await
    .expect("replay legacy call database");
    assert_eq!(replay.trace.turn_id, "turn-legacy");
    assert_eq!(replay.trace.calls.len(), 1);
}
