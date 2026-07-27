// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Operator read contract tests (query/tail/replay against a seeded store).

use super::*;
use crate::audit::{AttestationRecord, AuditOutcome, CallAuditMetadata, CepAuditContext};
// The write path: seeding an attestation picks v1/v2 from row evidence, the
// same as a real attest. `replay_audit` re-derives under the stored
// `predicate_type` (#28 D8), so the two are deliberately different functions.
use crate::provenance::build_in_toto_statement;
use crate::store_path::audit_db_path_for_project_in;
use crate::test_support::ScopedXdgStateHome;
use lancet_catalog::CepMethod;
use lancet_catalog::methods::workspace::ObserveWorkspace;
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
                ObserveWorkspace::NAME,
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
                Some(r#"{"required_capability":"read_path"}"#),
            )
            .unwrap();
    }
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
    assert_eq!(output.fragment.activities.len(), 2);
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
        let id = row["audit_id"].as_i64().expect("audit_id present");
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
    assert!(replay.statement.is_some());
    assert!(replay.source_audit_range_or_hash.is_some());
}

/// When an attestation was persisted for the turn from the same rows and
/// runtime version the replay derives from, `replay_audit` must report
/// `stored_statement_match == Some(true)`.
///
/// The persistence is done inline with this crate's own primitives
/// (`query_rows` → `build_in_toto_statement` → `insert_attestation`) —
/// mirroring what `lancet-api::store_turn_attestation` does — because a
/// dev-dependency on `lancet-api` would be a dependency cycle.
#[tokio::test]
async fn replay_audit_matches_stored_attestation() {
    let iso = IsolatedProjectAudit::setup();
    let session = "sess-replay-match";
    let db_path =
        audit_db_path_for_project_in(iso.state_path(), iso.project_path()).expect("audit path");
    std::fs::create_dir_all(db_path.parent().unwrap()).unwrap();
    let audit = CepAuditStore::open(&db_path).expect("open audit db");
    let turn_id = "turn-op";
    let ctx = CepAuditContext {
        turn_id: turn_id.into(),
        session_id: session.into(),
        model_id: "cli-model".into(),
        runtime_version: "test".into(),
    };
    // Seed two finalized rows for the turn so the derived statement is non-trivial.
    for i in 0..2u64 {
        let start = audit
            .start_call(
                &ctx,
                i + 1,
                ObserveWorkspace::NAME,
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
                Some(r#"{"required_capability":"read_path"}"#),
            )
            .unwrap();
    }

    // Persist the attestation from the same rows + runtime version ("test",
    // carried on every seeded row) that `replay_audit` will derive, so the
    // stored and derived statements must match.
    let filters = AuditQueryFilters {
        session_id: Some(session.to_string()),
        turn_id: Some(turn_id.to_string()),
        ..Default::default()
    };
    let rows = audit
        .query_rows(&filters)
        .expect("query rows for attestation");
    assert_eq!(rows.len(), 2, "both seeded rows must be finalized");
    let statement = build_in_toto_statement(turn_id, &rows, "test");
    let statement_json = serde_json::to_string(&statement).expect("serialize statement");
    let range_hash = source_audit_range_hash(&rows);
    let generated_at = rows
        .iter()
        .filter_map(|r| r.ended_at.as_deref())
        .next_back()
        .unwrap_or_else(|| rows[0].started_at.as_str())
        .to_string();
    audit
        .insert_attestation(&AttestationRecord {
            turn_id: turn_id.to_string(),
            session_id: session.to_string(),
            statement_json,
            predicate_type: statement.predicate_type.clone(),
            generated_at,
            source_audit_range_or_hash: range_hash,
        })
        .expect("store attestation");

    let request = AuditReplayRequest::new(iso.project_path(), session, turn_id);
    let replay = replay_audit(request).await.expect("replay");
    assert_eq!(
        replay.stored_statement_match,
        Some(true),
        "stored attestation should match the derived statement"
    );
    assert!(replay.statement.is_some());
    assert!(replay.source_audit_range_or_hash.is_some());
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
            ObserveWorkspace::NAME,
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
        output.fragment.activities.len(),
        "row_count must match activities in one query snapshot"
    );
    let mut seen = std::collections::HashSet::new();
    for activity in &output.fragment.activities {
        assert!(
            activity.ended_at.is_some(),
            "operator query must only return finalized rows (ended_at set)"
        );
        assert!(
            seen.insert(activity.id.clone()),
            "duplicate activity id in snapshot: {}",
            activity.id
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
                    ObserveWorkspace::NAME,
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
                let id = row
                    .get("audit_id")
                    .and_then(|v| v.as_i64())
                    .expect("tail row audit_id");
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
                ObserveWorkspace::NAME,
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
    assert_eq!(output.fragment.activities.len(), 1);
}

/// A pre-`attestation_records` legacy DB still serves query/tail (no attestation table is
/// needed for the read path); `CepAuditStore::open` tolerates its absence.
#[tokio::test]
async fn query_audit_reads_legacy_table_without_attestation_records() {
    let iso = IsolatedProjectAudit::setup();
    let db_path =
        audit_db_path_for_project_in(iso.state_path(), iso.project_path()).expect("audit path");
    std::fs::create_dir_all(db_path.parent().unwrap()).unwrap();

    // Build a store with only `cep_audit_entries` (no `attestation_records`), mirroring an
    // older schema version, then seed one finalized row directly through the store API so the
    // row shape stays canonical.
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

    // Reopen through `CepAuditStore::open`; its `init_schema` uses `CREATE TABLE IF NOT
    // EXISTS` for `attestation_records`, so a missing attestation table is restored without
    // touching the existing `cep_audit_entries` rows.
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
            ObserveWorkspace::NAME,
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
        .expect("query legacy db without attestation");
    assert_eq!(output.row_count, 1);
    assert_eq!(output.fragment.activities.len(), 1);

    // The attestation table must now exist (re-created by `init_schema`), so replay's
    // attestation lookup returns `None` rather than erroring — replay still succeeds.
    let replay = replay_audit(AuditReplayRequest::new(
        iso.project_path(),
        "sess-legacy-no-att",
        "turn-legacy",
    ))
    .await
    .expect("replay legacy db without attestation");
    assert_eq!(replay.trace.turn_id, "turn-legacy");
    // No stored attestation row was written, so the match slot stays `None`.
    assert_eq!(replay.stored_statement_match, None);
}
