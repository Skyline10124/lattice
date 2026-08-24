// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Operator query, tail, and replay over the authoritative InvocationStore.
//!
//! Reads pin one immutable ledger commit and never compose a CEP server or take
//! a workspace operation lock. Operator DTOs expose only typed binding facts,
//! lifecycle state, terminal material metadata, and content-free Cell evidence.

use std::path::{Path, PathBuf};

use lancet_protocol::{InvocationId, SessionId, TurnId};
use serde::{Deserialize, Serialize};

use crate::invocation_store::{
    InvocationRecord, InvocationState, InvocationStore, LifecycleTransition, PinnedLedgerView,
};

/// Maximum current-state rows returned by one query.
pub const AUDIT_QUERY_MAX_ROWS: usize = 1000;
/// Maximum transitions returned by one tail response, except that one atomic
/// reservation commit is never split across cursors.
pub const AUDIT_TAIL_MAX_ROWS: usize = 1000;
/// Tail is one immutable-ledger read, so there is exactly one batch.
pub const AUDIT_TAIL_MAX_BATCHES: usize = 1;

#[derive(Debug, thiserror::Error)]
pub enum OperatorAuditError {
    #[error("{0}")]
    Config(String),
    #[error("{0}")]
    Internal(String),
}

pub type OperatorAuditResult<T> = Result<T, OperatorAuditError>;

/// Query filters over one pinned current-state ledger view.
#[derive(Clone, Debug)]
pub struct AuditRequest {
    pub project_root: PathBuf,
    pub session_id: Option<String>,
    pub turn_id: Option<String>,
    pub method: Option<String>,
    pub verdict: Option<String>,
    /// Optional ledger commit at which to pin a query.
    pub ledger_oid: Option<String>,
    /// Optional exclusive ledger commit cursor for tail.
    pub after: Option<String>,
}

impl AuditRequest {
    pub fn new(project_root: impl Into<PathBuf>) -> Self {
        Self {
            project_root: project_root.into(),
            session_id: None,
            turn_id: None,
            method: None,
            verdict: None,
            ledger_oid: None,
            after: None,
        }
    }
}

/// One invocation's complete safe operator projection.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OperatorAuditRow {
    pub invocation_id: InvocationId,
    pub record: InvocationRecord,
}

/// Current-state query result pinned to one immutable ledger commit.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AuditQueryOutput {
    pub ledger_oid: String,
    pub row_count: usize,
    pub rows: Vec<OperatorAuditRow>,
}

/// One lifecycle transition in authoritative commit order.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OperatorTailRow {
    pub ledger_oid: String,
    pub invocation_id: InvocationId,
    pub record: InvocationRecord,
}

/// Transition tail result. `last_cursor` is exclusive on the next request.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AuditTailOutput {
    pub ledger_oid: String,
    pub rows: Vec<OperatorTailRow>,
    pub last_cursor: Option<String>,
    pub done: bool,
}

#[derive(Clone, Debug)]
pub struct AuditReplayRequest {
    pub project_root: PathBuf,
    pub session_id: String,
    pub turn_id: String,
    pub ledger_oid: Option<String>,
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
            ledger_oid: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OperatorTurnTrace {
    pub session_id: SessionId,
    pub turn_id: TurnId,
    pub ledger_oid: String,
    pub calls: Vec<OperatorAuditRow>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AuditReplayResult {
    pub trace: OperatorTurnTrace,
}

pub async fn query_audit(request: AuditRequest) -> OperatorAuditResult<AuditQueryOutput> {
    let store = open_project_store(&request.project_root)?;
    let view = pinned_view(&store, request.ledger_oid.as_deref())?;
    let session = parse_optional_session(request.session_id.as_deref())?;
    let turn = parse_optional_turn(request.turn_id.as_deref())?;
    validate_verdict(request.verdict.as_deref())?;

    let mut rows = view
        .snapshot
        .invocations
        .into_iter()
        .filter(|(_, record)| {
            matches_filters(
                record,
                session,
                turn,
                request.method.as_deref(),
                request.verdict.as_deref(),
            )
        })
        .map(|(invocation_id, record)| OperatorAuditRow {
            invocation_id,
            record,
        })
        .collect::<Vec<_>>();
    rows.sort_by_key(row_order);
    rows.truncate(AUDIT_QUERY_MAX_ROWS);

    Ok(AuditQueryOutput {
        ledger_oid: view.commit_oid.to_string(),
        row_count: rows.len(),
        rows,
    })
}

pub async fn tail_audit(request: AuditRequest) -> OperatorAuditResult<AuditTailOutput> {
    let store = open_project_store(&request.project_root)?;
    let session = parse_optional_session(request.session_id.as_deref())?;
    let turn = parse_optional_turn(request.turn_id.as_deref())?;
    validate_verdict(request.verdict.as_deref())?;
    let after = request.after.as_deref().map(parse_oid).transpose()?;
    let (tip, transitions) = store
        .tail_pinned(after)
        .map_err(|err| OperatorAuditError::Internal(format!("tail invocation ledger: {err}")))?;

    let filtered = transitions
        .into_iter()
        .filter(|transition| {
            matches_filters(
                &transition.record,
                session,
                turn,
                request.method.as_deref(),
                request.verdict.as_deref(),
            )
        })
        .collect::<Vec<_>>();
    let take = tail_boundary(&filtered);
    let done = take == filtered.len();
    let rows = filtered
        .into_iter()
        .take(take)
        .map(tail_row)
        .collect::<Vec<_>>();
    let last_cursor = rows
        .last()
        .map(|row| row.ledger_oid.clone())
        .or(request.after);
    let ledger_oid = tip.to_string();

    Ok(AuditTailOutput {
        ledger_oid,
        rows,
        last_cursor,
        done,
    })
}

pub async fn replay_audit(request: AuditReplayRequest) -> OperatorAuditResult<AuditReplayResult> {
    let session_id = SessionId::parse(&request.session_id)
        .map_err(|err| OperatorAuditError::Config(format!("invalid session_id: {err}")))?;
    let turn_id = TurnId::parse(&request.turn_id)
        .map_err(|err| OperatorAuditError::Config(format!("invalid turn_id: {err}")))?;
    let store = open_project_store(&request.project_root)?;
    let view = pinned_view(&store, request.ledger_oid.as_deref())?;
    let ledger_oid = view.commit_oid.to_string();
    let mut calls = view
        .snapshot
        .invocations
        .into_iter()
        .filter(|(_, record)| {
            record.binding.session_id == session_id && record.binding.turn_id == turn_id
        })
        .map(|(invocation_id, record)| OperatorAuditRow {
            invocation_id,
            record,
        })
        .collect::<Vec<_>>();
    calls.sort_by_key(row_order);
    if calls.is_empty() {
        return Err(OperatorAuditError::Config(format!(
            "no invocations for turn_id={turn_id} in session {session_id}"
        )));
    }

    Ok(AuditReplayResult {
        trace: OperatorTurnTrace {
            session_id,
            turn_id,
            ledger_oid,
            calls,
        },
    })
}

fn open_project_store(project_root: &Path) -> OperatorAuditResult<InvocationStore> {
    InvocationStore::open_project(project_root).map_err(|err| {
        OperatorAuditError::Config(format!(
            "invocation ledger not found for {}; run an agent turn in this project first: {err}",
            project_root.display()
        ))
    })
}

fn pinned_view(
    store: &InvocationStore,
    cursor: Option<&str>,
) -> OperatorAuditResult<PinnedLedgerView> {
    match cursor {
        Some(cursor) => store.pinned_view_at(parse_oid(cursor)?),
        None => store.pinned_view(),
    }
    .map_err(|err| OperatorAuditError::Internal(format!("pin invocation ledger: {err}")))
}

fn parse_oid(raw: &str) -> OperatorAuditResult<gix::ObjectId> {
    gix::ObjectId::from_hex(raw.as_bytes())
        .map_err(|err| OperatorAuditError::Config(format!("invalid ledger cursor '{raw}': {err}")))
}

fn parse_optional_session(raw: Option<&str>) -> OperatorAuditResult<Option<SessionId>> {
    raw.map(|raw| {
        SessionId::parse(raw)
            .map_err(|err| OperatorAuditError::Config(format!("invalid session_id: {err}")))
    })
    .transpose()
}

fn parse_optional_turn(raw: Option<&str>) -> OperatorAuditResult<Option<TurnId>> {
    raw.map(|raw| {
        TurnId::parse(raw)
            .map_err(|err| OperatorAuditError::Config(format!("invalid turn_id: {err}")))
    })
    .transpose()
}

fn validate_verdict(verdict: Option<&str>) -> OperatorAuditResult<()> {
    if verdict.is_none_or(|value| {
        matches!(
            value,
            "reserved"
                | "dispatch_started"
                | "succeeded"
                | "failed"
                | "rejected"
                | "cancelled"
                | "not_executed"
                | "indeterminate"
        )
    }) {
        Ok(())
    } else {
        Err(OperatorAuditError::Config(format!(
            "unknown invocation verdict: {}",
            verdict.unwrap_or_default()
        )))
    }
}

fn matches_filters(
    record: &InvocationRecord,
    session: Option<SessionId>,
    turn: Option<TurnId>,
    method: Option<&str>,
    verdict: Option<&str>,
) -> bool {
    session.is_none_or(|value| record.binding.session_id == value)
        && turn.is_none_or(|value| record.binding.turn_id == value)
        && method.is_none_or(|value| record.binding.method == value)
        && verdict.is_none_or(|value| invocation_state_label(&record.state) == value)
}

pub fn invocation_state_label(state: &InvocationState) -> &'static str {
    use crate::invocation_store::TerminalOutcome;
    match state {
        InvocationState::Reserved => "reserved",
        InvocationState::DispatchStarted => "dispatch_started",
        InvocationState::Terminal { outcome, .. } => match outcome {
            TerminalOutcome::Succeeded => "succeeded",
            TerminalOutcome::Failed => "failed",
            TerminalOutcome::Rejected => "rejected",
            TerminalOutcome::Cancelled => "cancelled",
            TerminalOutcome::NotExecuted { .. } => "not_executed",
            TerminalOutcome::Indeterminate { .. } => "indeterminate",
        },
    }
}

fn row_order(row: &OperatorAuditRow) -> (TurnId, u32, u32, InvocationId) {
    (
        row.record.binding.turn_id,
        row.record.binding.model_step_ordinal,
        row.record.binding.part_ordinal,
        row.invocation_id,
    )
}

fn tail_row(transition: LifecycleTransition) -> OperatorTailRow {
    OperatorTailRow {
        ledger_oid: transition.commit_oid.to_string(),
        invocation_id: transition.invocation_id,
        record: transition.record,
    }
}

fn tail_boundary(transitions: &[LifecycleTransition]) -> usize {
    if transitions.len() <= AUDIT_TAIL_MAX_ROWS {
        return transitions.len();
    }
    let boundary_oid = transitions[AUDIT_TAIL_MAX_ROWS - 1].commit_oid;
    let mut take = AUDIT_TAIL_MAX_ROWS;
    while transitions
        .get(take)
        .is_some_and(|transition| transition.commit_oid == boundary_oid)
    {
        take += 1;
    }
    take
}

#[cfg(test)]
#[path = "operator_tests.rs"]
mod tests;
