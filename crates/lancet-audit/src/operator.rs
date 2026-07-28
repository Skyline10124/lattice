// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Operator audit query/tail/replay API (direct store reads).
//!
//! Operator paths open [`CepAuditStore`] only, never an in-process CEP server.
//! That is structural rather than asserted: this crate does not depend on
//! `lancet-native`, `lancet-dispatch`, `lancet-cep`, or `lancet-runtime`, so
//! server, transport, and registry types are not nameable here. Keep it that
//! way — adding one of those dependencies would silently reopen the door.
//!
//! The DTOs here are operator-facing, not agent Tool schema types: they must
//! never grow catalog/schema derives or become generated catalog entries.
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

use lancet_protocol::effect_types::ImpactKind;
use lancet_protocol::provenance::{InTotoStatement, ProvGraphFragment, TurnExecutionTrace};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::audit::{
    AUDIT_QUERY_MAX_ROWS, AuditQueryFilters, AuditRow, CepAuditStore, impact_kind_from_label,
    impact_kind_label,
};
use crate::provenance::{
    EvidenceLookup, project_rows_with_evidence, project_turn_trace_with_evidence,
    rebuild_in_toto_statement_with_evidence, source_audit_range_hash,
};
use crate::store_path::audit_db_path_for_project;

/// Maximum rows emitted per tail batch (matches former native handler cap).
pub const AUDIT_TAIL_MAX_ROWS: usize = 1000;

/// Audit rows are streamed in batches; this caps frontend continuation loops.
pub const AUDIT_TAIL_MAX_BATCHES: usize = 1000;

/// Failure from an operator audit read.
///
/// Callers map these onto their own error vocabulary (the `lancet-api` facade
/// maps `Config` and `Internal` onto its equivalents 1:1). Display prints the
/// carried message verbatim so wire-visible error text stays byte-identical
/// across that mapping.
#[derive(Debug, thiserror::Error)]
pub enum OperatorAuditError {
    /// Caller-facing request/configuration problem (bad filter value, missing
    /// audit store for the project).
    #[error("{0}")]
    Config(String),

    /// An unexpected internal fault while reading the audit store.
    #[error("{0}")]
    Internal(String),
}

/// Convenience result alias for operator audit reads.
pub type OperatorAuditResult<T> = Result<T, OperatorAuditError>;

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

/// Replay result: reconstructed trace plus attestation comparison.
///
/// Serde shape is wire-stable (moved verbatim from the retired catalog DTO);
/// deliberately **no** schema derive — operator DTOs are not Tool schema types.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditReplayResult {
    pub trace: TurnExecutionTrace,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub statement: Option<InTotoStatement>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stored_statement_match: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_audit_range_or_hash: Option<String>,
}

/// Query finalized audit rows directly from the project audit store.
pub async fn query_audit(request: AuditRequest) -> OperatorAuditResult<AuditQueryOutput> {
    let impact_kind = parse_impact_kind(request.impact.as_deref())?;
    let session_id = request.session_id.clone();
    let store = open_project_audit_store(&request.project_root).await?;
    let filters = AuditQueryFilters {
        session_id: Some(session_id.clone()),
        turn_id: request.turn_id,
        method: request.method,
        impact_kind: impact_kind
            .as_ref()
            .map(|k| impact_kind_label(k).to_string()),
        capability: request.capability,
        verdict: request.verdict,
        since_audit_id: request.since_audit_id,
        limit: Some(AUDIT_QUERY_MAX_ROWS),
        ..Default::default()
    };
    let rows = store
        .query_rows(&filters)
        .map_err(|err| OperatorAuditError::Internal(format!("audit query: {err}")))?;
    let row_count = rows.len();
    // #112: resolve each Component row's Cell evidence through the same store.
    let resolve = |id: crate::evidence::EvidenceId| resolve_evidence_for_projection(&store, id);
    let fragment = project_rows_with_evidence(&rows, &EvidenceLookup::new(&resolve));
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
/// tail returns [`OperatorAuditError::Config`] carrying the last emitted
/// `audit_id` so the operator can resume with `--since`. The optimal handling
/// (explicit `CancellationToken` + CLI signal wiring) is not done; the
/// downgrade is accepted because `--tail` is an operator path and typical
/// store sizes complete in milliseconds.
pub async fn tail_audit(request: AuditRequest) -> OperatorAuditResult<AuditTailOutput> {
    let session_id = request.session_id.clone();
    let store = open_project_audit_store(&request.project_root).await?;
    let mut since = request.since_audit_id.unwrap_or(0);
    let mut rows = Vec::new();
    // Tail cursor state carried across batches (formerly the catalog
    // `AuditTailResult` wire DTO; the wire type died with the CEP method).
    // `done` is assigned on every iteration before the loop can break.
    let mut last_audit_id = since;
    let mut done;
    let mut batches = 0usize;

    loop {
        batches += 1;
        if batches > AUDIT_TAIL_MAX_BATCHES {
            return Err(OperatorAuditError::Config(format!(
                "audit tail exceeded {AUDIT_TAIL_MAX_BATCHES} batches ({} rows emitted); narrow with filters or resume with --since {}",
                rows.len(),
                last_audit_id
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
            .map_err(|err| OperatorAuditError::Internal(format!("audit tail: {err}")))?;

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

        last_audit_id = emit_rows.last().map(|r| r.audit_id).unwrap_or(since);
        done = !capped;

        if done {
            break;
        }
        since = last_audit_id;
        tokio::task::yield_now().await;
    }

    Ok(AuditTailOutput {
        session_id,
        rows,
        last_audit_id,
        done,
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
pub async fn replay_audit(request: AuditReplayRequest) -> OperatorAuditResult<AuditReplayResult> {
    if request.turn_id.trim().is_empty() {
        return Err(OperatorAuditError::Config("turn_id is required".into()));
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
        .map_err(|err| OperatorAuditError::Internal(format!("audit replay query: {err}")))?;
    if rows.is_empty() {
        return Err(OperatorAuditError::Config(format!(
            "no audit rows for turn_id={} in session {}",
            request.turn_id, request.session_id
        )));
    }

    let runtime = rows
        .iter()
        .find_map(|r| r.runtime_version.as_deref().filter(|s| !s.is_empty()))
        .unwrap_or("lattice")
        .to_string();

    // #112: Cell evidence lives in the evidence table, so the replay resolves
    // each Component row's `evidence_id` through the same store it read the
    // rows from. A row that resolves to nothing projects without evidence
    // rather than with a fabricated record.
    let resolve = |id: crate::evidence::EvidenceId| resolve_evidence_for_projection(&store, id);
    let lookup = EvidenceLookup::new(&resolve);

    let trace = project_turn_trace_with_evidence(&request.turn_id, &rows, &lookup);
    // #28 D8: the stored `predicate_type` is the authority on replay, so a
    // turn attested as v1 re-projects as v1 even on a binary that would emit
    // v2 for the same rows. An unattested turn falls back to row evidence.
    let stored = store.attestation_for_turn(&request.turn_id).ok().flatten();
    let statement = Some(rebuild_in_toto_statement_with_evidence(
        &request.turn_id,
        &rows,
        &runtime,
        stored.as_ref().map(|record| record.predicate_type.as_str()),
        &lookup,
    ));
    let stored_statement_match = stored.as_ref().and_then(|record| {
        stored_statement_matches_derived(&record.statement_json, statement.as_ref())
    });
    let range_hash = source_audit_range_hash(&rows);

    Ok(AuditReplayResult {
        trace,
        statement,
        stored_statement_match,
        source_audit_range_or_hash: Some(range_hash),
    })
}

async fn open_project_audit_store(project_root: &Path) -> OperatorAuditResult<CepAuditStore> {
    let audit_path = audit_db_path_for_project(project_root)
        .map_err(|err| OperatorAuditError::Config(format!("resolve external audit path: {err}")))?;
    if !audit_path.exists() {
        return Err(OperatorAuditError::Config(format!(
            "audit database not found at {}; run an agent turn in this project first",
            audit_path.display()
        )));
    }
    CepAuditStore::open(&audit_path)
        .map_err(|err| OperatorAuditError::Internal(format!("open audit store: {err}")))
}

fn tail_row_summary(row: &AuditRow) -> String {
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

fn parse_impact_kind(label: Option<&str>) -> OperatorAuditResult<Option<ImpactKind>> {
    let Some(label) = label else {
        return Ok(None);
    };
    impact_kind_from_label(label)
        .ok_or_else(|| OperatorAuditError::Config(format!("unknown impact kind: {label}")))
        .map(Some)
}

#[cfg(test)]
#[path = "operator_tests.rs"]
mod tests;

/// Resolve a call's evidence for a projection, surfacing a read failure instead
/// of degrading silently.
///
/// `EvidenceLookup` returns `Option`, so a refused read (an unsupported record
/// version, a corrupt column) and a call that genuinely produced no evidence are
/// the same value to the projection. That matters: `version_from_rows` picks the
/// attestation predicate version by whether the lookup resolves, so a swallowed
/// error would make the attestation *shape* a function of store health rather
/// than of what the turn did. The projection still degrades — it has no error
/// channel — but the failure is no longer invisible.
fn resolve_evidence_for_projection(
    store: &CepAuditStore,
    id: crate::evidence::EvidenceId,
) -> Option<crate::evidence::StoredCallEvidence> {
    match store.call_evidence(id) {
        Ok(found) => found,
        Err(error) => {
            tracing::error!(
                evidence_id = id.0,
                %error,
                "failed to read Cell evidence for a projection; the projection omits it"
            );
            None
        }
    }
}
