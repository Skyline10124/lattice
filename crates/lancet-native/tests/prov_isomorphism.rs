// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Supplementary tests: projection idempotence after canonicalization.
//!
//! This suite proves `project_rows` is stable when applied twice to the same audit
//! rows (modulo timestamps / PID metadata). It is **weaker** than
//! `attestation_matches_replay_statement`, which compares stored vs replay-derived
//! in-toto statements after a live turn.

use lancet_catalog::CepMethod;
use lancet_catalog::methods::workspace::{Create, Edit};
use lancet_native::audit::{CallAuditMetadata, CepAuditContext, CepAuditStore};
use lancet_native::provenance::{
    EvidenceLookup, canonicalize_prov_fragment, project_rows, project_rows_with_evidence,
};
use lancet_native::{CallEvidence, EvidenceId, EvidenceOutcome};
use lancet_protocol::cell::{Cell, ChannelKind, CtxOp, ExtImpact, Trust};
use lancet_protocol::evidence::{
    ADMISSION_EVIDENCE_VERSION, AdmissionEvidence, DENIED_OPERATIONS_RECORD_VERSION,
    DeniedOperation, DeniedOperationsRecord, EffectSource, OBSERVED_EFFECTS_RECORD_VERSION,
    ObservedEffect, ObservedEffectsRecord, ObservedScope, ObservedTarget,
};

/// `observe.workspace` as a plain method name.
///
/// It is a Component Tool since #30, so there is no catalog type to take `NAME`
/// from. These tests use it as an audit-row subject — PROV projection reads the
/// stored method name and never consults the catalog — so a literal is the
/// honest spelling.
const OBSERVE_WORKSPACE: &str = "observe.workspace";

fn store_row(
    audit: &CepAuditStore,
    ctx: &CepAuditContext,
    call_id: u64,
    method: &str,
    metadata: &str,
) {
    let start = audit
        .start_call(
            ctx,
            call_id,
            method,
            &CallAuditMetadata::native_policy_enforced(Some("started")),
        )
        .expect("start_call");
    audit
        .finalize_call(
            start,
            lancet_native::audit::AuditOutcome::Success {
                summary: format!("{method} ok"),
            },
            Some("native_handler"),
            Some(metadata),
        )
        .expect("finalize_call");
}

fn base_ctx(session: &str, turn: &str) -> CepAuditContext {
    CepAuditContext {
        turn_id: turn.into(),
        session_id: session.into(),
        model_id: "prov-model".into(),
        runtime_version: "lattice-test/1".into(),
    }
}

fn assert_projection_idempotent(rows: &[lancet_native::audit::AuditRow]) {
    let a = canonicalize_prov_fragment(&project_rows(rows));
    let b = canonicalize_prov_fragment(&project_rows(rows));
    assert_eq!(a, b, "double projection must match after canonicalize");
}

#[test]
fn read_only_turn_projection_is_isomorphic() {
    let audit = CepAuditStore::in_memory().expect("audit");
    let ctx = base_ctx("sess-prov", "turn-read");
    store_row(
        &audit,
        &ctx,
        1,
        OBSERVE_WORKSPACE,
        r#"{"required_capability":"read_path","content_hash":"sha256:read1"}"#,
    );
    store_row(
        &audit,
        &ctx,
        2,
        OBSERVE_WORKSPACE,
        r#"{"required_capability":"inspect","canonical_target":"src"}"#,
    );
    let rows = audit
        .query_rows(&lancet_native::AuditQueryFilters {
            session_id: Some(ctx.session_id.clone()),
            turn_id: Some(ctx.turn_id.clone()),
            ..Default::default()
        })
        .expect("query");
    assert_projection_idempotent(&rows);
}

#[test]
fn mutation_turn_projection_is_isomorphic() {
    let audit = CepAuditStore::in_memory().expect("audit");
    let ctx = base_ctx("sess-prov", "turn-mut");
    store_row(
        &audit,
        &ctx,
        1,
        "session.envelope",
        r#"{"required_capability":"mutate_session_state"}"#,
    );
    store_row(
        &audit,
        &ctx,
        2,
        Create::NAME,
        r#"{"required_capability":"write_path","bytes_written":12}"#,
    );
    store_row(
        &audit,
        &ctx,
        3,
        Edit::NAME,
        r#"{"required_capability":"write_path","changed_files":["src/a.rs"]}"#,
    );
    let rows = audit
        .query_rows(&lancet_native::AuditQueryFilters {
            session_id: Some(ctx.session_id.clone()),
            turn_id: Some(ctx.turn_id.clone()),
            ..Default::default()
        })
        .expect("query");
    assert_projection_idempotent(&rows);
}

/// #28 B6/B11: a Component turn's row carries "why authorized" apart from
/// "what happened", and both reach the PROV fragment as separate activity
/// attributes. The projection stays idempotent with the new keys present.
#[test]
fn component_turn_projection_separates_admission_from_effects() {
    let audit = CepAuditStore::in_memory().expect("audit");
    let ctx = base_ctx("sess-prov", "turn-component");
    store_row(
        &audit,
        &ctx,
        1,
        "noop.echo",
        // Since #112 the audit row points at the evidence table; the records
        // themselves are written below.
        r#"{"evidence_id": 1, "component_cells": ["alloc:none:session:in"]}"#,
    );
    // The evidence row the metadata points at: one refused operation and one
    // measured result, exactly as a partial-deny Component call produces.
    let admission = AdmissionEvidence {
        version: ADMISSION_EVIDENCE_VERSION,
        tool: "noop.echo".into(),
        approved_capabilities: vec!["session.result".into()],
        denied_capabilities: vec!["session.events".into()],
        boundary_excluded: Vec::new(),
        max_boundary: Trust::In,
    };
    let scope = ObservedScope {
        scope: "session".into(),
        trust: Trust::In,
        channel: ChannelKind::Session,
    };
    let target = ObservedTarget::Session {
        turn_id: ctx.turn_id.clone(),
    };
    let effects = ObservedEffectsRecord {
        version: OBSERVED_EFFECTS_RECORD_VERSION,
        effects: vec![ObservedEffect {
            seq: 1,
            cell: Cell {
                ctx_op: CtxOp::Alloc,
                ext_impact: ExtImpact::None,
                scope: "session".into(),
            },
            scope: scope.clone(),
            target: target.clone(),
            source: EffectSource::Result,
            context_measure: Some(18),
            // A result effect applies no bound, so it has no truncation verdict
            // to report (#30).
            truncated: None,
            visibility: None,
        }],
    };
    let denied = DeniedOperationsRecord {
        version: DENIED_OPERATIONS_RECORD_VERSION,
        denied: vec![DeniedOperation {
            seq: 0,
            cell: Cell {
                ctx_op: CtxOp::Mutate,
                ext_impact: ExtImpact::None,
                scope: "session".into(),
            },
            scope,
            target,
            reason: "cell mutate:none:session is not covered by an admitted capability".into(),
        }],
    };
    let evidence_id = audit
        .record_call_evidence(&CallEvidence {
            turn_id: &ctx.turn_id,
            session_id: &ctx.session_id,
            call_id: 1,
            tool: "noop.echo",
            outcome: EvidenceOutcome::Completed,
            admission: &admission,
            effects: Some(&effects),
            denied: Some(&denied),
        })
        .expect("record evidence");
    assert_eq!(evidence_id, EvidenceId(1), "the row's pointer must resolve");

    let rows = audit
        .query_rows(&lancet_native::AuditQueryFilters {
            session_id: Some(ctx.session_id.clone()),
            turn_id: Some(ctx.turn_id.clone()),
            ..Default::default()
        })
        .expect("query");

    let resolve = |id: EvidenceId| audit.call_evidence(id).ok().flatten();
    let lookup = EvidenceLookup::new(&resolve);
    let attributes = &project_rows_with_evidence(&rows, &lookup).activities[0].attributes;
    assert_eq!(
        attributes["admission_evidence"]["approved_capabilities"],
        serde_json::json!(["session.result"])
    );
    assert_eq!(
        attributes["observed_effects"]["effects"][0]["cell"]["ctx_op"],
        serde_json::json!("alloc")
    );
    assert_eq!(
        attributes["denied_operations"]["denied"][0]["cell"]["ctx_op"],
        serde_json::json!("mutate")
    );
    assert_projection_idempotent(&rows);
}

#[test]
fn mixed_turn_projection_is_isomorphic() {
    let audit = CepAuditStore::in_memory().expect("audit");
    let ctx = base_ctx("sess-prov", "turn-mixed");
    store_row(
        &audit,
        &ctx,
        1,
        OBSERVE_WORKSPACE,
        r#"{"required_capability":"read_path","content_hash":"sha256:m1"}"#,
    );
    store_row(
        &audit,
        &ctx,
        2,
        Edit::NAME,
        r#"{"required_capability":"write_path","changed_files":["b.txt"]}"#,
    );
    let rows = audit
        .query_rows(&lancet_native::AuditQueryFilters {
            session_id: Some(ctx.session_id.clone()),
            turn_id: Some(ctx.turn_id.clone()),
            ..Default::default()
        })
        .expect("query");
    assert_projection_idempotent(&rows);
}
