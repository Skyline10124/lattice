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

use lancet_protocol::evidence::{AdmissionEvidence, DeniedOperationsRecord, ObservedEffectsRecord};
use serde::{Deserialize, Serialize};

use crate::audit::{
    AUDIT_QUERY_MAX_ROWS, AuditQueryFilters, AuditRow, CepAuditStore, SEMANTIC_STAGE_COMPONENT,
    impact_kind_from_label, impact_kind_label,
};
use crate::evidence::{EvidenceId, EvidenceOutcome, StoredCallEvidence};
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

/// Optional structured Cell evidence attached to a finalized audit row.
///
/// The evidence tables remain the authority for these records.  This DTO does
/// not expose SQLite's transitional `evidence_id`; it carries only the existing
/// protocol evidence serde shapes required by an operator read.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperatorCellEvidence {
    pub outcome: EvidenceOutcome,
    pub admission: AdmissionEvidence,
    pub effects: ObservedEffectsRecord,
    pub denied: DeniedOperationsRecord,
}

impl From<StoredCallEvidence> for OperatorCellEvidence {
    fn from(value: StoredCallEvidence) -> Self {
        Self {
            outcome: value.outcome,
            admission: value.admission,
            effects: value.effects,
            denied: value.denied,
        }
    }
}

/// One finalized SQLite audit row as exposed to operator query reads.
///
/// `call_id` is the transitional SQLite row value, not an InvocationId.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperatorAuditRow {
    pub audit_id: i64,
    pub call_id: u64,
    pub method: String,
    pub started_at: String,
    pub ended_at: String,
    pub result_or_error: String,
    pub impact_kind: String,
    pub turn_id: String,
    pub session_id: String,
    pub semantic_stage: String,
    pub semantic_status: Option<String>,
    pub guard_stage: String,
    pub policy_enforced: bool,
    pub model_id: Option<String>,
    pub runtime_version: Option<String>,
    pub evidence: Option<OperatorCellEvidence>,
}

/// Result of a bounded audit query.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditQueryOutput {
    pub session_id: String,
    pub row_count: usize,
    pub rows: Vec<OperatorAuditRow>,
}

/// Compact finalized audit row emitted by audit tail.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperatorTailRow {
    pub audit_id: i64,
    pub method: String,
    pub impact_kind: String,
    pub summary: String,
    pub evidence: Option<OperatorCellEvidence>,
}

/// Result of a bounded audit tail.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditTailOutput {
    pub session_id: String,
    pub rows: Vec<OperatorTailRow>,
    pub last_audit_id: i64,
    pub done: bool,
    pub batches: usize,
}

/// Replay request for an operator turn read.
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

/// Operator replay trace for one turn.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperatorTurnTrace {
    pub turn_id: String,
    pub calls: Vec<OperatorReplayCall>,
}

/// One finalized call in an operator replay trace.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperatorReplayCall {
    pub audit_id: i64,
    /// Textual form of the transitional SQLite row value, not an InvocationId.
    pub call_id: String,
    pub method: String,
    pub impact_kind: String,
    pub outcome_summary: Option<String>,
    pub evidence: Option<OperatorCellEvidence>,
}

/// Replay result for an operator turn read.
///
/// Deliberately no schema derive: operator DTOs are not Tool schema types.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditReplayResult {
    pub trace: OperatorTurnTrace,
}

/// Query finalized audit rows directly from the project audit store.
pub async fn query_audit(request: AuditRequest) -> OperatorAuditResult<AuditQueryOutput> {
    let impact_kind = parse_impact_label(request.impact.as_deref())?;
    let session_id = request.session_id.clone();
    let store = open_project_audit_store(&request.project_root).await?;
    let filters = AuditQueryFilters {
        session_id: Some(session_id.clone()),
        turn_id: request.turn_id,
        method: request.method,
        impact_kind,
        capability: request.capability,
        verdict: request.verdict,
        since_audit_id: request.since_audit_id,
        limit: Some(AUDIT_QUERY_MAX_ROWS),
        ..Default::default()
    };
    let rows = store
        .query_rows(&filters)
        .map_err(|err| OperatorAuditError::Internal(format!("audit query: {err}")))?;
    let rows = rows
        .iter()
        .map(|row| materialize_operator_audit_row(&store, row))
        .collect::<OperatorAuditResult<Vec<_>>>()?;
    let row_count = rows.len();
    Ok(AuditQueryOutput {
        session_id,
        row_count,
        rows,
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
    let impact_kind = parse_impact_label(request.impact.as_deref())?;
    let session_id = request.session_id.clone();
    let store = open_project_audit_store(&request.project_root).await?;
    let mut since = request.since_audit_id.unwrap_or(0);
    // `query_rows` expresses its cursor as `audit_id >= ?`. There is no i64
    // successor to the largest legal SQLite id, so incrementing it would
    // saturate and repeat the cursor row. Treat that terminal cursor as an
    // empty, completed tail instead.
    if since == i64::MAX {
        return Ok(AuditTailOutput {
            session_id,
            rows: Vec::new(),
            last_audit_id: since,
            done: true,
            batches: 1,
        });
    }
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

        let since_filter = if since > 0 {
            Some(since.saturating_add(1))
        } else {
            None
        };
        let filters = AuditQueryFilters {
            session_id: Some(session_id.clone()),
            turn_id: request.turn_id.clone(),
            method: request.method.clone(),
            impact_kind: impact_kind.clone(),
            capability: request.capability.clone(),
            verdict: request.verdict.clone(),
            since_audit_id: since_filter,
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
            rows.push(materialize_operator_tail_row(&store, row)?);
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

/// Read the finalized calls of one turn directly from the audit store.
///
/// `replay_audit` is operator-only: it neither composes a CEP server nor
/// self-audits. It returns row facts in audit-id order; it does not rebuild a
/// projection graph or a Turn statement.
pub async fn replay_audit(request: AuditReplayRequest) -> OperatorAuditResult<AuditReplayResult> {
    if request.turn_id.trim().is_empty() {
        return Err(OperatorAuditError::Config("turn_id is required".into()));
    }
    let store = open_project_audit_store(&request.project_root).await?;
    let filters = AuditQueryFilters {
        session_id: Some(request.session_id.clone()),
        turn_id: Some(request.turn_id.clone()),
        // Replay is the complete finalized call set for one turn. The public
        // query API remains bounded, but that cap must not truncate a replay.
        limit: None,
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

    let calls = rows
        .iter()
        .map(|row| materialize_operator_replay_call(&store, row))
        .collect::<OperatorAuditResult<Vec<_>>>()?;

    Ok(AuditReplayResult {
        trace: OperatorTurnTrace {
            turn_id: request.turn_id,
            calls,
        },
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

fn materialize_operator_audit_row(
    store: &CepAuditStore,
    row: &AuditRow,
) -> OperatorAuditResult<OperatorAuditRow> {
    Ok(OperatorAuditRow {
        audit_id: row.audit_id,
        call_id: row.call_id,
        method: row.method.clone(),
        started_at: row.started_at.clone(),
        ended_at: finalized_field(row, "ended_at", &row.ended_at)?,
        result_or_error: finalized_field(row, "result_or_error", &row.result_or_error)?,
        impact_kind: row.impact_kind.clone(),
        turn_id: row.turn_id.clone(),
        session_id: row.session_id.clone(),
        semantic_stage: row.semantic_stage.clone(),
        semantic_status: row.semantic_status.clone(),
        guard_stage: row.guard_stage.clone(),
        policy_enforced: row.policy_enforced,
        model_id: row.model_id.clone(),
        runtime_version: row.runtime_version.clone(),
        evidence: materialize_operator_cell_evidence(store, row)?,
    })
}

fn materialize_operator_tail_row(
    store: &CepAuditStore,
    row: &AuditRow,
) -> OperatorAuditResult<OperatorTailRow> {
    Ok(OperatorTailRow {
        audit_id: row.audit_id,
        method: row.method.clone(),
        impact_kind: row.impact_kind.clone(),
        summary: finalized_field(row, "result_or_error", &row.result_or_error)?,
        evidence: materialize_operator_cell_evidence(store, row)?,
    })
}

fn materialize_operator_replay_call(
    store: &CepAuditStore,
    row: &AuditRow,
) -> OperatorAuditResult<OperatorReplayCall> {
    // `query_rows` only yields finalized records. Keep the field optional in the
    // wire DTO because the established replay shape makes the summary optional.
    let _ = finalized_field(row, "ended_at", &row.ended_at)?;
    let outcome_summary = Some(finalized_field(
        row,
        "result_or_error",
        &row.result_or_error,
    )?);
    Ok(OperatorReplayCall {
        audit_id: row.audit_id,
        call_id: row.call_id.to_string(),
        method: row.method.clone(),
        impact_kind: row.impact_kind.clone(),
        outcome_summary,
        evidence: materialize_operator_cell_evidence(store, row)?,
    })
}

fn finalized_field(
    row: &AuditRow,
    field: &str,
    value: &Option<String>,
) -> OperatorAuditResult<String> {
    value.clone().ok_or_else(|| {
        OperatorAuditError::Internal(format!(
            "audit row {} is not finalized: {field} is NULL",
            row.audit_id
        ))
    })
}

/// Attach the Cell evidence selected by an audit row's internal pointer.
///
/// `metadata_json` never crosses this boundary. A missing pointer is a valid
/// no-evidence state for every finalized row. Once a pointer exists, all
/// lookup failures are operator-read errors: returning `None` would silently
/// turn a corrupt evidence reference into a no-evidence row.
pub fn materialize_operator_cell_evidence(
    store: &CepAuditStore,
    row: &AuditRow,
) -> OperatorAuditResult<Option<OperatorCellEvidence>> {
    let Some(evidence_id) = evidence_id_from_metadata(row)? else {
        return Ok(None);
    };
    let stored = store.call_evidence(evidence_id).map_err(|error| {
        OperatorAuditError::Internal(format!(
            "audit row {} evidence_id {}: read Cell evidence: {error}",
            row.audit_id, evidence_id.0
        ))
    })?;
    let stored = stored.ok_or_else(|| {
        OperatorAuditError::Internal(format!(
            "audit row {} evidence_id {}: referenced Cell evidence record is missing",
            row.audit_id, evidence_id.0
        ))
    })?;
    if stored.session_id != row.session_id
        || stored.turn_id != row.turn_id
        || stored.call_id != row.call_id
        || stored.tool != row.method
    {
        return Err(OperatorAuditError::Internal(format!(
            "audit row {} evidence_id {}: referenced Cell evidence does not match the audit row",
            row.audit_id, evidence_id.0
        )));
    }
    Ok(Some(stored.into()))
}

fn evidence_id_from_metadata(row: &AuditRow) -> OperatorAuditResult<Option<EvidenceId>> {
    let Some(raw) = row.metadata_json.as_deref() else {
        return Ok(None);
    };
    let metadata: serde_json::Value = match serde_json::from_str(raw) {
        Ok(metadata) => metadata,
        // Native metadata is an internal summary only. A malformed historic
        // native blob cannot make a no-evidence row disappear from the full
        // finalized-row collection. Component metadata stays strict because
        // malformed JSON cannot establish whether a pointer was present.
        Err(_) if row.semantic_stage != SEMANTIC_STAGE_COMPONENT => return Ok(None),
        Err(error) => {
            return Err(OperatorAuditError::Internal(format!(
                "audit row {} metadata_json is not valid JSON: {error}",
                row.audit_id
            )));
        }
    };
    let Some(object) = metadata.as_object() else {
        return Ok(None);
    };
    let Some(raw_id) = object.get("evidence_id") else {
        return Ok(None);
    };
    let id = raw_id.as_i64().filter(|id| *id > 0).ok_or_else(|| {
        OperatorAuditError::Internal(format!(
            "audit row {} has an invalid evidence_id pointer",
            row.audit_id
        ))
    })?;
    Ok(Some(EvidenceId(id)))
}

fn parse_impact_label(label: Option<&str>) -> OperatorAuditResult<Option<String>> {
    let Some(label) = label else {
        return Ok(None);
    };
    // Component rows intentionally carry the persisted `unknown` label: there
    // is no legacy ImpactKind to coerce them into. Keep it queryable as a row
    // label while retaining the established validation for every legacy label.
    if label == "unknown" {
        return Ok(Some(label.to_string()));
    }
    impact_kind_from_label(label)
        .ok_or_else(|| OperatorAuditError::Config(format!("unknown impact kind: {label}")))
        .map(|kind| Some(impact_kind_label(&kind).to_string()))
}

#[cfg(test)]
#[path = "operator_tests.rs"]
mod tests;
