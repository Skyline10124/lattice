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
    DeniedOperation, DeniedOperationsRecord, EffectSource, MutationEvidence,
    MutationInstallation, MutationPrecondition, OBSERVED_EFFECTS_RECORD_VERSION,
    ObservedEffect, ObservedEffectsRecord, ObservedScope, ObservedTarget,
    WorkspaceVisibilityEvidence,
};
use rusqlite::params;

use crate::audit::{AUDIT_SCHEMA_VERSION, AuditError, CepAuditStore};
use crate::evidence::{
    CallEvidence, EVIDENCE_SCHEMA_VERSION_PRE_MUTATION, EVIDENCE_SCHEMA_VERSION_PRE_VISIBILITY,
    EvidenceError, EvidenceId, EvidenceOutcome,
};

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
                truncated: None,
                visibility: None,
                mutation: None,
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
                truncated: None,
                visibility: None,
                mutation: None,
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
                mutation: None,
            },
            DeniedOperation {
                seq: 2,
                cell: cell(CtxOp::Mutate),
                scope: scope(),
                target: ObservedTarget::Path {
                    path: "src/lib.rs".into(),
                },
                reason: "path target denied this cycle".into(),
                mutation: None,
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

#[test]
fn workspace_visibility_evidence_round_trips_in_queryable_columns() {
    let store = CepAuditStore::in_memory().expect("store");
    let admission = admission(&["workspace.observation"]);
    let visibility = WorkspaceVisibilityEvidence {
        included_ignored: true,
        filtered_system_exclusion: true,
        filtered_read_denied: false,
        filtered_ignored_default: true,
    };
    let effects = ObservedEffectsRecord {
        version: OBSERVED_EFFECTS_RECORD_VERSION,
        effects: vec![ObservedEffect {
            seq: 0,
            cell: Cell {
                ctx_op: CtxOp::Alloc,
                ext_impact: ExtImpact::Read,
                scope: "workspace".into(),
            },
            scope: ObservedScope {
                scope: "workspace".into(),
                trust: Trust::In,
                channel: ChannelKind::Workspace,
            },
            target: ObservedTarget::Path { path: "src".into() },
            source: EffectSource::Operation,
            context_measure: Some(12),
            truncated: Some(false),
            visibility: Some(visibility),
            mutation: None,
        }],
    };
    let denied = empty_denials();
    let id = store
        .record_call_evidence(&CallEvidence {
            turn_id: "turn-visible",
            session_id: "sess-1",
            call_id: 8,
            tool: "observe.workspace",
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
    assert_eq!(stored.effects.effects[0].visibility, Some(visibility));
    store
        .with_connection_for_tests(|conn| {
            conn.query_row(
                "SELECT visibility_included_ignored,
                        visibility_filtered_system_exclusion,
                        visibility_filtered_read_denied,
                        visibility_filtered_ignored_default
                 FROM evidence_effects WHERE evidence_id = ?1",
                params![id.0],
                |row| {
                    Ok((
                        row.get::<_, bool>(0)?,
                        row.get::<_, bool>(1)?,
                        row.get::<_, bool>(2)?,
                        row.get::<_, bool>(3)?,
                    ))
                },
            )
            .map(|stored| assert_eq!(stored, (true, true, false, true)))
        })
        .expect("queryable visibility columns");
}


#[test]
fn mutation_evidence_round_trips_in_queryable_columns() {
    let store = CepAuditStore::in_memory().expect("store");
    let admission = admission(&["workspace.mutation"]);
    let mutation = MutationEvidence {
        before_revision: None,
        after_revision: Some("blake3:after".into()),
        precondition: MutationPrecondition::Satisfied,
        installation: MutationInstallation::Installed,
    };
    let effects = ObservedEffectsRecord {
        version: OBSERVED_EFFECTS_RECORD_VERSION,
        effects: vec![ObservedEffect {
            seq: 0,
            cell: Cell {
                ctx_op: CtxOp::Mutate,
                ext_impact: ExtImpact::Write,
                scope: "workspace".into(),
            },
            scope: ObservedScope {
                scope: "workspace".into(),
                trust: Trust::In,
                channel: ChannelKind::Workspace,
            },
            target: ObservedTarget::Path {
                path: "fresh.txt".into(),
            },
            source: EffectSource::Operation,
            context_measure: Some(6),
            truncated: None,
            visibility: None,
            mutation: Some(mutation.clone()),
        }],
    };
    let denied = DeniedOperationsRecord {
        version: DENIED_OPERATIONS_RECORD_VERSION,
        denied: vec![DeniedOperation {
            seq: 1,
            cell: Cell {
                ctx_op: CtxOp::Mutate,
                ext_impact: ExtImpact::Write,
                scope: "workspace".into(),
            },
            scope: ObservedScope {
                scope: "workspace".into(),
                trust: Trust::In,
                channel: ChannelKind::Workspace,
            },
            target: ObservedTarget::Path {
                path: "notes.txt".into(),
            },
            reason: "stale-revision".into(),
            mutation: Some(MutationEvidence {
                before_revision: Some("blake3:current".into()),
                after_revision: None,
                precondition: MutationPrecondition::StaleRevision,
                installation: MutationInstallation::NotAttempted,
            }),
        }],
    };
    let id = store
        .record_call_evidence(&CallEvidence {
            turn_id: "turn-mutate",
            session_id: "sess-1",
            call_id: 11,
            tool: "mutate.edit",
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
    assert_eq!(stored.effects.effects[0].mutation, Some(mutation));
    assert_eq!(
        stored.denied.denied[0].mutation.as_ref().unwrap().precondition,
        MutationPrecondition::StaleRevision
    );

    store
        .with_connection_for_tests(|conn| {
            conn.query_row(
                "SELECT mutation_before_revision, mutation_after_revision,
                        mutation_precondition, mutation_installation
                 FROM evidence_effects WHERE evidence_id = ?1",
                params![id.0],
                |row| {
                    Ok((
                        row.get::<_, Option<String>>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                    ))
                },
            )
            .map(|stored| {
                assert_eq!(
                    stored,
                    (None, Some("blake3:after".into()), "satisfied".into(), "installed".into())
                )
            })?;
            conn.query_row(
                "SELECT mutation_before_revision, mutation_after_revision,
                        mutation_precondition, mutation_installation
                 FROM evidence_denials WHERE evidence_id = ?1",
                params![id.0],
                |row| {
                    Ok((
                        row.get::<_, Option<String>>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                    ))
                },
            )
            .map(|stored| {
                assert_eq!(
                    stored,
                    (
                        Some("blake3:current".into()),
                        None,
                        "stale_revision".into(),
                        "not_attempted".into()
                    )
                )
            })
        })
        .expect("queryable mutation columns");
}

#[test]
fn pre_mutation_database_gains_queryable_columns_and_keeps_evidence() {
    let dir = tempfile::TempDir::new().expect("temp dir");
    let path = dir.path().join("audit.db");
    let id = {
        let store = CepAuditStore::open(&path).expect("create current store");
        store
            .record_call_evidence(&CallEvidence {
                turn_id: "turn-before-mutation",
                session_id: "sess-1",
                call_id: 12,
                tool: "noop.echo",
                outcome: EvidenceOutcome::Completed,
                admission: &admission(&["session.result"]),
                effects: Some(&effects_record()),
                denied: Some(&empty_denials()),
            })
            .expect("record legacy evidence")
    };

    {
        let conn = rusqlite::Connection::open(&path).expect("open for downgrade fixture");
        for table in ["evidence_effects", "evidence_denials"] {
            for column in [
                "mutation_before_revision",
                "mutation_after_revision",
                "mutation_precondition",
                "mutation_installation",
            ] {
                conn.execute(
                    &format!("ALTER TABLE {table} DROP COLUMN {column}"),
                    [],
                )
                .expect("remove post-v4 column");
            }
        }
        conn.pragma_update(None, "user_version", EVIDENCE_SCHEMA_VERSION_PRE_MUTATION)
            .expect("stamp v4 fixture");
    }

    let store = CepAuditStore::open(&path).expect("v4 store upgrades additively");
    let stored = store
        .call_evidence(id)
        .expect("read legacy evidence")
        .expect("evidence remains");
    assert_eq!(stored.effects.effects[0].mutation, None);

    store
        .with_connection_for_tests(|conn| {
            for table in ["evidence_effects", "evidence_denials"] {
                let mut present = BTreeSet::new();
                conn.pragma(None, "table_info", table, |row| {
                    present.insert(row.get::<_, String>("name")?);
                    Ok(())
                })?;
                for column in [
                    "mutation_before_revision",
                    "mutation_after_revision",
                    "mutation_precondition",
                    "mutation_installation",
                ] {
                    assert!(present.contains(column), "migration must add {table}.{column}");
                }
            }
            Ok(())
        })
        .expect("inspect upgraded shape");
}

#[test]
fn corrupt_mutation_enum_label_is_reported_not_coerced() {
    let store = CepAuditStore::in_memory().expect("store");
    let admission = admission(&["workspace.mutation"]);
    let effects = ObservedEffectsRecord {
        version: OBSERVED_EFFECTS_RECORD_VERSION,
        effects: vec![ObservedEffect {
            seq: 0,
            cell: Cell {
                ctx_op: CtxOp::Mutate,
                ext_impact: ExtImpact::Write,
                scope: "workspace".into(),
            },
            scope: ObservedScope {
                scope: "workspace".into(),
                trust: Trust::In,
                channel: ChannelKind::Workspace,
            },
            target: ObservedTarget::Path {
                path: "a.txt".into(),
            },
            source: EffectSource::Operation,
            context_measure: Some(1),
            truncated: None,
            visibility: None,
            mutation: Some(MutationEvidence {
                before_revision: None,
                after_revision: Some("blake3:x".into()),
                precondition: MutationPrecondition::Satisfied,
                installation: MutationInstallation::Installed,
            }),
        }],
    };
    let id = store
        .record_call_evidence(&CallEvidence {
            turn_id: "turn-corrupt",
            session_id: "sess-1",
            call_id: 13,
            tool: "mutate.create",
            outcome: EvidenceOutcome::Completed,
            admission: &admission,
            effects: Some(&effects),
            denied: Some(&empty_denials()),
        })
        .expect("record");

    store
        .with_connection_for_tests(|conn| {
            conn.execute(
                "UPDATE evidence_effects SET mutation_precondition = 'teleport' WHERE evidence_id = ?1",
                params![id.0],
            )
            .map(|_| ())
        })
        .expect("corrupt label");

    let err = store.call_evidence(id).expect_err("corrupt label");
    assert!(
        matches!(
            err,
            EvidenceError::UnreadableField {
                field: "mutation_precondition",
                ..
            }
        ),
        "{err:?}"
    );
}

#[test]
fn pre_visibility_database_gains_queryable_columns_and_keeps_evidence() {
    let dir = tempfile::TempDir::new().expect("temp dir");
    let path = dir.path().join("audit.db");
    let id = {
        let store = CepAuditStore::open(&path).expect("create current store");
        store
            .record_call_evidence(&CallEvidence {
                turn_id: "turn-before-visibility",
                session_id: "sess-1",
                call_id: 9,
                tool: "noop.echo",
                outcome: EvidenceOutcome::Completed,
                admission: &admission(&["session.result"]),
                effects: Some(&effects_record()),
                denied: Some(&empty_denials()),
            })
            .expect("record legacy evidence")
    };

    {
        let conn = rusqlite::Connection::open(&path).expect("open for downgrade fixture");
        for column in [
            "visibility_included_ignored",
            "visibility_filtered_system_exclusion",
            "visibility_filtered_read_denied",
            "visibility_filtered_ignored_default",
        ] {
            conn.execute(
                &format!("ALTER TABLE evidence_effects DROP COLUMN {column}"),
                [],
            )
            .expect("remove post-v3 column");
        }
        conn.pragma_update(None, "user_version", EVIDENCE_SCHEMA_VERSION_PRE_VISIBILITY)
            .expect("stamp v3 fixture");
    }

    let store = CepAuditStore::open(&path).expect("v3 store upgrades additively");
    let stored = store
        .call_evidence(id)
        .expect("read legacy evidence")
        .expect("evidence remains");
    assert_eq!(stored.effects.effects[0].visibility, None);

    store
        .with_connection_for_tests(|conn| {
            let mut present = BTreeSet::new();
            conn.pragma(None, "table_info", "evidence_effects", |row| {
                present.insert(row.get::<_, String>("name")?);
                Ok(())
            })?;
            for column in [
                "visibility_included_ignored",
                "visibility_filtered_system_exclusion",
                "visibility_filtered_read_denied",
                "visibility_filtered_ignored_default",
            ] {
                assert!(present.contains(column), "migration must add {column}");
            }
            Ok(())
        })
        .expect("inspect upgraded shape");
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
            truncated: None,
            visibility: None,
            mutation: None,
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

/// The per-entry `record_version` columns gate too. They are written on every
/// insert, so a column that is never read back would leave a future v2 effect
/// shape interpretable as v1 — the exact mis-parse the version field prevents.
#[test]
fn unsupported_entry_version_is_refused_on_read() {
    for (table, record) in [
        ("evidence_effects", "observed_effect"),
        ("evidence_denials", "denied_operation"),
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
                    &format!("UPDATE {table} SET record_version = 99 WHERE evidence_id = ?1"),
                    params![id.0],
                )
                .map(|_| ())
            })
            .expect("bump entry version");

        match store.call_evidence(id).expect_err("must be refused") {
            EvidenceError::UnsupportedRecordVersion {
                record: got,
                found,
                supported,
                ..
            } => {
                assert_eq!(got, record);
                assert_eq!(found, 99);
                assert_eq!(supported, 1);
            }
            other => panic!("expected UnsupportedRecordVersion for {table}, got {other}"),
        }
    }
}

/// A NULL version column is not "the current version" — it is a row whose
/// version is unknown, which is what the check exists to refuse. The producer
/// always writes both columns, so NULL means a foreign writer or corruption.
#[test]
fn missing_record_version_is_refused_on_read() {
    for column in ["effects_version", "denials_version"] {
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
                    &format!("UPDATE evidence_calls SET {column} = NULL WHERE evidence_id = ?1"),
                    params![id.0],
                )
                .map(|_| ())
            })
            .expect("null the version");

        let err = store.call_evidence(id).expect_err("must be refused");
        assert!(
            matches!(err, EvidenceError::MissingRecordVersion { .. }),
            "a NULL {column} must be refused, got {err}"
        );
        assert!(
            err.to_string().contains("move the database aside"),
            "the error must carry an operator action: {err}"
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
        conn.pragma_update(
            None,
            "user_version",
            crate::audit::AUDIT_SCHEMA_VERSION_PRE_EVIDENCE,
        )
        .expect("stamp the pre-evidence version");
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
            truncated: None,
            visibility: None,
            mutation: None,
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
            mutation: None,
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
                mutation: None,
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
                mutation: None,
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
                mutation: None,
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

/// A legacy import must not carry an `evidence_id` into the new store. The id is
/// a per-store AUTOINCREMENT rowid restarting at 1, so a kept pointer would let a
/// historical row resolve to an unrelated later call's evidence.
#[test]
fn legacy_import_drops_the_stale_evidence_pointer() {
    let dir = tempfile::TempDir::new().expect("temp dir");
    let legacy_path = dir.path().join("legacy.db");
    let fresh_path = dir.path().join("fresh.db");

    // A legacy store with one Component row pointing at its own evidence id 1.
    {
        let legacy = CepAuditStore::open(&legacy_path).expect("legacy store");
        let ctx = crate::audit::CepAuditContext {
            turn_id: "old-turn".into(),
            session_id: "old-sess".into(),
            ..Default::default()
        };
        let start = legacy
            .start_call(
                &ctx,
                1,
                "noop.echo",
                &crate::audit::CallAuditMetadata::for_component_dispatch(),
            )
            .expect("start");
        legacy
            .finalize_call(
                start,
                crate::audit::AuditOutcome::Success {
                    summary: "ok".into(),
                },
                None,
                Some(r#"{"evidence_id":1,"component_cells":["alloc:none:session:in"]}"#),
            )
            .expect("finalize");
    }

    // A fresh store imports it, then records evidence of its own — which takes
    // id 1, the very id the imported row used to point at.
    let fresh = CepAuditStore::open(&fresh_path).expect("fresh store");
    fresh
        .import_legacy_read_only_if_empty(&legacy_path)
        .expect("import");

    let admission = admission(&["session.result"]);
    let effects = effects_record();
    let denied = empty_denials();
    let new_id = fresh
        .record_call_evidence(&CallEvidence {
            turn_id: "new-turn",
            session_id: "new-sess",
            call_id: 9,
            tool: "BRAND-NEW-TOOL",
            outcome: EvidenceOutcome::Completed,
            admission: &admission,
            effects: Some(&effects),
            denied: Some(&denied),
        })
        .expect("record");
    assert_eq!(new_id, EvidenceId(1), "the fixture depends on id reuse");

    let rows = fresh
        .query_rows(&crate::audit::AuditQueryFilters::default())
        .expect("query");
    let imported = rows
        .iter()
        .find(|row| row.turn_id == "old-turn")
        .expect("the imported row survived");
    let metadata: serde_json::Value =
        serde_json::from_str(imported.metadata_json.as_deref().expect("metadata"))
            .expect("metadata is JSON");
    assert!(
        metadata.get("evidence_id").is_none(),
        "an imported row must not point at this store's evidence: {metadata}"
    );
    // The rest of the metadata is preserved.
    assert_eq!(
        metadata["component_cells"],
        serde_json::json!(["alloc:none:session:in"])
    );
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
