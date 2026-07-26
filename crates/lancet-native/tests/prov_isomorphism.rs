// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Supplementary tests: projection idempotence after canonicalization.
//!
//! This suite proves `project_rows` is stable when applied twice to the same audit
//! rows (modulo timestamps / PID metadata). It is **weaker** than
//! `attestation_matches_replay_statement`, which compares stored vs replay-derived
//! in-toto statements after a live turn.

use lancet_catalog::CepMethod;
use lancet_catalog::methods::workspace::{Create, Edit, ObserveWorkspace};
use lancet_native::audit::{CallAuditMetadata, CepAuditContext, CepAuditStore};
use lancet_native::provenance::{canonicalize_prov_fragment, project_rows};

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
        ObserveWorkspace::NAME,
        r#"{"required_capability":"read_path","content_hash":"sha256:read1"}"#,
    );
    store_row(
        &audit,
        &ctx,
        2,
        ObserveWorkspace::NAME,
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
        r#"{
            "admission_evidence": {
                "version": 1,
                "tool": "noop.echo",
                "approved_capabilities": ["session.result"],
                "denied_capabilities": ["session.events"],
                "boundary_excluded": [],
                "max_boundary": "in"
            },
            "observed_effects": {
                "version": 1,
                "effects": [{
                    "seq": 1,
                    "cell": {"ctx_op": "alloc", "ext_impact": "none", "scope": "session"},
                    "scope": {"scope": "session", "trust": "in", "channel": "session"},
                    "target": {"kind": "session", "turn_id": "turn-component"},
                    "source": "result",
                    "context_measure": 18
                }]
            },
            "denied_operations": [{
                "seq": 0,
                "cell": {"ctx_op": "mutate", "ext_impact": "none", "scope": "session"},
                "scope": {"scope": "session", "trust": "in", "channel": "session"},
                "target": {"kind": "session", "turn_id": "turn-component"},
                "reason": "cell mutate:none:session is not covered by an admitted capability"
            }],
            "component_cells": ["alloc:none:session:in"]
        }"#,
    );
    let rows = audit
        .query_rows(&lancet_native::AuditQueryFilters {
            session_id: Some(ctx.session_id.clone()),
            turn_id: Some(ctx.turn_id.clone()),
            ..Default::default()
        })
        .expect("query");

    let attributes = &project_rows(&rows).activities[0].attributes;
    assert_eq!(
        attributes["admission_evidence"]["approved_capabilities"],
        serde_json::json!(["session.result"])
    );
    assert_eq!(
        attributes["observed_effects"]["effects"][0]["cell"]["ctx_op"],
        serde_json::json!("alloc")
    );
    assert_eq!(
        attributes["denied_operations"][0]["cell"]["ctx_op"],
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
        ObserveWorkspace::NAME,
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
