// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Public operator audit query/tail/replay API (direct store reads).
//!
//! Operator paths open [`lancet_audit::CepAuditStore`] only (no in-process CEP
//! server composition). See `operator_audit_module_does_not_reference_cep_server_types`.
//!
//! # Concurrency invariant (downgraded, ACCEPTED RESIDUAL — see FINDINGS.md L-11-02)
//!
//! Reads and an active turn's audit writes share one SQLite file via separate
//! [`CepAuditStore`] handles. Read safety is guaranteed structurally **and**
//! covered by a synthetic-writer concurrent regression test:
//!
//! - `CepAuditStore::query_rows` hard-codes `ended_at IS NOT NULL AND
//!   result_or_error IS NOT NULL`, so in-flight (`start_call`-only) rows are
//!   excluded at the SQL layer — see `query_audit_excludes_unfinalized_rows_during_active_turn`.
//! - `init_schema` sets `journal_mode = WAL`, giving each `SELECT` a statement-level
//!   snapshot that does not block the active turn's writer.
//! - `operator_audit_reads_concurrent_with_active_writer` spawns a **synthetic**
//!   writer task (direct `CepAuditStore::start_call`/`finalize_call`, some rows
//!   left unfinalized) alongside 4 `query_audit` reader tasks and 1 `tail_audit`
//!   task on a multi-thread runtime, then asserts each query snapshot is
//!   internally consistent (`assert_operator_query_snapshot_consistent`:
//!   row_count == activities, all `ended_at` set, no duplicate ids) and tail
//!   batches contain no duplicate `audit_id`.
//!
//! **Downgrade:** the writer is synthetic, not a live production turn. This is
//! accepted because a live turn ultimately writes through the same
//! `CepAuditStore` API, so the store concurrency boundary is identical; a
//! live-turn integration test would add integration coverage but no additional
//! concurrency-safety coverage. The optimal handling (live-turn concurrent-read
//! integration test) is not done.
//!
//! A future change that weakens the structural guarantee (e.g. switching
//! `journal_mode` off WAL, making `query_rows` span multiple `SELECT`s without
//! holding the snapshot, or introducing a `CepAuditStore`-external concurrency
//! hazard) must escalate this finding and add a live-turn integration test.

use std::path::{Path, PathBuf};

use lancet_audit::{
    AUDIT_QUERY_MAX_ROWS, AuditQueryFilters, CepAuditStore, build_in_toto_statement,
    impact_kind_from_label, impact_kind_label, project_rows, project_turn_trace,
    source_audit_range_hash,
};
use lancet_catalog::methods::audit::{AuditQueryParams, AuditReplayResult, AuditTailResult};
use lancet_protocol::effect_types::ImpactKind;
use lancet_protocol::provenance::{InTotoStatement, ProvGraphFragment};
use serde_json::{Value, json};

use crate::config::audit_db_path_for_project;
use crate::error::{LancetError, LancetResult};

/// Maximum rows emitted per tail batch (matches former native handler cap).
pub const AUDIT_TAIL_MAX_ROWS: usize = 1000;

/// Audit rows are streamed in batches; this caps frontend continuation loops.
pub const AUDIT_TAIL_MAX_BATCHES: usize = 1000;

/// Request shape for audit query/tail frontends.
#[derive(Clone, Debug)]
pub struct AuditRequest {
    pub project_root: PathBuf,
    pub session_id: String,
    pub turn_id: Option<String>,
    pub method: Option<String>,
    pub impact: Option<String>,
    pub capability: Option<String>,
    pub verdict: Option<String>,
    pub since_audit_id: Option<i64>,
}

impl AuditRequest {
    pub fn new(project_root: impl Into<PathBuf>) -> Self {
        Self {
            project_root: project_root.into(),
            session_id: "default".to_string(),
            turn_id: None,
            method: None,
            impact: None,
            capability: None,
            verdict: None,
            since_audit_id: None,
        }
    }
}

/// Result of a bounded audit query.
#[derive(Clone, Debug)]
pub struct AuditQueryOutput {
    pub session_id: String,
    pub row_count: usize,
    pub fragment: ProvGraphFragment,
}

/// Result of a bounded audit tail.
#[derive(Clone, Debug)]
pub struct AuditTailOutput {
    pub session_id: String,
    pub rows: Vec<Value>,
    pub last_audit_id: i64,
    pub done: bool,
    pub batches: usize,
}

/// Replay request for operator provenance reconstruction.
#[derive(Clone, Debug)]
pub struct AuditReplayRequest {
    pub project_root: PathBuf,
    pub session_id: String,
    pub turn_id: String,
}

impl AuditReplayRequest {
    pub fn new(
        project_root: impl Into<PathBuf>,
        session_id: impl Into<String>,
        turn_id: impl Into<String>,
    ) -> Self {
        Self {
            project_root: project_root.into(),
            session_id: session_id.into(),
            turn_id: turn_id.into(),
        }
    }
}

/// Query finalized audit rows directly from the project audit store.
pub async fn query_audit(request: AuditRequest) -> LancetResult<AuditQueryOutput> {
    let impact_kind = parse_impact_kind(request.impact.as_deref())?;
    let session_id = request.session_id.clone();
    let store = open_project_audit_store(&request.project_root).await?;
    let mut filters = audit_query_filters_from_params(
        &AuditQueryParams {
            method: request.method,
            impact_kind,
            capability: request.capability,
            verdict: request.verdict,
            since_audit_id: request.since_audit_id,
            until_audit_id: None,
            turn_id: request.turn_id,
            session_id: Some(session_id.clone()),
        },
        &session_id,
    );
    filters.limit = Some(AUDIT_QUERY_MAX_ROWS);
    let rows = store
        .query_rows(&filters)
        .map_err(|err| LancetError::Internal(format!("audit query: {err}")))?;
    let row_count = rows.len();
    let fragment = project_rows(&rows);
    Ok(AuditQueryOutput {
        session_id,
        row_count,
        fragment,
    })
}

/// Tail finalized audit rows directly from the project audit store.
///
/// # Cancellation contract (downgraded, ACCEPTED RESIDUAL — see FINDINGS.md L-11-01)
///
/// The batch loop yields to the runtime between batches via
/// `tokio::task::yield_now().await` (after updating the cursor, before the next
/// `query_rows`), so dropping the future (operator Ctrl-C) can take effect at
/// batch boundaries. Each batch body itself is a synchronous SQLite `SELECT`
/// plus in-memory projection, so cancellation is **between batches**, not
/// mid-`SELECT`.
///
/// This is a downgrade: there is no dedicated `CancellationToken` plumbed from
/// the CLI into `tail_audit`, and `lattice audit --tail` does not wire Ctrl-C
/// to anything beyond the default future-drop. Worst-case work is bounded by
/// `AUDIT_TAIL_MAX_BATCHES * AUDIT_TAIL_MAX_ROWS` (1000 * 1000); an over-long
/// tail returns [`LancetError::Config`] carrying the last emitted `audit_id` so
/// the operator can resume with `--since`. The optimal handling (explicit
/// `CancellationToken` + CLI signal wiring) is not done; the downgrade is
/// accepted because `--tail` is an operator path and typical store sizes
/// complete in milliseconds.
pub async fn tail_audit(request: AuditRequest) -> LancetResult<AuditTailOutput> {
    let session_id = request.session_id.clone();
    let store = open_project_audit_store(&request.project_root).await?;
    let mut since = request.since_audit_id.unwrap_or(0);
    let mut rows = Vec::new();
    let mut tail_result = AuditTailResult {
        last_audit_id: since,
        done: true,
    };
    let mut batches = 0usize;

    loop {
        batches += 1;
        if batches > AUDIT_TAIL_MAX_BATCHES {
            return Err(LancetError::Config(format!(
                "audit tail exceeded {AUDIT_TAIL_MAX_BATCHES} batches ({} rows emitted); narrow with filters or resume with --since {}",
                rows.len(),
                tail_result.last_audit_id
            )));
        }

        let since_filter = if since > 0 { Some(since + 1) } else { None };
        let filters = AuditQueryFilters {
            session_id: Some(session_id.clone()),
            since_audit_id: since_filter,
            exclude_method_prefix: Some("observe.audit".into()),
            limit: Some(AUDIT_TAIL_MAX_ROWS + 1),
            ..Default::default()
        };
        let batch = store
            .query_rows(&filters)
            .map_err(|err| LancetError::Internal(format!("audit tail: {err}")))?;

        let capped = batch.len() > AUDIT_TAIL_MAX_ROWS;
        let emit_rows = if capped {
            &batch[..AUDIT_TAIL_MAX_ROWS]
        } else {
            &batch[..]
        };

        for row in emit_rows {
            rows.push(json!({
                "audit_id": row.audit_id,
                "method": row.method,
                "impact_kind": row.impact_kind,
                "summary": tail_row_summary(row),
            }));
        }

        tail_result = AuditTailResult {
            last_audit_id: emit_rows.last().map(|r| r.audit_id).unwrap_or(since),
            done: !capped,
        };

        if tail_result.done {
            break;
        }
        since = tail_result.last_audit_id;
        tokio::task::yield_now().await;
    }

    Ok(AuditTailOutput {
        session_id,
        rows,
        last_audit_id: tail_result.last_audit_id,
        done: tail_result.done,
        batches,
    })
}

/// Reconstruct turn trace, in-toto statement, and attestation comparison from stored rows.
///
/// # Output contract (wire superset over the retired agent-exposed handler)
///
/// `replay_audit` is an **operator-only** read; it is not an agent-exposed
/// CEP method, so it does not self-audit (no `record_durable_metadata`, no
/// `record_effect`). The retired native `observe.audit_replay` handler did
/// both, because it ran as an agent capability under policy gating.
///
/// The wire [`AuditReplayResult`] is a **superset** of that retired handler's
/// result: `stored_statement_match` and `source_audit_range_or_hash` were
/// durable-metadata-only in the old handler (written via
/// `record_durable_metadata`, returned as `None` on the wire); here they are
/// returned on the wire directly. This is an intentional improvement for the
/// operator path — the operator reads `replay_audit`'s output, not the audit
/// store's metadata — and is accepted as the post-retirement shape.
pub async fn replay_audit(request: AuditReplayRequest) -> LancetResult<AuditReplayResult> {
    if request.turn_id.trim().is_empty() {
        return Err(LancetError::Config("turn_id is required".into()));
    }
    let store = open_project_audit_store(&request.project_root).await?;
    let filters = AuditQueryFilters {
        session_id: Some(request.session_id.clone()),
        turn_id: Some(request.turn_id.clone()),
        limit: Some(AUDIT_QUERY_MAX_ROWS),
        ..Default::default()
    };
    let rows = store
        .query_rows(&filters)
        .map_err(|err| LancetError::Internal(format!("audit replay query: {err}")))?;
    if rows.is_empty() {
        return Err(LancetError::Config(format!(
            "no audit rows for turn_id={} in session {}",
            request.turn_id, request.session_id
        )));
    }

    let runtime = rows
        .iter()
        .find_map(|r| r.runtime_version.as_deref().filter(|s| !s.is_empty()))
        .unwrap_or("lattice")
        .to_string();

    let trace = project_turn_trace(&request.turn_id, &rows);
    let statement = Some(build_in_toto_statement(&request.turn_id, &rows, &runtime));
    let stored_statement_match = match store.attestation_for_turn(&request.turn_id) {
        Ok(Some(record)) => {
            stored_statement_matches_derived(&record.statement_json, statement.as_ref())
        }
        _ => None,
    };
    let range_hash = source_audit_range_hash(&rows);

    Ok(AuditReplayResult {
        trace,
        statement,
        stored_statement_match,
        source_audit_range_or_hash: Some(range_hash),
    })
}

async fn open_project_audit_store(project_root: &Path) -> LancetResult<CepAuditStore> {
    let audit_path = audit_db_path_for_project(project_root)
        .map_err(|err| LancetError::Config(format!("resolve external audit path: {err}")))?;
    if !audit_path.exists() {
        return Err(LancetError::Config(format!(
            "audit database not found at {}; run an agent turn in this project first",
            audit_path.display()
        )));
    }
    CepAuditStore::open(&audit_path)
        .map_err(|err| LancetError::Internal(format!("open audit store: {err}")))
}

fn audit_query_filters_from_params(
    params: &AuditQueryParams,
    session_id: &str,
) -> AuditQueryFilters {
    AuditQueryFilters {
        session_id: Some(session_id.to_string()),
        turn_id: params.turn_id.clone(),
        method: params.method.clone(),
        impact_kind: params
            .impact_kind
            .as_ref()
            .map(|k| impact_kind_label(k).to_string()),
        capability: params.capability.clone(),
        verdict: params.verdict.clone(),
        since_audit_id: params.since_audit_id,
        until_audit_id: params.until_audit_id,
        started_after: None,
        started_before: None,
        rejection_stage: None,
        exclude_method_prefix: None,
        limit: None,
    }
}

fn tail_row_summary(row: &lancet_audit::AuditRow) -> String {
    row.result_or_error
        .clone()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| format!("{} (in progress)", row.method))
}

fn stored_statement_matches_derived(
    stored_json: &str,
    derived: Option<&InTotoStatement>,
) -> Option<bool> {
    let derived = derived?;
    let stored: InTotoStatement = serde_json::from_str(stored_json).ok()?;
    Some(stored.predicate_type == derived.predicate_type && stored.predicate == derived.predicate)
}

fn parse_impact_kind(label: Option<&str>) -> LancetResult<Option<ImpactKind>> {
    let Some(label) = label else {
        return Ok(None);
    };
    impact_kind_from_label(label)
        .ok_or_else(|| LancetError::Config(format!("unknown impact kind: {label}")))
        .map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::audit_db_path_for_project_in;
    use crate::store_turn_attestation;
    use lancet_audit::{CallAuditMetadata, CepAuditContext};
    use lancet_catalog::CepMethod;
    use lancet_catalog::methods::workspace::ObserveWorkspace;
    use rusqlite::Connection;
    use std::sync::Mutex;
    use tempfile::TempDir;

    static XDG_TEST_LOCK: Mutex<()> = Mutex::new(());

    struct IsolatedProjectAudit {
        _state: TempDir,
        _project: TempDir,
        _guard: std::sync::MutexGuard<'static, ()>,
    }

    impl IsolatedProjectAudit {
        fn setup() -> Self {
            let guard = XDG_TEST_LOCK.lock().expect("xdg test lock");
            let state = TempDir::new().expect("state tempdir");
            let project = TempDir::new().expect("project tempdir");
            unsafe {
                std::env::set_var("XDG_STATE_HOME", state.path());
            }
            Self {
                _state: state,
                _project: project,
                _guard: guard,
            }
        }

        fn state_path(&self) -> &Path {
            self._state.path()
        }

        fn project_path(&self) -> &Path {
            self._project.path()
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
                    lancet_audit::AuditOutcome::Success {
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
        assert!(matches!(err, LancetError::Config(_)));
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

    /// When an attestation was persisted for the turn (via `store_turn_attestation`,
    /// which derives its statement from the same rows + runtime version),
    /// `replay_audit` must report `stored_statement_match == Some(true)`.
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
                    lancet_audit::AuditOutcome::Success {
                        summary: format!("read ok {i}"),
                    },
                    Some("native_handler"),
                    Some(r#"{"required_capability":"read_path"}"#),
                )
                .unwrap();
        }
        // Persist the attestation from the same rows + runtime version that
        // `replay_audit` will derive, so the stored and derived statements match.
        store_turn_attestation(&audit, &ctx, turn_id).expect("store attestation");

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
                        lancet_audit::AuditOutcome::Success {
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
            let conn = rusqlite::Connection::open(&db_path).expect("open legacy db");
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
                lancet_audit::AuditOutcome::Success {
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
}
