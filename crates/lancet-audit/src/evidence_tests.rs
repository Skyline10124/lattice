// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Contract tests for the versioned Cell evidence store (#112).
//!
//! Coverage matches the ticket's criteria: retrieval by turn and by call,
//! `version` validation on read, the presence of denial and failure rows, an
//! actionable error for an incompatible table shape, and a round-trip of every
//! #28 evidence scenario through the new tables.

use std::collections::BTreeSet;

use lancet_protocol::cell::{Cell, ChannelKind, CtxOp, ExtImpact, Trust};
use lancet_protocol::evidence::{
    ADMISSION_EVIDENCE_VERSION, AdmissionEvidence, DENIED_OPERATIONS_RECORD_VERSION,
    DeniedOperation, DeniedOperationsRecord, EffectSource, OBSERVED_EFFECTS_RECORD_VERSION,
    ObservedEffect, ObservedEffectsRecord, ObservedScope, ObservedTarget,
};
use rusqlite::params;

use crate::audit::{AUDIT_SCHEMA_VERSION, AuditError, CepAuditStore};
use crate::evidence::{CallEvidence, EvidenceError, EvidenceId, EvidenceOutcome};

fn scope() -> ObservedScope {
    ObservedScope {
        scope: "session".into(),
        trust: Trust::In,
        channel: ChannelKind::Session,
    }
}

fn cell(ctx_op: CtxOp) -> Cell {
    Cell {
        ctx_op,
        ext_impact: ExtImpact::None,
        scope: "session".into(),
    }
}

/// The two effects a successful `noop.echo` call produces: the gated
/// `emit-event` operation and the measured result.
fn effects_record() -> ObservedEffectsRecord {
    ObservedEffectsRecord {
        version: OBSERVED_EFFECTS_RECORD_VERSION,
        effects: vec![
            ObservedEffect {
                seq: 0,
                cell: cell(CtxOp::Mutate),
                scope: scope(),
                target: ObservedTarget::Session {
                    turn_id: "turn-1".into(),
                },
                source: EffectSource::Operation,
                context_measure: None,
            },
            ObservedEffect {
                seq: 1,
                cell: cell(CtxOp::Alloc),
                scope: scope(),
                target: ObservedTarget::Session {
                    turn_id: "turn-1".into(),
                },
                source: EffectSource::Result,
                context_measure: Some(17),
            },
        ],
    }
}

fn admission(approved: &[&str]) -> AdmissionEvidence {
    AdmissionEvidence {
        version: ADMISSION_EVIDENCE_VERSION,
        tool: "noop.echo".into(),
        approved_capabilities: approved.iter().map(|s| (*s).to_string()).collect(),
        denied_capabilities: vec!["net.fetch".into()],
        boundary_excluded: vec!["fs.write".into()],
        max_boundary: Trust::In,
    }
}

fn denials() -> DeniedOperationsRecord {
    DeniedOperationsRecord {
        version: DENIED_OPERATIONS_RECORD_VERSION,
        denied: vec![
            DeniedOperation {
                seq: 3,
                cell: cell(CtxOp::Mutate),
                scope: scope(),
                target: ObservedTarget::None,
                reason: "uncovered operation".into(),
            },
            DeniedOperation {
                seq: 2,
                cell: cell(CtxOp::Mutate),
                scope: scope(),
                target: ObservedTarget::Path {
                    path: "src/lib.rs".into(),
                },
                reason: "path target denied this cycle".into(),
            },
        ],
    }
}

fn empty_effects() -> ObservedEffectsRecord {
    ObservedEffectsRecord {
        version: OBSERVED_EFFECTS_RECORD_VERSION,
        effects: Vec::new(),
    }
}

fn empty_denials() -> DeniedOperationsRecord {
    DeniedOperationsRecord {
        version: DENIED_OPERATIONS_RECORD_VERSION,
        denied: Vec::new(),
    }
}

/// A completed call's evidence survives the round trip intact, in `seq` order,
/// with host-resolved scope, concrete target, source, and measure preserved.
#[test]
fn completed_call_evidence_round_trips() {
    let store = CepAuditStore::in_memory().expect("store");
    let admission = admission(&["session.events", "session.result"]);
    let effects = effects_record();
    let denied = empty_denials();
    let id = store
        .record_call_evidence(&CallEvidence {
            turn_id: "turn-1",
            session_id: "sess-1",
            call_id: 7,
            tool: "noop.echo",
            outcome: EvidenceOutcome::Completed,
            admission: &admission,
            effects: Some(&effects),
            denied: Some(&denied),
        })
        .expect("record");

    let stored = store
        .call_evidence(id)
        .expect("read")
        .expect("evidence exists");

    assert_eq!(stored.evidence_id, id);
    assert_eq!(stored.turn_id, "turn-1");
    assert_eq!(stored.session_id, "sess-1");
    assert_eq!(stored.call_id, 7);
    assert_eq!(stored.tool, "noop.echo");
    assert_eq!(stored.outcome, EvidenceOutcome::Completed);
    assert!(!stored.recorded_at.is_empty());
    // "Why authorized" survives whole, including what each stage removed.
    assert_eq!(stored.admission, admission);
    // "What happened" survives whole, in seq order.
    assert_eq!(stored.effects, effects);
    assert_eq!(stored.effects.effects[0].source, EffectSource::Operation);
    assert_eq!(stored.effects.effects[0].context_measure, None);
    assert_eq!(stored.effects.effects[1].context_measure, Some(17));
    assert_eq!(stored.denied.denied, Vec::new());
}

/// Criterion 1: evidence is addressable by turn. Every call of the turn comes
/// back, oldest first, and calls of another turn do not.
#[test]
fn evidence_is_addressable_by_turn() {
    let store = CepAuditStore::in_memory().expect("store");
    let admission = admission(&["session.result"]);
    let effects = effects_record();
    let denied = empty_denials();
    for (turn, call_id) in [("turn-a", 1u64), ("turn-a", 2), ("turn-b", 3)] {
        store
            .record_call_evidence(&CallEvidence {
                turn_id: turn,
                session_id: "sess-1",
                call_id,
                tool: "noop.echo",
                outcome: EvidenceOutcome::Completed,
                admission: &admission,
                effects: Some(&effects),
                denied: Some(&denied),
            })
            .expect("record");
    }

    let turn_a = store.call_evidence_for_turn("turn-a").expect("by turn");
    assert_eq!(turn_a.len(), 2);
    assert_eq!(turn_a[0].call_id, 1, "oldest first");
    assert_eq!(turn_a[1].call_id, 2);
    assert!(
        turn_a[0].evidence_id < turn_a[1].evidence_id,
        "identity is monotonic within a turn"
    );

    let turn_b = store.call_evidence_for_turn("turn-b").expect("by turn");
    assert_eq!(turn_b.len(), 1);
    assert_eq!(turn_b[0].call_id, 3);

    assert!(
        store
            .call_evidence_for_turn("turn-missing")
            .expect("by turn")
            .is_empty()
    );
}

/// Criterion 1: evidence is addressable by call, the way the transport
/// identifies a call — session + turn + per-turn `call_id`. A same `call_id` in
/// a different turn or session is a different call.
#[test]
fn evidence_is_addressable_by_call() {
    let store = CepAuditStore::in_memory().expect("store");
    let admission = admission(&["session.result"]);
    let effects = effects_record();
    let denied = empty_denials();
    for (session, turn) in [
        ("sess-1", "turn-a"),
        ("sess-1", "turn-b"),
        ("sess-2", "turn-a"),
    ] {
        store
            .record_call_evidence(&CallEvidence {
                turn_id: turn,
                session_id: session,
                call_id: 9,
                tool: "noop.echo",
                outcome: EvidenceOutcome::Completed,
                admission: &admission,
                effects: Some(&effects),
                denied: Some(&denied),
            })
            .expect("record");
    }

    let found = store
        .call_evidence_for_call("sess-1", "turn-a", 9)
        .expect("by call");
    assert_eq!(found.len(), 1, "call_id 9 is scoped by session and turn");
    assert_eq!(found[0].session_id, "sess-1");
    assert_eq!(found[0].turn_id, "turn-a");

    assert!(
        store
            .call_evidence_for_call("sess-1", "turn-a", 10)
            .expect("by call")
            .is_empty()
    );
}

/// Criterion 5: an admission failure is evidence, not absence. The refused call
/// has no effects at all but keeps the `AdmissionEvidence` that explains which
/// stage removed what.
#[test]
fn admission_denial_is_recorded_with_its_evidence() {
    let store = CepAuditStore::in_memory().expect("store");
    // A tool that was never enabled has an empty approved set (stage 1).
    let admission = AdmissionEvidence {
        version: ADMISSION_EVIDENCE_VERSION,
        tool: "noop.echo".into(),
        approved_capabilities: Vec::new(),
        denied_capabilities: Vec::new(),
        boundary_excluded: Vec::new(),
        max_boundary: Trust::In,
    };
    let effects = empty_effects();
    let denied = empty_denials();
    let id = store
        .record_call_evidence(&CallEvidence {
            turn_id: "turn-1",
            session_id: "sess-1",
            call_id: 4,
            tool: "noop.echo",
            outcome: EvidenceOutcome::AdmissionDenied,
            admission: &admission,
            effects: Some(&effects),
            denied: Some(&denied),
        })
        .expect("record");

    let stored = store.call_evidence(id).expect("read").expect("exists");
    assert_eq!(stored.outcome, EvidenceOutcome::AdmissionDenied);
    assert!(stored.admission.approved_capabilities.is_empty());
    assert!(
        stored.effects.effects.is_empty(),
        "a refused call produced no effect"
    );
    // The denial is queryable as a denial, not inferred from empty effects.
    let turn = store.call_evidence_for_turn("turn-1").expect("by turn");
    assert_eq!(turn.len(), 1);
    assert_eq!(turn[0].outcome, EvidenceOutcome::AdmissionDenied);
}

/// Criterion 5: per-operation refusals land as `DeniedOperation`, in `seq`
/// order, alongside the effects that did happen. The call itself succeeded.
#[test]
fn per_operation_denials_land_beside_the_effects_that_happened() {
    let store = CepAuditStore::in_memory().expect("store");
    let admission = admission(&["session.result"]);
    let effects = effects_record();
    let denied = denials();
    let id = store
        .record_call_evidence(&CallEvidence {
            turn_id: "turn-1",
            session_id: "sess-1",
            call_id: 5,
            tool: "noop.echo",
            outcome: EvidenceOutcome::Completed,
            admission: &admission,
            effects: Some(&effects),
            denied: Some(&denied),
        })
        .expect("record");

    let stored = store.call_evidence(id).expect("read").expect("exists");
    assert_eq!(stored.outcome, EvidenceOutcome::Completed);
    assert_eq!(stored.effects.effects.len(), 2);
    // Producer order was 3 then 2; the store returns seq order.
    let seqs: Vec<u32> = stored.denied.denied.iter().map(|d| d.seq).collect();
    assert_eq!(seqs, vec![2, 3]);
    assert_eq!(
        stored.denied.denied[0].target,
        ObservedTarget::Path {
            path: "src/lib.rs".into()
        }
    );
    assert_eq!(stored.denied.denied[1].target, ObservedTarget::None);
    // A refusal never becomes an effect.
    assert!(
        stored
            .effects
            .effects
            .iter()
            .all(|effect| effect.cell.ctx_op != CtxOp::Free)
    );
}

/// Criterion 5: a result-cell violation fails the call but retains the
/// operation effects the call had already recorded.
#[test]
fn failed_call_retains_the_operation_effects_it_already_recorded() {
    let store = CepAuditStore::in_memory().expect("store");
    let admission = admission(&["session.events"]);
    // Only the operation effect: the result effect is never recorded when the
    // result cell is refused.
    let effects = ObservedEffectsRecord {
        version: OBSERVED_EFFECTS_RECORD_VERSION,
        effects: vec![ObservedEffect {
            seq: 0,
            cell: cell(CtxOp::Mutate),
            scope: scope(),
            target: ObservedTarget::Session {
                turn_id: "turn-1".into(),
            },
            source: EffectSource::Operation,
            context_measure: None,
        }],
    };
    let denied = empty_denials();
    let id = store
        .record_call_evidence(&CallEvidence {
            turn_id: "turn-1",
            session_id: "sess-1",
            call_id: 6,
            tool: "noop.echo",
            outcome: EvidenceOutcome::Failed,
            admission: &admission,
            effects: Some(&effects),
            denied: Some(&denied),
        })
        .expect("record");

    let stored = store.call_evidence(id).expect("read").expect("exists");
    assert_eq!(stored.outcome, EvidenceOutcome::Failed);
    assert_eq!(stored.effects.effects.len(), 1);
    assert_eq!(stored.effects.effects[0].source, EffectSource::Operation);
    assert!(
        stored
            .effects
            .effects
            .iter()
            .all(|effect| effect.source != EffectSource::Result),
        "the refused result effect was never recorded"
    );
}

/// A cancelled call keeps the evidence it produced before the interrupt.
#[test]
fn cancelled_call_keeps_its_evidence() {
    let store = CepAuditStore::in_memory().expect("store");
    let admission = admission(&["session.events"]);
    let effects = effects_record();
    let denied = empty_denials();
    let id = store
        .record_call_evidence(&CallEvidence {
            turn_id: "turn-1",
            session_id: "sess-1",
            call_id: 8,
            tool: "noop.echo",
            outcome: EvidenceOutcome::Cancelled,
            admission: &admission,
            effects: Some(&effects),
            denied: Some(&denied),
        })
        .expect("record");

    let stored = store.call_evidence(id).expect("read").expect("exists");
    assert_eq!(stored.outcome, EvidenceOutcome::Cancelled);
    assert_eq!(stored.effects.effects.len(), 2);
}

/// Criterion 1: each record's `version` is validated on read. A row written by
/// a newer producer is refused with an actionable error instead of being parsed
/// under this binary's shape.
#[test]
fn unsupported_record_version_is_refused_on_read() {
    for (table, column) in [
        ("evidence_calls", "admission_version"),
        ("evidence_calls", "effects_version"),
        ("evidence_calls", "denials_version"),
    ] {
        let store = CepAuditStore::in_memory().expect("store");
        let admission = admission(&["session.result"]);
        let effects = effects_record();
        let denied = denials();
        let id = store
            .record_call_evidence(&CallEvidence {
                turn_id: "turn-1",
                session_id: "sess-1",
                call_id: 1,
                tool: "noop.echo",
                outcome: EvidenceOutcome::Completed,
                admission: &admission,
                effects: Some(&effects),
                denied: Some(&denied),
            })
            .expect("record");

        store
            .with_connection_for_tests(|conn| {
                conn.execute(
                    &format!("UPDATE {table} SET {column} = 99 WHERE evidence_id = ?1"),
                    params![id.0],
                )
                .map(|_| ())
            })
            .expect("bump version");

        let err = store
            .call_evidence(id)
            .expect_err("an unsupported version must be refused");
        match err {
            EvidenceError::UnsupportedRecordVersion {
                found, supported, ..
            } => {
                assert_eq!(found, 99);
                assert_eq!(supported, 1);
            }
            other => panic!("expected UnsupportedRecordVersion for {column}, got {other}"),
        }
        // The message names the action, not just the mismatch.
        let rendered = store.call_evidence(id).unwrap_err().to_string();
        assert!(
            rendered.contains("upgrade the binary or move the database aside"),
            "version error must be actionable: {rendered}"
        );
    }
}

/// A corrupt enum label is reported as an unreadable field, naming the column,
/// rather than silently coerced to a default.
#[test]
fn unreadable_column_value_is_reported_not_coerced() {
    let store = CepAuditStore::in_memory().expect("store");
    let admission = admission(&["session.result"]);
    let effects = effects_record();
    let denied = empty_denials();
    let id = store
        .record_call_evidence(&CallEvidence {
            turn_id: "turn-1",
            session_id: "sess-1",
            call_id: 1,
            tool: "noop.echo",
            outcome: EvidenceOutcome::Completed,
            admission: &admission,
            effects: Some(&effects),
            denied: Some(&denied),
        })
        .expect("record");

    store
        .with_connection_for_tests(|conn| {
            conn.execute(
                "UPDATE evidence_effects SET ctx_op = 'teleport' WHERE evidence_id = ?1 AND seq = 0",
                params![id.0],
            )
            .map(|_| ())
        })
        .expect("corrupt label");

    match store.call_evidence(id).expect_err("must be refused") {
        EvidenceError::UnreadableField { field, value, .. } => {
            assert_eq!(field, "ctx_op");
            assert_eq!(value, "teleport");
        }
        other => panic!("expected UnreadableField, got {other}"),
    }
}

/// Criterion 4: a database that already claims the evidence schema version but
/// whose evidence table shape disagrees is refused with an operator action, and
/// its history is left untouched.
#[test]
fn incompatible_evidence_table_shape_is_actionable_and_leaves_history() {
    let dir = tempfile::TempDir::new().expect("temp dir");
    let path = dir.path().join("audit.db");

    // A file stamped at the current version whose `evidence_calls` predates the
    // admission columns. `ALTER TABLE` cannot repair it: the missing columns are
    // NOT NULL.
    {
        let conn = rusqlite::Connection::open(&path).expect("open");
        conn.execute_batch(
            "CREATE TABLE cep_audit_entries (
                audit_id INTEGER PRIMARY KEY AUTOINCREMENT,
                call_id INTEGER NOT NULL,
                method TEXT NOT NULL,
                started_at TEXT NOT NULL,
                impact_kind TEXT NOT NULL,
                turn_id TEXT NOT NULL,
                session_id TEXT NOT NULL,
                semantic_stage TEXT NOT NULL,
                guard_stage TEXT NOT NULL,
                policy_enforced INTEGER NOT NULL
             );
             CREATE TABLE attestation_records (
                turn_id TEXT PRIMARY KEY,
                session_id TEXT NOT NULL,
                statement_json TEXT NOT NULL,
                predicate_type TEXT NOT NULL,
                generated_at TEXT NOT NULL,
                source_audit_range_or_hash TEXT NOT NULL
             );
             CREATE TABLE evidence_calls (
                evidence_id INTEGER PRIMARY KEY AUTOINCREMENT,
                turn_id TEXT NOT NULL
             );
             INSERT INTO cep_audit_entries
                (call_id, method, started_at, impact_kind, turn_id, session_id,
                 semantic_stage, guard_stage, policy_enforced)
             VALUES (1, 'noop.echo', '1', 'unknown', 't', 's', 'cell_component',
                     'cell_admission', 1);",
        )
        .expect("legacy shape");
        conn.pragma_update(None, "user_version", AUDIT_SCHEMA_VERSION)
            .expect("stamp version");
    }

    let err = match CepAuditStore::open(&path) {
        Ok(_) => panic!("a mismatched evidence shape must be refused"),
        Err(err) => err,
    };
    match err {
        AuditError::IncompatibleSchema { reason, .. } => {
            assert!(
                reason.contains("evidence table shape"),
                "reason must name the evidence shape: {reason}"
            );
            assert!(
                reason.contains("move it aside"),
                "reason must carry the operator action: {reason}"
            );
            for column in ["session_id", "admission_json"] {
                assert!(
                    reason.contains(column),
                    "reason must name the missing column {column}: {reason}"
                );
            }
        }
        other => panic!("expected IncompatibleSchema, got {other}"),
    }

    // History is untouched: the pre-existing audit row is still there.
    let conn = rusqlite::Connection::open(&path).expect("reopen");
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM cep_audit_entries", [], |row| {
            row.get(0)
        })
        .expect("count");
    assert_eq!(count, 1, "refusing to open must not drop history");
}

/// Criterion 4: a pre-#112 database (schema version 1) is adopted additively —
/// it gains the evidence tables on open and keeps its rows.
#[test]
fn pre_evidence_database_gains_the_tables_and_keeps_history() {
    let dir = tempfile::TempDir::new().expect("temp dir");
    let path = dir.path().join("audit.db");

    {
        let conn = rusqlite::Connection::open(&path).expect("open");
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
             );
             CREATE TABLE attestation_records (
                turn_id TEXT PRIMARY KEY,
                session_id TEXT NOT NULL,
                statement_json TEXT NOT NULL,
                predicate_type TEXT NOT NULL,
                generated_at TEXT NOT NULL,
                source_audit_range_or_hash TEXT NOT NULL
             );
             INSERT INTO cep_audit_entries
                (call_id, method, started_at, ended_at, result_or_error, impact_kind,
                 turn_id, session_id, semantic_stage, guard_stage, policy_enforced)
             VALUES (1, 'observe.workspace', '1', '2', 'success:ok',
                     'workspace_observation', 't', 's', 'cep_native',
                     'cep_policy_lifecycle', 1);",
        )
        .expect("v1 shape");
        conn.pragma_update(None, "user_version", 1i64)
            .expect("stamp v1");
    }

    let store = CepAuditStore::open(&path).expect("a v1 database is adopted");

    // The old row survived.
    let rows = store
        .query_rows(&crate::audit::AuditQueryFilters::default())
        .expect("query");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].method, "observe.workspace");

    // And the evidence tables are now usable.
    let admission = admission(&["session.result"]);
    let effects = effects_record();
    let denied = empty_denials();
    let id = store
        .record_call_evidence(&CallEvidence {
            turn_id: "t",
            session_id: "s",
            call_id: 2,
            tool: "noop.echo",
            outcome: EvidenceOutcome::Completed,
            admission: &admission,
            effects: Some(&effects),
            denied: Some(&denied),
        })
        .expect("evidence writes after upgrade");
    assert!(store.call_evidence(id).expect("read").is_some());

    // The stamped version moved forward exactly once.
    let conn = rusqlite::Connection::open(&path).expect("reopen");
    let version: i64 = conn
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .expect("version");
    assert_eq!(version, AUDIT_SCHEMA_VERSION);
}

/// Every #28 evidence scenario round-trips on the new table: the full allow
/// path, a partial deny, a tool-not-enabled refusal, and a multi-target denial
/// set. This is the ticket's "#28 scenarios on the new table" criterion.
#[test]
fn all_issue28_evidence_scenarios_round_trip() {
    let store = CepAuditStore::in_memory().expect("store");

    let allow_admission = admission(&["session.events", "session.result"]);
    let allow_effects = effects_record();
    let no_denials = empty_denials();

    let partial_admission = AdmissionEvidence {
        version: ADMISSION_EVIDENCE_VERSION,
        tool: "noop.echo".into(),
        // `session.events` subtracted by the profile deny list (stage 2).
        approved_capabilities: vec!["session.result".into()],
        denied_capabilities: vec!["session.events".into()],
        boundary_excluded: Vec::new(),
        max_boundary: Trust::In,
    };
    let partial_effects = ObservedEffectsRecord {
        version: OBSERVED_EFFECTS_RECORD_VERSION,
        effects: vec![ObservedEffect {
            seq: 1,
            cell: cell(CtxOp::Alloc),
            scope: scope(),
            target: ObservedTarget::Session {
                turn_id: "turn-1".into(),
            },
            source: EffectSource::Result,
            context_measure: Some(19),
        }],
    };
    let partial_denials = DeniedOperationsRecord {
        version: DENIED_OPERATIONS_RECORD_VERSION,
        denied: vec![DeniedOperation {
            seq: 0,
            cell: cell(CtxOp::Mutate),
            scope: scope(),
            target: ObservedTarget::Session {
                turn_id: "turn-1".into(),
            },
            reason: "capability not admitted".into(),
        }],
    };

    let boundary_admission = AdmissionEvidence {
        version: ADMISSION_EVIDENCE_VERSION,
        tool: "noop.echo".into(),
        approved_capabilities: Vec::new(),
        denied_capabilities: Vec::new(),
        // Removed by the `max_boundary` predicate (stage 3).
        boundary_excluded: vec!["session.result".into()],
        max_boundary: Trust::In,
    };
    let empty = empty_effects();

    // A denial set covering every `ObservedTarget` variant.
    let all_targets = DeniedOperationsRecord {
        version: DENIED_OPERATIONS_RECORD_VERSION,
        denied: vec![
            DeniedOperation {
                seq: 0,
                cell: cell(CtxOp::Mutate),
                scope: scope(),
                target: ObservedTarget::Path {
                    path: "src/lib.rs".into(),
                },
                reason: "path target refused".into(),
            },
            DeniedOperation {
                seq: 1,
                cell: cell(CtxOp::Mutate),
                scope: ObservedScope {
                    scope: "net".into(),
                    trust: Trust::Ex,
                    channel: ChannelKind::Network,
                },
                target: ObservedTarget::Host {
                    host: "example.com".into(),
                },
                reason: "host target refused".into(),
            },
            DeniedOperation {
                seq: 2,
                cell: cell(CtxOp::Mutate),
                scope: ObservedScope {
                    scope: "ws".into(),
                    trust: Trust::In,
                    channel: ChannelKind::Workspace,
                },
                target: ObservedTarget::None,
                reason: "no target".into(),
            },
        ],
    };

    let scenarios: Vec<(
        &str,
        EvidenceOutcome,
        &AdmissionEvidence,
        &ObservedEffectsRecord,
        &DeniedOperationsRecord,
    )> = vec![
        (
            "allow",
            EvidenceOutcome::Completed,
            &allow_admission,
            &allow_effects,
            &no_denials,
        ),
        (
            "partial-deny",
            EvidenceOutcome::Completed,
            &partial_admission,
            &partial_effects,
            &partial_denials,
        ),
        (
            "tool-not-enabled",
            EvidenceOutcome::AdmissionDenied,
            &boundary_admission,
            &empty,
            &no_denials,
        ),
        (
            "all-targets",
            EvidenceOutcome::Failed,
            &allow_admission,
            &empty,
            &all_targets,
        ),
    ];

    let mut ids = BTreeSet::new();
    for (index, (name, outcome, admission, effects, denied)) in scenarios.iter().enumerate() {
        let id = store
            .record_call_evidence(&CallEvidence {
                turn_id: "turn-1",
                session_id: "sess-1",
                call_id: index as u64,
                tool: "noop.echo",
                outcome: *outcome,
                admission,
                effects: Some(effects),
                denied: Some(denied),
            })
            .unwrap_or_else(|e| panic!("record {name}: {e}"));
        assert!(ids.insert(id), "each call gets a distinct identity");

        let stored = store
            .call_evidence(id)
            .unwrap_or_else(|e| panic!("read {name}: {e}"))
            .unwrap_or_else(|| panic!("{name} evidence missing"));
        assert_eq!(&stored.admission, *admission, "{name} admission");
        assert_eq!(&stored.effects, *effects, "{name} effects");
        assert_eq!(&stored.denied, *denied, "{name} denials");
        assert_eq!(stored.outcome, *outcome, "{name} outcome");
    }

    // All four are reachable through the turn index.
    let turn = store.call_evidence_for_turn("turn-1").expect("by turn");
    assert_eq!(turn.len(), scenarios.len());
}

/// Reading an identity that was never written is `None`, not an error.
#[test]
fn unknown_evidence_id_reads_as_none() {
    let store = CepAuditStore::in_memory().expect("store");
    assert!(
        store
            .call_evidence(EvidenceId(4242))
            .expect("read")
            .is_none()
    );
}
