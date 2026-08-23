// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

// Private #139-B shell module: nothing routes through it until the Commit 6
// production switch, so the non-test lib build sees it as dead code.
#![allow(dead_code)]

//! The cross-domain ledger→reconciliation-facts projection (ADR 0005 §2.1,
//! GitHub #139-B Commit 1).
//!
//! Reconciliation is a pure decision function owned by `lattice-session`;
//! the invocation ledger is owned by `lancet-audit`; and
//! `lancet-audit -> lattice-session` must never exist. Composing the two is
//! therefore a `lattice-api` responsibility: this module projects one pinned
//! audit-domain [`PinnedLedgerView`] into the session-domain
//! [`InvocationFactsSnapshot`] the pure reducer consumes.
//!
//! This is the same projection the #139-A qualification seam
//! (`crates/lancet-audit/tests/invocation_qualification.rs`) exercises as a
//! dev-dependency; here it becomes the production composition point. The
//! mapping stays value-for-value: no fact is invented, dropped, or defaulted.
//! The view is already pinned to one immutable ledger commit, so projecting
//! performs no I/O and never re-opens or advances it.

use std::collections::BTreeMap;

use lancet_audit::invocation_store::{
    InvocationRecord, InvocationState, PinnedLedgerView, PlanReservation, ReservationBinding,
    TerminalMaterial, TerminalOutcome,
};
use lancet_protocol::{InvocationId, PlanKey};
use lattice_session::pending::{
    ExecutionOutcome, IndeterminateReason, InvocationFactsSnapshot, InvocationLifecycleFact,
    InvocationRecordFact, NotExecutedReason, PlannedBinding, ResultMaterial, binding_digest,
};

/// Project one exact plan plus every queried candidate from a pinned ledger
/// view into the pure reconciliation vocabulary.
///
/// Every queried candidate that has a record appears in the snapshot,
/// including records owned by other plans: the reducer distinguishes a clean
/// collision from corruption by owner comparison alone.
pub(crate) fn project_facts(
    view: &PinnedLedgerView,
    plan_key: PlanKey,
    candidate_ids: &[InvocationId],
) -> InvocationFactsSnapshot {
    let plan = view.plan(plan_key).map(project_plan);
    let invocations = view
        .query_invocations(candidate_ids)
        .into_iter()
        .map(|(id, record)| (id, project_record(record)))
        .collect::<BTreeMap<_, _>>();
    InvocationFactsSnapshot { plan, invocations }
}

fn project_plan(plan: &PlanReservation) -> lattice_session::pending::PlanFacts {
    lattice_session::pending::PlanFacts {
        plan_key: plan.plan_key,
        bindings: plan.bindings.iter().map(project_binding).collect(),
    }
}

fn project_binding(binding: &ReservationBinding) -> PlannedBinding {
    let identity = &binding.identity;
    let address = lancet_protocol::ToolCallAddress {
        message: lancet_protocol::ModelMessageAddress {
            session_id: identity.session_id,
            turn_id: identity.turn_id,
            model_step_ordinal: identity.model_step_ordinal,
        },
        part_ordinal: identity.part_ordinal,
    };
    PlannedBinding {
        invocation_id: binding.invocation_id,
        binding_digest: binding_digest(
            &binding.invocation_id,
            &address,
            &identity.method,
            &identity.descriptor_identity,
            &identity.arguments_digest,
        ),
        address,
        method: identity.method.clone(),
        descriptor_identity: identity.descriptor_identity.clone(),
        arguments_digest: identity.arguments_digest,
    }
}

fn project_record(record: InvocationRecord) -> InvocationRecordFact {
    InvocationRecordFact {
        owner: record.owner,
        lifecycle: match record.state {
            InvocationState::Reserved => InvocationLifecycleFact::Reserved,
            InvocationState::DispatchStarted => InvocationLifecycleFact::DispatchStarted,
            InvocationState::Terminal {
                outcome, material, ..
            } => InvocationLifecycleFact::Terminal {
                outcome: project_outcome(outcome),
                material: project_material(material),
            },
        },
    }
}

fn project_outcome(outcome: TerminalOutcome) -> ExecutionOutcome {
    match outcome {
        TerminalOutcome::Succeeded => ExecutionOutcome::Succeeded,
        TerminalOutcome::Failed => ExecutionOutcome::Failed,
        TerminalOutcome::Rejected => ExecutionOutcome::Rejected,
        TerminalOutcome::Cancelled => ExecutionOutcome::Cancelled,
        TerminalOutcome::NotExecuted { reason } => ExecutionOutcome::NotExecuted {
            reason: match reason {
                lancet_audit::invocation_store::NotExecutedReason::BeforeDispatch => {
                    NotExecutedReason::BeforeDispatch
                }
                lancet_audit::invocation_store::NotExecutedReason::CancelledBeforeDispatch => {
                    NotExecutedReason::CancelledBeforeDispatch
                }
                lancet_audit::invocation_store::NotExecutedReason::PrecedingInvocationIndeterminate => {
                    NotExecutedReason::PrecedingInvocationIndeterminate
                }
            },
        },
        TerminalOutcome::Indeterminate { reason } => ExecutionOutcome::Indeterminate {
            reason: match reason {
                lancet_audit::invocation_store::IndeterminateReason::MayHaveOccurred => {
                    IndeterminateReason::MayHaveOccurred
                }
            },
        },
    }
}

fn project_material(material: TerminalMaterial) -> ResultMaterial {
    match material {
        TerminalMaterial::Available {
            digest,
            byte_length,
        } => ResultMaterial::Available {
            digest,
            byte_length,
        },
        TerminalMaterial::Unavailable {
            reason,
            digest,
            byte_length,
        } => ResultMaterial::Unavailable {
            reason: match reason {
                lancet_audit::invocation_store::UnavailableMaterialReason::DraftPublicationFailed => {
                    lattice_session::pending::UnavailableMaterialReason::DraftPublicationFailed
                }
                lancet_audit::invocation_store::UnavailableMaterialReason::OutputTooLarge => {
                    lattice_session::pending::UnavailableMaterialReason::OutputTooLarge
                }
                lancet_audit::invocation_store::UnavailableMaterialReason::NotExecuted => {
                    lattice_session::pending::UnavailableMaterialReason::NotExecuted
                }
            },
            digest,
            byte_length,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lancet_audit::invocation_store::{BindingIdentity, LedgerSnapshot};
    use lancet_protocol::{ModelMessageAddress, SessionId, ToolCallAddress, TurnId};
    use lattice_conversation::arguments_digest;

    fn ids(count: usize) -> Vec<InvocationId> {
        (0..count)
            .map(|index| {
                InvocationId::parse(&format!("00000000-0000-4000-8000-{index:012x}")).unwrap()
            })
            .collect()
    }

    fn address(part: u32) -> ToolCallAddress {
        ToolCallAddress {
            message: ModelMessageAddress {
                session_id: SessionId::parse("3f8b2c1a7d4e4f5a8b9c0d1e2f3a4b5c").unwrap(),
                turn_id: TurnId::parse("3f8b2c1a-7d4e-4f5a-8b9c-0d1e2f3a4b5c").unwrap(),
                model_step_ordinal: 0,
            },
            part_ordinal: part,
        }
    }

    fn reservation_binding(invocation: InvocationId, part: u32) -> ReservationBinding {
        let addr = address(part);
        let args_digest = arguments_digest(&serde_json::json!({}));
        ReservationBinding {
            invocation_id: invocation,
            identity: BindingIdentity {
                session_id: addr.message.session_id,
                turn_id: addr.message.turn_id,
                model_step_ordinal: addr.message.model_step_ordinal,
                part_ordinal: addr.part_ordinal,
                method: "observe.workspace".to_string(),
                descriptor_identity: "d".repeat(64),
                arguments_digest: args_digest,
            },
        }
    }

    fn view_with(snapshot: LedgerSnapshot) -> PinnedLedgerView {
        PinnedLedgerView {
            commit_oid: gix::ObjectId::null(gix::hash::Kind::Sha1),
            snapshot,
        }
    }

    fn record(owner: PlanKey, state: InvocationState) -> (InvocationId, InvocationRecord) {
        let invocation = ids(1)[0];
        (
            invocation,
            InvocationRecord {
                owner,
                state,
                binding: reservation_binding(invocation, 0).identity,
            },
        )
    }

    #[test]
    fn empty_view_projects_to_empty_facts() {
        let view = view_with(LedgerSnapshot::default());
        let plan_key = PlanKey {
            turn_id: TurnId::parse("3f8b2c1a-7d4e-4f5a-8b9c-0d1e2f3a4b5c").unwrap(),
            model_step_ordinal: 0,
        };
        let facts = project_facts(&view, plan_key, &ids(2));
        assert_eq!(facts, InvocationFactsSnapshot::empty());
        assert!(facts.plan.is_none());
        assert!(facts.invocations.is_empty());
    }

    #[test]
    fn exact_reservation_projects_with_recomputed_binding_digests() {
        let plan_key = PlanKey {
            turn_id: TurnId::parse("3f8b2c1a-7d4e-4f5a-8b9c-0d1e2f3a4b5c").unwrap(),
            model_step_ordinal: 0,
        };
        let candidates = ids(2);
        let bindings = vec![
            reservation_binding(candidates[0], 0),
            reservation_binding(candidates[1], 1),
        ];
        let mut snapshot = LedgerSnapshot::default();
        snapshot.plans.push(PlanReservation {
            plan_key,
            bindings: bindings.clone(),
        });
        for binding in &bindings {
            snapshot.invocations.insert(
                binding.invocation_id,
                InvocationRecord {
                    owner: plan_key,
                    state: InvocationState::Reserved,
                    binding: binding.identity.clone(),
                },
            );
        }
        let view = view_with(snapshot);

        let facts = project_facts(&view, plan_key, &candidates);
        let plan = facts.plan.clone().expect("exact reservation projects");
        assert_eq!(plan.plan_key, plan_key);
        assert_eq!(plan.bindings.len(), 2);
        // The digest is recomputed from the projected identity, so a round
        // trip through the ledger preserves `PlannedBinding` equality.
        let expected: Vec<PlannedBinding> = bindings
            .iter()
            .map(|binding| project_binding(binding))
            .collect();
        assert_eq!(plan.bindings, expected);
        for candidate in &candidates {
            assert_eq!(
                facts.lifecycle_of(candidate),
                Some(&InvocationLifecycleFact::Reserved)
            );
        }
        // The projected snapshot validates as an exact match.
        assert!(matches!(
            facts.validate_against_plan(&plan_key, &plan.bindings),
            Ok(lattice_session::pending::PlanValidation::ExactMatch)
        ));
    }

    #[test]
    fn foreign_owned_candidate_is_preserved_for_collision_detection() {
        let plan_key = PlanKey {
            turn_id: TurnId::parse("3f8b2c1a-7d4e-4f5a-8b9c-0d1e2f3a4b5c").unwrap(),
            model_step_ordinal: 0,
        };
        let foreign_key = PlanKey {
            turn_id: TurnId::parse("aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee").unwrap(),
            model_step_ordinal: 3,
        };
        let candidates = ids(1);
        let mut snapshot = LedgerSnapshot::default();
        let (invocation, record) = record(foreign_key, InvocationState::DispatchStarted);
        assert_eq!(invocation, candidates[0]);
        snapshot.invocations.insert(invocation, record);
        let view = view_with(snapshot);

        let facts = project_facts(&view, plan_key, &candidates);
        assert!(facts.plan.is_none());
        assert_eq!(
            facts.lifecycle_of(&candidates[0]),
            Some(&InvocationLifecycleFact::DispatchStarted)
        );
        // The reducer sees the foreign owner and decides `Collision`.
        let projected = project_binding(&reservation_binding(candidates[0], 0));
        assert!(matches!(
            facts.validate_against_plan(&plan_key, &[projected]),
            Ok(lattice_session::pending::PlanValidation::Collision)
        ));
    }
}
