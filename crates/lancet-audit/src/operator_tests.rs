// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use super::*;
use crate::invocation_store::{
    IndeterminateReason, NotExecutedReason, ReservationBinding, TerminalEvidence, TerminalMaterial,
    TerminalOutcome,
};
use lancet_protocol::{BindingIdentity, PlanKey};
use lattice_store::{ProjectStoreKey, StateLayout};
use tempfile::TempDir;

const SESSION: &str = "aaaaaaaa111144448888cccccccccccc";
const TURN: &str = "3f8b2c1a-7d4e-4f5a-8b9c-0d1e2f3a4b5c";

struct Fixture {
    _xdg: crate::test_support::ScopedXdgStateHome,
    project: TempDir,
    layout: StateLayout,
    key: ProjectStoreKey,
}

impl Fixture {
    fn new() -> Self {
        let xdg = crate::test_support::ScopedXdgStateHome::new();
        let project = tempfile::tempdir().expect("project");
        let layout = StateLayout::in_state_root(xdg.path());
        let key = ProjectStoreKey::derive(project.path()).expect("project key");
        InvocationStore::create(&layout, &key).expect("create invocation store");
        Self {
            _xdg: xdg,
            project,
            layout,
            key,
        }
    }

    fn store(&self) -> InvocationStore {
        InvocationStore::open(&self.layout, &self.key).expect("open invocation store")
    }

    fn request(&self) -> AuditRequest {
        AuditRequest::new(self.project.path())
    }
}

fn turn_id() -> TurnId {
    TurnId::parse(TURN).expect("turn")
}

fn session_id() -> SessionId {
    SessionId::parse(SESSION).expect("session")
}

fn binding_for_turn(turn_id: TurnId, step: u32, part: u32, method: &str) -> ReservationBinding {
    ReservationBinding {
        invocation_id: InvocationId::parse(&format!(
            "00000000-0000-4000-8000-{step:06x}{part:06x}"
        ))
        .expect("invocation"),
        identity: BindingIdentity {
            session_id: session_id(),
            turn_id,
            model_step_ordinal: step,
            part_ordinal: part,
            method: method.to_string(),
            descriptor_identity: "d".repeat(64),
            arguments_digest: [part as u8; 32],
        },
    }
}

fn binding(step: u32, part: u32, method: &str) -> ReservationBinding {
    binding_for_turn(turn_id(), step, part, method)
}

fn reserve(store: &InvocationStore, step: u32, bindings: &[ReservationBinding]) {
    store
        .reserve_batch(
            PlanKey {
                session_id: session_id(),
                turn_id: turn_id(),
                model_step_ordinal: step,
            },
            bindings,
        )
        .expect("reserve");
}

fn evidence() -> TerminalEvidence {
    TerminalEvidence {
        admission: lancet_protocol::AdmissionEvidence {
            version: lancet_protocol::ADMISSION_EVIDENCE_VERSION,
            tool: "test.echo".to_string(),
            approved_capabilities: vec!["session.read".to_string()],
            denied_capabilities: Vec::new(),
            boundary_excluded: Vec::new(),
            max_boundary: lancet_protocol::Trust::In,
        },
        effects: lancet_protocol::ObservedEffectsRecord {
            version: lancet_protocol::OBSERVED_EFFECTS_RECORD_VERSION,
            effects: Vec::new(),
        },
        denied: lancet_protocol::DeniedOperationsRecord {
            version: lancet_protocol::DENIED_OPERATIONS_RECORD_VERSION,
            denied: Vec::new(),
        },
    }
}

#[tokio::test]
async fn query_pins_current_state_and_includes_unfinalized_records() {
    let fixture = Fixture::new();
    let store = fixture.store();
    let a = binding(0, 0, "observe.workspace");
    let b = binding(0, 1, "mutate.edit");
    reserve(&store, 0, &[a.clone(), b.clone()]);
    store.dispatch_started(a.invocation_id).expect("dispatch");

    let mut request = fixture.request();
    request.session_id = Some(SESSION.to_string());
    let output = query_audit(request).await.expect("query");
    assert_eq!(output.row_count, 2);
    assert_eq!(output.rows[0].invocation_id, a.invocation_id);
    assert!(matches!(
        output.rows[0].record.state,
        InvocationState::DispatchStarted
    ));
    assert!(matches!(
        output.rows[1].record.state,
        InvocationState::Reserved
    ));
    assert_eq!(
        output.ledger_oid,
        store.pinned_view().unwrap().commit_oid.to_string()
    );

    let mut pinned = fixture.request();
    pinned.ledger_oid = Some(output.ledger_oid.clone());
    let same = query_audit(pinned).await.expect("pinned query");
    assert_eq!(same, output);
}

#[tokio::test]
async fn query_filters_typed_identity_method_and_terminal_verdict() {
    let fixture = Fixture::new();
    let store = fixture.store();
    let call = binding(2, 0, "fetch.web");
    reserve(&store, 2, std::slice::from_ref(&call));
    store.dispatch_started(call.invocation_id).unwrap();
    store
        .publish_terminal(
            call.invocation_id,
            TerminalOutcome::Rejected,
            TerminalMaterial::Unavailable {
                reason: crate::invocation_store::UnavailableMaterialReason::DraftPublicationFailed,
                digest: Some([7; 32]),
                byte_length: Some(99),
            },
            evidence(),
        )
        .unwrap();

    let mut request = fixture.request();
    request.session_id = Some(SESSION.to_string());
    request.turn_id = Some(TURN.to_string());
    request.method = Some("fetch.web".to_string());
    request.verdict = Some("rejected".to_string());
    let output = query_audit(request).await.unwrap();
    assert_eq!(output.rows.len(), 1);
    assert_eq!(output.rows[0].invocation_id, call.invocation_id);
}

#[tokio::test]
async fn same_parent_sibling_turn_ids_keep_operator_rows_isolated() {
    let fixture = Fixture::new();
    let store = fixture.store();
    let sibling = TurnId::parse("4f8b2c1a-7d4e-4f5a-8b9c-0d1e2f3a4b5c").unwrap();
    let first = binding_for_turn(turn_id(), 2, 0, "fetch.web");
    let second = binding_for_turn(sibling, 3, 0, "fetch.web");
    reserve(&store, 2, std::slice::from_ref(&first));
    store
        .reserve_batch(
            PlanKey {
                session_id: session_id(),
                turn_id: sibling,
                model_step_ordinal: 3,
            },
            std::slice::from_ref(&second),
        )
        .unwrap();

    let mut first_query = fixture.request();
    first_query.session_id = Some(SESSION.into());
    first_query.turn_id = Some(turn_id().to_string());
    let first_rows = query_audit(first_query).await.unwrap();
    let mut second_query = fixture.request();
    second_query.session_id = Some(SESSION.into());
    second_query.turn_id = Some(sibling.to_string());
    let second_rows = query_audit(second_query).await.unwrap();

    assert_eq!(first_rows.rows.len(), 1);
    assert_eq!(first_rows.rows[0].invocation_id, first.invocation_id);
    assert_eq!(second_rows.rows.len(), 1);
    assert_eq!(second_rows.rows[0].invocation_id, second.invocation_id);
}
#[tokio::test]
async fn tail_is_transition_ordered_and_cursor_exclusive() {
    let fixture = Fixture::new();
    let store = fixture.store();
    let a = binding(0, 0, "a");
    let b = binding(0, 1, "b");
    reserve(&store, 0, &[a.clone(), b.clone()]);
    store.dispatch_started(a.invocation_id).unwrap();

    let first = tail_audit(fixture.request()).await.expect("tail");
    assert_eq!(first.rows.len(), 3);
    assert_eq!(first.rows[0].invocation_id, a.invocation_id);
    assert_eq!(first.rows[1].invocation_id, b.invocation_id);
    assert_eq!(first.rows[0].ledger_oid, first.rows[1].ledger_oid);
    assert_eq!(first.rows[2].invocation_id, a.invocation_id);
    assert!(first.done);

    let mut continued = fixture.request();
    continued.after = first.last_cursor;
    let second = tail_audit(continued).await.expect("continued tail");
    assert!(second.rows.is_empty());
    assert!(second.done);
}

#[tokio::test]
async fn replay_uses_structural_order_and_typed_turn_identity() {
    let fixture = Fixture::new();
    let store = fixture.store();
    let late = binding(4, 1, "late");
    let early = binding(4, 0, "early");
    reserve(&store, 4, &[early.clone(), late.clone()]);

    let output = replay_audit(AuditReplayRequest::new(
        fixture.project.path(),
        SESSION,
        TURN,
    ))
    .await
    .expect("replay");
    assert_eq!(output.trace.session_id, session_id());
    assert_eq!(output.trace.turn_id, turn_id());
    assert_eq!(
        output
            .trace
            .calls
            .iter()
            .map(|row| row.invocation_id)
            .collect::<Vec<_>>(),
        vec![early.invocation_id, late.invocation_id]
    );
}

#[tokio::test]
async fn unreachable_and_malformed_cursors_fail_closed() {
    let fixture = Fixture::new();
    let mut malformed = fixture.request();
    malformed.ledger_oid = Some("not-an-oid".to_string());
    assert!(matches!(
        query_audit(malformed).await.unwrap_err(),
        OperatorAuditError::Config(_)
    ));

    let mut unreachable = fixture.request();
    unreachable.after = Some("1111111111111111111111111111111111111111".to_string());
    assert!(matches!(
        tail_audit(unreachable).await.unwrap_err(),
        OperatorAuditError::Internal(_)
    ));
}

#[tokio::test]
async fn operator_json_contains_digests_and_lengths_but_no_secret_material() {
    let fixture = Fixture::new();
    let store = fixture.store();
    let call = binding(1, 0, "secret.tool");
    reserve(&store, 1, std::slice::from_ref(&call));
    store.dispatch_started(call.invocation_id).unwrap();
    store
        .publish_terminal(
            call.invocation_id,
            TerminalOutcome::Succeeded,
            TerminalMaterial::Available {
                digest: [9; 32],
                byte_length: 65_537,
            },
            evidence(),
        )
        .unwrap();

    let output = query_audit(fixture.request()).await.unwrap();
    let json = serde_json::to_string(&output).unwrap();
    assert!(json.contains("65537"));
    assert!(json.contains(&"09".repeat(32)));
    for forbidden_key in [
        "arguments",
        "result_bytes",
        "provider_payload",
        "opaque_state",
        "metadata_json",
    ] {
        assert!(
            !json.contains(&format!("\"{forbidden_key}\":")),
            "operator JSON leaked key {forbidden_key}: {json}"
        );
    }
}

#[test]
fn state_labels_cover_closed_sums() {
    assert_eq!(
        invocation_state_label(&InvocationState::Terminal {
            outcome: TerminalOutcome::NotExecuted {
                reason: NotExecutedReason::BeforeDispatch,
            },
            error: None,
            material: TerminalMaterial::Unavailable {
                reason: crate::invocation_store::UnavailableMaterialReason::NotExecuted,
                digest: None,
                byte_length: None,
            },
            evidence: Box::new(evidence()),
        }),
        "not_executed"
    );
    assert_eq!(
        invocation_state_label(&InvocationState::Terminal {
            outcome: TerminalOutcome::Indeterminate {
                reason: IndeterminateReason::MayHaveOccurred,
            },
            error: None,
            material: TerminalMaterial::Unavailable {
                reason: crate::invocation_store::UnavailableMaterialReason::DraftPublicationFailed,
                digest: None,
                byte_length: None,
            },
            evidence: Box::new(evidence()),
        }),
        "indeterminate"
    );
}
