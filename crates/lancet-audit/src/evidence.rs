// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Versioned Cell evidence store (#112, ADR 0005 decision 2).
//!
//! Cell evidence is the durable record of Tool execution, and its vocabulary is
//! [`lancet_protocol::evidence`] — this module adds persistence, not a second
//! shape. Three tables hold the three records a call produces:
//!
//! - `evidence_calls` — one row per Tool call, addressable by turn and by call.
//!   Its `evidence_id` is the **evidence identity**: a store-generated,
//!   monotonic integer that later runtime events reference. It is deliberately
//!   *not* `cep_audit_entries.audit_id`: ADR 0004 decision 8 deletes that store,
//!   and an identity that dies with it cannot be the stable one.
//! - `evidence_effects` — the `ObservedEffect` entries of one call's
//!   [`ObservedEffectsRecord`], one row per effect, ordered by `seq`.
//! - `evidence_denials` — the call's [`DeniedOperation`] entries, same `seq`
//!   sequence.
//!
//! Every record's `version` is persisted next to its payload and validated on
//! read: an unknown version is an actionable [`EvidenceError::UnsupportedRecordVersion`],
//! never a silently mis-parsed record.
//!
//! Denials and failures are evidence, not absence (decision 3): an admission
//! failure writes its [`AdmissionEvidence`] with `outcome = "denied"`, a
//! per-operation refusal writes a `DeniedOperation`, and a result-cell violation
//! keeps the operation effects the call had already recorded.
//!
//! This store coexists with `cep_audit_entries` for the whole migration. The
//! evidence tables are the authoritative carrier of structured evidence; the
//! CEP audit row keeps only its own summary columns.

use lancet_protocol::cell::{Cell, ChannelKind, CtxOp, ExtImpact, Trust};
use lancet_protocol::evidence::{
    ADMISSION_EVIDENCE_VERSION, AdmissionEvidence, DENIED_OPERATIONS_RECORD_VERSION,
    DeniedOperation, DeniedOperationsRecord, EffectSource, OBSERVED_EFFECTS_RECORD_VERSION,
    ObservedEffect, ObservedEffectsRecord, ObservedScope, ObservedTarget,
};
use rusqlite::{Connection, params};

/// Schema version of the evidence tables, stamped in `PRAGMA user_version`
/// alongside the audit tables (they share one database file, so they share one
/// version counter — see [`crate::audit::AUDIT_SCHEMA_VERSION`]).
pub const EVIDENCE_SCHEMA_VERSION: i64 = 2;

/// Columns an already-versioned `evidence_calls` table must carry. A file whose
/// `user_version` claims evidence support but whose table shape disagrees is
/// reported, never altered.
pub(crate) const REQUIRED_EVIDENCE_CALL_COLUMNS: [&str; 9] = [
    "evidence_id",
    "turn_id",
    "session_id",
    "call_id",
    "tool",
    "outcome",
    "recorded_at",
    "admission_version",
    "admission_json",
];

/// Columns an already-versioned `evidence_effects` table must carry.
///
/// `cell_scope` and `scope` are separate columns on purpose: `Cell::scope` is
/// the scope the cell roots in, and `ObservedScope::scope` is the scope the host
/// resolved trust and channel from. Today's producers set them to the same name,
/// but they are two protocol fields and a store that collapses them would lose
/// information the moment they diverge.
pub(crate) const REQUIRED_EVIDENCE_EFFECT_COLUMNS: [&str; 13] = [
    "evidence_id",
    "seq",
    "record_version",
    "ctx_op",
    "ext_impact",
    "cell_scope",
    "scope",
    "scope_trust",
    "scope_channel",
    "target_kind",
    "target_value",
    "source",
    "context_measure",
];

/// Columns an already-versioned `evidence_denials` table must carry.
pub(crate) const REQUIRED_EVIDENCE_DENIAL_COLUMNS: [&str; 12] = [
    "evidence_id",
    "seq",
    "record_version",
    "ctx_op",
    "ext_impact",
    "cell_scope",
    "scope",
    "scope_trust",
    "scope_channel",
    "target_kind",
    "target_value",
    "reason",
];

/// DDL for the evidence tables. Created with `IF NOT EXISTS` so a pre-#112
/// database gains them on first open.
pub(crate) const EVIDENCE_SCHEMA_DDL: &str = "\
CREATE TABLE IF NOT EXISTS evidence_calls (
    evidence_id INTEGER PRIMARY KEY AUTOINCREMENT,
    turn_id TEXT NOT NULL,
    session_id TEXT NOT NULL,
    call_id INTEGER NOT NULL,
    tool TEXT NOT NULL,
    outcome TEXT NOT NULL,
    recorded_at TEXT NOT NULL,
    admission_version INTEGER NOT NULL,
    admission_json TEXT NOT NULL,
    effects_version INTEGER,
    denials_version INTEGER
);
CREATE INDEX IF NOT EXISTS evidence_calls_by_turn
    ON evidence_calls (turn_id, evidence_id);
CREATE INDEX IF NOT EXISTS evidence_calls_by_call
    ON evidence_calls (session_id, turn_id, call_id);
CREATE TABLE IF NOT EXISTS evidence_effects (
    evidence_id INTEGER NOT NULL REFERENCES evidence_calls(evidence_id),
    seq INTEGER NOT NULL,
    record_version INTEGER NOT NULL,
    ctx_op TEXT NOT NULL,
    ext_impact TEXT NOT NULL,
    cell_scope TEXT NOT NULL,
    scope TEXT NOT NULL,
    scope_trust TEXT NOT NULL,
    scope_channel TEXT NOT NULL,
    target_kind TEXT NOT NULL,
    target_value TEXT,
    source TEXT NOT NULL,
    context_measure INTEGER,
    PRIMARY KEY (evidence_id, seq)
);
CREATE TABLE IF NOT EXISTS evidence_denials (
    evidence_id INTEGER NOT NULL REFERENCES evidence_calls(evidence_id),
    seq INTEGER NOT NULL,
    record_version INTEGER NOT NULL,
    ctx_op TEXT NOT NULL,
    ext_impact TEXT NOT NULL,
    cell_scope TEXT NOT NULL,
    scope TEXT NOT NULL,
    scope_trust TEXT NOT NULL,
    scope_channel TEXT NOT NULL,
    target_kind TEXT NOT NULL,
    target_value TEXT,
    reason TEXT NOT NULL,
    PRIMARY KEY (evidence_id, seq)
);";

/// How the call the evidence describes ended. Persisted so the denial and
/// failure population is queryable without re-deriving it from the effects
/// (decision 3): a permission system is designed from exactly this set.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EvidenceOutcome {
    /// The Component ran and returned a validated result.
    Completed,
    /// Admission refused the Tool before the runtime was entered. The
    /// `AdmissionEvidence` states which stage removed what.
    AdmissionDenied,
    /// The call was dispatched and failed after admission (trap, timeout,
    /// resource exhaustion, schema mismatch, or a result-cell violation).
    /// Operation effects recorded before the failure are retained.
    Failed,
    /// The call was cancelled. Evidence already produced stays on the record.
    Cancelled,
}

impl EvidenceOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::AdmissionDenied => "admission_denied",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    fn from_str(raw: &str) -> Option<Self> {
        match raw {
            "completed" => Some(Self::Completed),
            "admission_denied" => Some(Self::AdmissionDenied),
            "failed" => Some(Self::Failed),
            "cancelled" => Some(Self::Cancelled),
            _ => None,
        }
    }
}

/// The identity of one call's evidence. Stable, persisted, and the only handle
/// a later runtime event needs (#112 criterion 2 / ADR 0005 decision 6).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EvidenceId(pub i64);

impl std::fmt::Display for EvidenceId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// One call's evidence as handed to the store. Borrowed so a producer can write
/// straight from its ledger snapshot without cloning the records.
///
/// The three records are separate on purpose: `admission` is *why* the call was
/// authorized, `effects` is *what happened*, `denied` is *what was refused*. A
/// capability alias is admission evidence only and never becomes an effect.
#[derive(Clone, Copy, Debug)]
pub struct CallEvidence<'a> {
    pub turn_id: &'a str,
    pub session_id: &'a str,
    pub call_id: u64,
    pub tool: &'a str,
    pub outcome: EvidenceOutcome,
    pub admission: &'a AdmissionEvidence,
    pub effects: Option<&'a ObservedEffectsRecord>,
    pub denied: Option<&'a DeniedOperationsRecord>,
}

/// One call's evidence as read back from the store.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredCallEvidence {
    pub evidence_id: EvidenceId,
    pub turn_id: String,
    pub session_id: String,
    pub call_id: u64,
    pub tool: String,
    pub outcome: EvidenceOutcome,
    pub recorded_at: String,
    pub admission: AdmissionEvidence,
    pub effects: ObservedEffectsRecord,
    pub denied: DeniedOperationsRecord,
}

/// Evidence persistence failures. Distinct from [`crate::audit::AuditError`]
/// because a `version` a reader does not support is not a database error and
/// must not be reported as one.
#[derive(Debug, thiserror::Error)]
pub enum EvidenceError {
    #[error("evidence database error: {0}")]
    Database(#[from] rusqlite::Error),
    #[error(
        "evidence record {record} for evidence_id {evidence_id} reports version {found}, this binary reads {supported}; upgrade the binary or move the database aside"
    )]
    UnsupportedRecordVersion {
        record: &'static str,
        evidence_id: i64,
        found: i64,
        supported: u32,
    },
    #[error("evidence row {evidence_id} has an unreadable {field}: {value}")]
    UnreadableField {
        evidence_id: i64,
        field: &'static str,
        value: String,
    },
    #[error("evidence admission record for evidence_id {evidence_id} is not valid JSON: {source}")]
    AdmissionJson {
        evidence_id: i64,
        source: serde_json::Error,
    },
}

/// Write one call's evidence and return its identity.
///
/// Runs inside the caller's transaction-less connection: the three inserts are
/// wrapped in one SQLite transaction so a reader never sees a call row without
/// its effects.
pub(crate) fn insert_call_evidence(
    conn: &mut Connection,
    evidence: &CallEvidence<'_>,
    recorded_at: &str,
) -> Result<EvidenceId, EvidenceError> {
    let admission_json = serde_json::to_string(evidence.admission).map_err(|source| {
        EvidenceError::AdmissionJson {
            evidence_id: -1,
            source,
        }
    })?;

    let tx = conn.transaction()?;
    tx.execute(
        "INSERT INTO evidence_calls (
            turn_id, session_id, call_id, tool, outcome, recorded_at,
            admission_version, admission_json, effects_version, denials_version
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![
            evidence.turn_id,
            evidence.session_id,
            evidence.call_id as i64,
            evidence.tool,
            evidence.outcome.as_str(),
            recorded_at,
            i64::from(evidence.admission.version),
            admission_json,
            evidence.effects.map(|record| i64::from(record.version)),
            evidence.denied.map(|record| i64::from(record.version)),
        ],
    )?;
    let evidence_id = tx.last_insert_rowid();

    if let Some(record) = evidence.effects {
        let mut effects = record.effects.clone();
        effects.sort_by_key(|effect| effect.seq);
        for effect in &effects {
            let (target_kind, target_value) = split_target(&effect.target);
            tx.execute(
                "INSERT INTO evidence_effects (
                    evidence_id, seq, record_version, ctx_op, ext_impact, cell_scope,
                    scope, scope_trust, scope_channel, target_kind, target_value, source,
                    context_measure
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
                params![
                    evidence_id,
                    effect.seq,
                    i64::from(record.version),
                    ctx_op_label(effect.cell.ctx_op),
                    ext_impact_label(effect.cell.ext_impact),
                    effect.cell.scope,
                    effect.scope.scope,
                    trust_label(effect.scope.trust),
                    channel_label(effect.scope.channel),
                    target_kind,
                    target_value,
                    source_label(effect.source),
                    effect.context_measure.map(|m| m as i64),
                ],
            )?;
        }
    }

    if let Some(record) = evidence.denied {
        let mut denied = record.denied.clone();
        denied.sort_by_key(|operation| operation.seq);
        for operation in &denied {
            let (target_kind, target_value) = split_target(&operation.target);
            tx.execute(
                "INSERT INTO evidence_denials (
                    evidence_id, seq, record_version, ctx_op, ext_impact, cell_scope,
                    scope, scope_trust, scope_channel, target_kind, target_value, reason
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                params![
                    evidence_id,
                    operation.seq,
                    i64::from(record.version),
                    ctx_op_label(operation.cell.ctx_op),
                    ext_impact_label(operation.cell.ext_impact),
                    operation.cell.scope,
                    operation.scope.scope,
                    trust_label(operation.scope.trust),
                    channel_label(operation.scope.channel),
                    target_kind,
                    target_value,
                    operation.reason,
                ],
            )?;
        }
    }

    tx.commit()?;
    Ok(EvidenceId(evidence_id))
}

/// Read one call's evidence by its identity.
pub(crate) fn call_evidence_by_id(
    conn: &Connection,
    evidence_id: EvidenceId,
) -> Result<Option<StoredCallEvidence>, EvidenceError> {
    let mut rows = select_calls(conn, "WHERE evidence_id = ?1", params![evidence_id.0])?;
    if rows.is_empty() {
        return Ok(None);
    }
    Ok(Some(rows.remove(0)))
}

/// Read every call's evidence for one turn, oldest first.
pub(crate) fn call_evidence_for_turn(
    conn: &Connection,
    turn_id: &str,
) -> Result<Vec<StoredCallEvidence>, EvidenceError> {
    select_calls(
        conn,
        "WHERE turn_id = ?1 ORDER BY evidence_id",
        params![turn_id],
    )
}

/// Read the evidence of one dispatched call, identified the way the transport
/// identifies it: session + turn + the per-turn `call_id`. Several rows are
/// possible only if a `call_id` is reused within a turn; they come back in
/// record order.
pub(crate) fn call_evidence_for_call(
    conn: &Connection,
    session_id: &str,
    turn_id: &str,
    call_id: u64,
) -> Result<Vec<StoredCallEvidence>, EvidenceError> {
    select_calls(
        conn,
        "WHERE session_id = ?1 AND turn_id = ?2 AND call_id = ?3 ORDER BY evidence_id",
        params![session_id, turn_id, call_id as i64],
    )
}

fn select_calls(
    conn: &Connection,
    where_clause: &str,
    bind: impl rusqlite::Params,
) -> Result<Vec<StoredCallEvidence>, EvidenceError> {
    let sql = format!(
        "SELECT evidence_id, turn_id, session_id, call_id, tool, outcome, recorded_at,
                admission_version, admission_json, effects_version, denials_version
         FROM evidence_calls {where_clause}"
    );
    let mut stmt = conn.prepare(&sql)?;
    let raw: Vec<RawCall> = stmt
        .query_map(bind, |row| {
            Ok(RawCall {
                evidence_id: row.get(0)?,
                turn_id: row.get(1)?,
                session_id: row.get(2)?,
                call_id: row.get(3)?,
                tool: row.get(4)?,
                outcome: row.get(5)?,
                recorded_at: row.get(6)?,
                admission_version: row.get(7)?,
                admission_json: row.get(8)?,
                effects_version: row.get(9)?,
                denials_version: row.get(10)?,
            })
        })?
        .collect::<Result<_, _>>()?;

    let mut out = Vec::with_capacity(raw.len());
    for call in raw {
        out.push(hydrate(conn, call)?);
    }
    Ok(out)
}

struct RawCall {
    evidence_id: i64,
    turn_id: String,
    session_id: String,
    call_id: i64,
    tool: String,
    outcome: String,
    recorded_at: String,
    admission_version: i64,
    admission_json: String,
    effects_version: Option<i64>,
    denials_version: Option<i64>,
}

fn hydrate(conn: &Connection, call: RawCall) -> Result<StoredCallEvidence, EvidenceError> {
    // Criterion 1: the record's own `version` gates interpretation. An
    // unsupported version is refused rather than parsed under the wrong shape.
    check_version(
        "admission_evidence",
        call.evidence_id,
        call.admission_version,
        ADMISSION_EVIDENCE_VERSION,
    )?;
    let admission: AdmissionEvidence =
        serde_json::from_str(&call.admission_json).map_err(|source| {
            EvidenceError::AdmissionJson {
                evidence_id: call.evidence_id,
                source,
            }
        })?;

    let effects_version = call
        .effects_version
        .unwrap_or(i64::from(OBSERVED_EFFECTS_RECORD_VERSION));
    check_version(
        "observed_effects",
        call.evidence_id,
        effects_version,
        OBSERVED_EFFECTS_RECORD_VERSION,
    )?;
    let denials_version = call
        .denials_version
        .unwrap_or(i64::from(DENIED_OPERATIONS_RECORD_VERSION));
    check_version(
        "denied_operations",
        call.evidence_id,
        denials_version,
        DENIED_OPERATIONS_RECORD_VERSION,
    )?;

    let effects = read_effects(conn, call.evidence_id, effects_version as u32)?;
    let denied = read_denials(conn, call.evidence_id, denials_version as u32)?;

    let outcome =
        EvidenceOutcome::from_str(&call.outcome).ok_or_else(|| EvidenceError::UnreadableField {
            evidence_id: call.evidence_id,
            field: "outcome",
            value: call.outcome.clone(),
        })?;

    Ok(StoredCallEvidence {
        evidence_id: EvidenceId(call.evidence_id),
        turn_id: call.turn_id,
        session_id: call.session_id,
        call_id: call.call_id as u64,
        tool: call.tool,
        outcome,
        recorded_at: call.recorded_at,
        admission,
        effects,
        denied,
    })
}

fn check_version(
    record: &'static str,
    evidence_id: i64,
    found: i64,
    supported: u32,
) -> Result<(), EvidenceError> {
    if found == i64::from(supported) {
        Ok(())
    } else {
        Err(EvidenceError::UnsupportedRecordVersion {
            record,
            evidence_id,
            found,
            supported,
        })
    }
}

fn read_effects(
    conn: &Connection,
    evidence_id: i64,
    version: u32,
) -> Result<ObservedEffectsRecord, EvidenceError> {
    let mut stmt = conn.prepare(
        "SELECT seq, ctx_op, ext_impact, cell_scope, scope, scope_trust, scope_channel,
                target_kind, target_value, source, context_measure
         FROM evidence_effects WHERE evidence_id = ?1 ORDER BY seq",
    )?;
    let rows: Vec<RawEffect> = stmt
        .query_map(params![evidence_id], |row| {
            Ok(RawEffect {
                seq: row.get(0)?,
                ctx_op: row.get(1)?,
                ext_impact: row.get(2)?,
                cell_scope: row.get(3)?,
                scope: row.get(4)?,
                scope_trust: row.get(5)?,
                scope_channel: row.get(6)?,
                target_kind: row.get(7)?,
                target_value: row.get(8)?,
                source: row.get(9)?,
                context_measure: row.get(10)?,
            })
        })?
        .collect::<Result<_, _>>()?;

    let mut effects = Vec::with_capacity(rows.len());
    for raw in rows {
        effects.push(ObservedEffect {
            seq: raw.seq,
            cell: Cell {
                ctx_op: parse_ctx_op(evidence_id, &raw.ctx_op)?,
                ext_impact: parse_ext_impact(evidence_id, &raw.ext_impact)?,
                scope: raw.cell_scope,
            },
            scope: ObservedScope {
                scope: raw.scope,
                trust: parse_trust(evidence_id, &raw.scope_trust)?,
                channel: parse_channel(evidence_id, &raw.scope_channel)?,
            },
            target: join_target(evidence_id, &raw.target_kind, raw.target_value)?,
            source: parse_source(evidence_id, &raw.source)?,
            context_measure: raw.context_measure.map(|m| m as u64),
        });
    }
    Ok(ObservedEffectsRecord { version, effects })
}

struct RawEffect {
    seq: u32,
    ctx_op: String,
    ext_impact: String,
    cell_scope: String,
    scope: String,
    scope_trust: String,
    scope_channel: String,
    target_kind: String,
    target_value: Option<String>,
    source: String,
    context_measure: Option<i64>,
}

struct RawDenial {
    seq: u32,
    ctx_op: String,
    ext_impact: String,
    cell_scope: String,
    scope: String,
    scope_trust: String,
    scope_channel: String,
    target_kind: String,
    target_value: Option<String>,
    reason: String,
}

fn read_denials(
    conn: &Connection,
    evidence_id: i64,
    version: u32,
) -> Result<DeniedOperationsRecord, EvidenceError> {
    let mut stmt = conn.prepare(
        "SELECT seq, ctx_op, ext_impact, cell_scope, scope, scope_trust, scope_channel,
                target_kind, target_value, reason
         FROM evidence_denials WHERE evidence_id = ?1 ORDER BY seq",
    )?;
    let rows: Vec<RawDenial> = stmt
        .query_map(params![evidence_id], |row| {
            Ok(RawDenial {
                seq: row.get(0)?,
                ctx_op: row.get(1)?,
                ext_impact: row.get(2)?,
                cell_scope: row.get(3)?,
                scope: row.get(4)?,
                scope_trust: row.get(5)?,
                scope_channel: row.get(6)?,
                target_kind: row.get(7)?,
                target_value: row.get(8)?,
                reason: row.get(9)?,
            })
        })?
        .collect::<Result<_, _>>()?;

    let mut denied = Vec::with_capacity(rows.len());
    for raw in rows {
        denied.push(DeniedOperation {
            seq: raw.seq,
            cell: Cell {
                ctx_op: parse_ctx_op(evidence_id, &raw.ctx_op)?,
                ext_impact: parse_ext_impact(evidence_id, &raw.ext_impact)?,
                scope: raw.cell_scope,
            },
            scope: ObservedScope {
                scope: raw.scope,
                trust: parse_trust(evidence_id, &raw.scope_trust)?,
                channel: parse_channel(evidence_id, &raw.scope_channel)?,
            },
            target: join_target(evidence_id, &raw.target_kind, raw.target_value)?,
            reason: raw.reason,
        });
    }
    Ok(DeniedOperationsRecord { version, denied })
}

// ---------------------------------------------------------------------------
// Column codecs. Cells are stored decomposed rather than as a JSON blob so the
// evidence surface is queryable (criterion 1: "first-class, queryable"), which
// a blob is not.
// ---------------------------------------------------------------------------

fn ctx_op_label(op: CtxOp) -> &'static str {
    match op {
        CtxOp::Alloc => "alloc",
        CtxOp::Free => "free",
        CtxOp::Mutate => "mutate",
    }
}

fn parse_ctx_op(evidence_id: i64, raw: &str) -> Result<CtxOp, EvidenceError> {
    match raw {
        "alloc" => Ok(CtxOp::Alloc),
        "free" => Ok(CtxOp::Free),
        "mutate" => Ok(CtxOp::Mutate),
        other => Err(EvidenceError::UnreadableField {
            evidence_id,
            field: "ctx_op",
            value: other.to_string(),
        }),
    }
}

fn ext_impact_label(impact: ExtImpact) -> &'static str {
    match impact {
        ExtImpact::None => "none",
        ExtImpact::Read => "read",
        ExtImpact::Write => "write",
    }
}

fn parse_ext_impact(evidence_id: i64, raw: &str) -> Result<ExtImpact, EvidenceError> {
    match raw {
        "none" => Ok(ExtImpact::None),
        "read" => Ok(ExtImpact::Read),
        "write" => Ok(ExtImpact::Write),
        other => Err(EvidenceError::UnreadableField {
            evidence_id,
            field: "ext_impact",
            value: other.to_string(),
        }),
    }
}

fn trust_label(trust: Trust) -> &'static str {
    match trust {
        Trust::In => "in",
        Trust::Ex => "ex",
    }
}

fn parse_trust(evidence_id: i64, raw: &str) -> Result<Trust, EvidenceError> {
    match raw {
        "in" => Ok(Trust::In),
        "ex" => Ok(Trust::Ex),
        other => Err(EvidenceError::UnreadableField {
            evidence_id,
            field: "scope_trust",
            value: other.to_string(),
        }),
    }
}

fn channel_label(channel: ChannelKind) -> &'static str {
    match channel {
        ChannelKind::Workspace => "workspace",
        ChannelKind::Network => "network",
        ChannelKind::Session => "session",
    }
}

fn parse_channel(evidence_id: i64, raw: &str) -> Result<ChannelKind, EvidenceError> {
    match raw {
        "workspace" => Ok(ChannelKind::Workspace),
        "network" => Ok(ChannelKind::Network),
        "session" => Ok(ChannelKind::Session),
        other => Err(EvidenceError::UnreadableField {
            evidence_id,
            field: "scope_channel",
            value: other.to_string(),
        }),
    }
}

fn source_label(source: EffectSource) -> &'static str {
    match source {
        EffectSource::Operation => "operation",
        EffectSource::Result => "result",
    }
}

fn parse_source(evidence_id: i64, raw: &str) -> Result<EffectSource, EvidenceError> {
    match raw {
        "operation" => Ok(EffectSource::Operation),
        "result" => Ok(EffectSource::Result),
        other => Err(EvidenceError::UnreadableField {
            evidence_id,
            field: "source",
            value: other.to_string(),
        }),
    }
}

/// Split a target into its tag and payload. The payload column is nullable
/// because `ObservedTarget::None` carries none.
fn split_target(target: &ObservedTarget) -> (&'static str, Option<String>) {
    match target {
        ObservedTarget::Path { path } => ("path", Some(path.clone())),
        ObservedTarget::Host { host } => ("host", Some(host.clone())),
        ObservedTarget::Session { turn_id } => ("session", Some(turn_id.clone())),
        ObservedTarget::None => ("none", None),
    }
}

fn join_target(
    evidence_id: i64,
    kind: &str,
    value: Option<String>,
) -> Result<ObservedTarget, EvidenceError> {
    let missing = || EvidenceError::UnreadableField {
        evidence_id,
        field: "target_value",
        value: format!("{kind} target has no value"),
    };
    match kind {
        "path" => Ok(ObservedTarget::Path {
            path: value.ok_or_else(missing)?,
        }),
        "host" => Ok(ObservedTarget::Host {
            host: value.ok_or_else(missing)?,
        }),
        "session" => Ok(ObservedTarget::Session {
            turn_id: value.ok_or_else(missing)?,
        }),
        "none" => Ok(ObservedTarget::None),
        other => Err(EvidenceError::UnreadableField {
            evidence_id,
            field: "target_kind",
            value: other.to_string(),
        }),
    }
}

#[cfg(test)]
#[path = "evidence_tests.rs"]
mod tests;
