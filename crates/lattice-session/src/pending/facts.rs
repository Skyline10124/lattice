// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Invocation facts snapshot (ADR 0005 §11/§14, GitHub #139-A).
//!
//! Reconciliation is a pure decision function over a Pending snapshot and a
//! pinned Invocation facts snapshot. The #139-A qualification InvocationStore
//! projects into this vocabulary; the reducer never queries the store directly.
//! The type is deliberately owned here, not in `lancet-audit`, so the
//! reducer's input contract stays a session-domain type and any invocation-store
//! adapter maps into it rather than the reverse.
//!
//! `Absent` is a query result, not a record: an invocation with no entry in
//! [`InvocationFactsSnapshot::invocations`] is absent. A partial plan
//! (some invocations recorded without a plan reservation) violates the
//! all-or-nothing reservation contract and fails closed as corruption.

use std::collections::{BTreeMap, BTreeSet};

use lancet_protocol::{InvocationId, PlanKey};
use serde::{Deserialize, Serialize};

use super::binding::PlannedBinding;

/// The recorded reservation of one plan, when any reservation exists.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanFacts {
    /// The reserved plan's structural identity.
    pub plan_key: PlanKey,
    /// The exact ordered bindings the reservation recorded.
    pub bindings: Vec<PlannedBinding>,
}

/// One invocation's recorded ledger entry, including its owning plan
/// (ADR 0005 §11, #139-A decision D-A1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InvocationRecordFact {
    /// The plan that owns this invocation. Mandatory — the store always
    /// records the owning `PlanKey` at reservation time.
    pub owner: PlanKey,
    /// Lifecycle state.
    pub lifecycle: InvocationLifecycleFact,
}

/// One invocation's lifecycle state (ADR 0005 §11).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InvocationLifecycleFact {
    /// Reserved, not dispatched.
    Reserved,
    /// Durable before executor handoff; the conservative "may have
    /// occurred" boundary. No `HandoffCompleted` state exists.
    DispatchStarted,
    /// Terminal: closed execution outcome plus independent result material.
    Terminal {
        /// Closed-sum execution outcome.
        outcome: ExecutionOutcome,
        /// Independently available or unavailable result material.
        material: ResultMaterial,
    },
}

/// Closed sum of Tool execution outcomes (ADR 0005 §11) with typed reasons.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionOutcome {
    /// The Tool executed and returned a result.
    Succeeded,
    /// The Tool executed and produced a failure result.
    Failed,
    /// The call was rejected before execution.
    Rejected,
    /// Execution was cancelled after dispatch.
    Cancelled,
    /// The call never dispatched; typed reason says why.
    NotExecuted {
        /// Typed closure reason.
        reason: NotExecutedReason,
    },
    /// The system cannot determine whether execution occurred.
    Indeterminate {
        /// Typed indeterminacy reason.
        reason: IndeterminateReason,
    },
}

/// Typed reason a call never dispatched (ADR 0005 §13).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotExecutedReason {
    /// A `BindingPlanned`/`Bound` closure-only path: reopen never
    /// dispatches.
    BeforeDispatch,
    /// Cancellation was first observed at this reserved invocation before
    /// dispatch. Later bindings closed structurally use [`Self::BeforeDispatch`].
    CancelledBeforeDispatch,
    /// An earlier invocation in the ordered batch went indeterminate, so
    /// this one never ran.
    PrecedingInvocationIndeterminate,
}

/// Typed reason execution is indeterminate (ADR 0005 §12).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IndeterminateReason {
    /// `DispatchStarted` is durable with no terminal fact: execution may
    /// have occurred.
    MayHaveOccurred,
}

/// Model-visible result material, independent of execution outcome
/// (ADR 0005 §11): a successful execution can have unavailable material
/// without being relabeled as an execution failure.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResultMaterial {
    /// Material exists, identified by digest and byte length.
    Available {
        /// Digest of the canonical content.
        #[serde(with = "super::hex_digest")]
        digest: [u8; 32],
        /// Canonical byte length.
        byte_length: u64,
    },
    /// No model-visible material; typed reason plus optional digest/length.
    Unavailable {
        /// Typed unavailability reason.
        reason: UnavailableMaterialReason,
        /// Digest of the full content, when it was computed.
        #[serde(default, with = "super::hex_digest_option")]
        digest: Option<[u8; 32]>,
        /// Full content byte length, when known.
        #[serde(default)]
        byte_length: Option<u64>,
    },
}

/// Typed reason result material is unavailable (ADR 0005 §12).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnavailableMaterialReason {
    /// Draft publication failed cleanly (or the draft is provably absent);
    /// the outcome remains the true execution outcome.
    DraftPublicationFailed,
    /// Canonical content exceeded the ToolContent limit; the full digest
    /// and length stay honest without storing the bytes.
    OutputTooLarge,
    /// The invocation never dispatched, so there is no material to make
    /// model-visible (closure-only paths).
    NotExecuted,
}

/// Result of [`InvocationFactsSnapshot::validate_against_plan`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanValidation {
    /// No reservation for this PlanKey, no candidate IDs in store.
    AbsentPlan,
    /// No reservation for this PlanKey, but at least one candidate ID is
    /// held by another plan. The reducer should revise candidates.
    Collision,
    /// Exact reservation matches. Idempotent retry.
    ExactMatch,
}

/// A pinned snapshot of the invocation facts for one pending Turn's plan.
///
/// `invocations` may include records owned by **other** plans when the
/// queried candidate IDs collide with existing reservations.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct InvocationFactsSnapshot {
    /// The recorded reservation for this Turn's plan, when any exists.
    pub plan: Option<PlanFacts>,
    /// Recorded invocation states, including any foreign-owned collisions.
    /// No entry means absent.
    pub invocations: BTreeMap<InvocationId, InvocationRecordFact>,
}

impl InvocationFactsSnapshot {
    /// An empty snapshot: no reservation, no invocations.
    pub fn empty() -> Self {
        Self::default()
    }

    /// The recorded lifecycle of one invocation; `None` means absent.
    pub fn lifecycle_of(&self, invocation: &InvocationId) -> Option<&InvocationLifecycleFact> {
        self.invocations.get(invocation).map(|r| &r.lifecycle)
    }

    /// The full record of one invocation including owner; `None` means absent.
    pub fn record_of(&self, invocation: &InvocationId) -> Option<&InvocationRecordFact> {
        self.invocations.get(invocation)
    }

    /// Validate the snapshot against the all-or-nothing reservation
    /// contract and detect clean UUID collisions (#139-A decision D-A2).
    ///
    /// Returns [`PlanValidation`] on success or
    /// [`SessionError::JournalCorruption`] for any invariant violation.
    ///
    /// The snapshot must describe exactly the queried candidate set. A
    /// duplicate candidate in the durable plan, or a record for an
    /// invocation the projection was never asked about, means the facts do
    /// not describe this plan and fail closed. Combined with the
    /// per-binding presence check below, [`PlanValidation::ExactMatch`]
    /// therefore requires `invocations.keys()` to equal the plan's exact
    /// candidate set (#139-A).
    pub fn validate_against_plan(
        &self,
        plan_key: &PlanKey,
        bindings: &[PlannedBinding],
    ) -> Result<PlanValidation, crate::SessionError> {
        let mut candidates = BTreeSet::new();
        for binding in bindings {
            if !candidates.insert(binding.invocation_id) {
                return Err(crate::SessionError::JournalCorruption {
                    detail: format!(
                        "duplicate candidate invocation {} in the durable plan",
                        binding.invocation_id
                    ),
                });
            }
        }
        if let Some(extra) = self.invocations.keys().find(|id| !candidates.contains(id)) {
            return Err(crate::SessionError::JournalCorruption {
                detail: format!("invocation {extra} is recorded outside the queried candidate set"),
            });
        }
        match &self.plan {
            None => {
                // No reservation for this PlanKey. Check candidate IDs.
                let mut has_foreign = false;
                let mut has_own = false;
                for binding in bindings {
                    if let Some(record) = self.invocations.get(&binding.invocation_id) {
                        if record.owner == *plan_key {
                            has_own = true;
                        } else {
                            has_foreign = true;
                        }
                    }
                }
                if has_own {
                    // Truth table #3: own-PlanKey records without reservation.
                    Err(crate::SessionError::JournalCorruption {
                        detail: "invocation records owned by this plan exist \
                                 without a plan reservation"
                            .to_string(),
                    })
                } else if has_foreign {
                    // Truth table #2: clean collision.
                    Ok(PlanValidation::Collision)
                } else {
                    // Truth table #1: fresh reservation.
                    Ok(PlanValidation::AbsentPlan)
                }
            }
            Some(recorded) => {
                if &recorded.plan_key != plan_key || recorded.bindings != bindings {
                    // Truth table #7: different bindings.
                    return Err(crate::SessionError::JournalCorruption {
                        detail: "recorded reservation differs from the durable exact plan"
                            .to_string(),
                    });
                }
                for binding in bindings {
                    match self.invocations.get(&binding.invocation_id) {
                        None => {
                            // Truth table #5: partial reservation.
                            return Err(crate::SessionError::JournalCorruption {
                                detail: format!(
                                    "partial reservation: invocation {} has no record",
                                    binding.invocation_id
                                ),
                            });
                        }
                        Some(record) if record.owner != *plan_key => {
                            // Truth table #6: foreign ID in exact reservation.
                            return Err(crate::SessionError::JournalCorruption {
                                detail: format!(
                                    "invocation {} owned by foreign plan in exact reservation",
                                    binding.invocation_id
                                ),
                            });
                        }
                        Some(_) => {} // correct owner
                    }
                }
                // Truth table #4: exact match.
                Ok(PlanValidation::ExactMatch)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lancet_protocol::{SessionId, TurnId};

    fn plan_key() -> PlanKey {
        PlanKey {
            turn_id: TurnId::parse("3f8b2c1a-7d4e-4f5a-8b9c-0d1e2f3a4b5c").unwrap(),
            model_step_ordinal: 0,
        }
    }

    fn other_plan_key() -> PlanKey {
        PlanKey {
            turn_id: TurnId::parse("aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee").unwrap(),
            model_step_ordinal: 0,
        }
    }

    fn record(owner: PlanKey, lifecycle: InvocationLifecycleFact) -> InvocationRecordFact {
        InvocationRecordFact { owner, lifecycle }
    }

    fn bindings(count: usize) -> Vec<PlannedBinding> {
        (0..count)
            .map(|index| {
                let invocation =
                    InvocationId::parse(&format!("00000000-0000-4000-8000-{index:012x}")).unwrap();
                let address = lancet_protocol::ToolCallAddress {
                    message: lancet_protocol::ModelMessageAddress {
                        session_id: SessionId::parse("3f8b2c1a7d4e4f5a8b9c0d1e2f3a4b5c").unwrap(),
                        turn_id: plan_key().turn_id,
                        model_step_ordinal: 0,
                    },
                    part_ordinal: index as u32,
                };
                let args_digest = lattice_conversation::arguments_digest(&serde_json::json!({}));
                PlannedBinding {
                    binding_digest: PlannedBinding::compute_binding_digest(
                        &invocation,
                        &address,
                        "t",
                        "d",
                        &args_digest,
                    ),
                    invocation_id: invocation,
                    address,
                    method: "t".to_string(),
                    descriptor_identity: "d".to_string(),
                    arguments_digest: args_digest,
                }
            })
            .collect()
    }

    // --- Truth table #1: absent plan, all candidates absent ---

    #[test]
    fn absent_plan_is_valid_when_nothing_is_recorded() {
        let facts = InvocationFactsSnapshot::empty();
        assert!(facts.plan.is_none());
        assert!(matches!(
            facts.validate_against_plan(&plan_key(), &bindings(2)),
            Ok(PlanValidation::AbsentPlan),
        ));
    }

    // --- Truth table #2: absent plan, foreign-owned candidates ---

    #[test]
    fn collision_detected_when_candidate_owned_by_another_plan() {
        let plan = bindings(2);
        let mut facts = InvocationFactsSnapshot::empty();
        facts.invocations.insert(
            plan[0].invocation_id,
            record(other_plan_key(), InvocationLifecycleFact::Reserved),
        );
        assert!(matches!(
            facts.validate_against_plan(&plan_key(), &plan),
            Ok(PlanValidation::Collision),
        ));
    }

    // --- Truth table #3: absent plan, own-PlanKey orphan records ---

    #[test]
    fn orphan_own_records_without_reservation_is_corruption() {
        let plan = bindings(1);
        let mut facts = InvocationFactsSnapshot::empty();
        facts.invocations.insert(
            plan[0].invocation_id,
            record(plan_key(), InvocationLifecycleFact::Reserved),
        );
        assert!(facts.validate_against_plan(&plan_key(), &plan).is_err());
    }

    // --- Truth table #4: exact match ---

    #[test]
    fn exact_reservation_validates() {
        let plan = bindings(2);
        let mut facts = InvocationFactsSnapshot::empty();
        facts.plan = Some(PlanFacts {
            plan_key: plan_key(),
            bindings: plan.clone(),
        });
        for binding in &plan {
            facts.invocations.insert(
                binding.invocation_id,
                record(plan_key(), InvocationLifecycleFact::Reserved),
            );
        }
        assert!(matches!(
            facts.validate_against_plan(&plan_key(), &plan),
            Ok(PlanValidation::ExactMatch),
        ));
    }

    // --- Truth table #5: exact match, partial ---

    #[test]
    fn partial_reservation_fails_closed() {
        let plan = bindings(2);
        let mut facts = InvocationFactsSnapshot::empty();
        facts.plan = Some(PlanFacts {
            plan_key: plan_key(),
            bindings: plan.clone(),
        });
        facts.invocations.insert(
            plan[0].invocation_id,
            record(plan_key(), InvocationLifecycleFact::Reserved),
        );
        assert!(facts.validate_against_plan(&plan_key(), &plan).is_err());
    }

    // --- Truth table #6: exact match, foreign owner ---

    #[test]
    fn foreign_owner_in_exact_reservation_is_corruption() {
        let plan = bindings(2);
        let mut facts = InvocationFactsSnapshot::empty();
        facts.plan = Some(PlanFacts {
            plan_key: plan_key(),
            bindings: plan.clone(),
        });
        facts.invocations.insert(
            plan[0].invocation_id,
            record(plan_key(), InvocationLifecycleFact::Reserved),
        );
        facts.invocations.insert(
            plan[1].invocation_id,
            record(other_plan_key(), InvocationLifecycleFact::Reserved),
        );
        assert!(facts.validate_against_plan(&plan_key(), &plan).is_err());
    }

    // --- Truth table #7: different bindings ---

    #[test]
    fn different_plan_bindings_fails() {
        let plan = bindings(2);
        let mut facts = InvocationFactsSnapshot::empty();
        facts.plan = Some(PlanFacts {
            plan_key: plan_key(),
            bindings: plan.clone(),
        });
        for binding in &plan {
            facts.invocations.insert(
                binding.invocation_id,
                record(plan_key(), InvocationLifecycleFact::Reserved),
            );
        }
        let mut different = plan.clone();
        different[0].descriptor_identity = "other".to_string();
        assert!(
            facts
                .validate_against_plan(&plan_key(), &different)
                .is_err()
        );
    }

    // --- Lifecycle accessors ---

    #[test]
    fn lifecycle_of_reports_absent_without_a_record() {
        let facts = InvocationFactsSnapshot::empty();
        let invocation = InvocationId::parse("00000000-0000-4000-8000-000000000001").unwrap();
        assert!(facts.lifecycle_of(&invocation).is_none());
        let mut recorded = facts.clone();
        recorded.invocations.insert(
            invocation,
            record(plan_key(), InvocationLifecycleFact::DispatchStarted),
        );
        assert_eq!(
            recorded.lifecycle_of(&invocation),
            Some(&InvocationLifecycleFact::DispatchStarted),
        );
    }

    // --- Records without plan (no foreign owner) ---

    #[test]
    fn records_without_any_plan_reservation_are_corruption() {
        let plan = bindings(1);
        let mut orphan = InvocationFactsSnapshot::empty();
        orphan.invocations.insert(
            plan[0].invocation_id,
            record(plan_key(), InvocationLifecycleFact::Reserved),
        );
        assert!(orphan.validate_against_plan(&plan_key(), &plan).is_err());
    }

    // --- Exact key-set equality (extra / duplicate records) ---

    #[test]
    fn extra_own_record_in_exact_reservation_is_corruption() {
        let plan = bindings(2);
        let mut facts = InvocationFactsSnapshot::empty();
        facts.plan = Some(PlanFacts {
            plan_key: plan_key(),
            bindings: plan.clone(),
        });
        for binding in &plan {
            facts.invocations.insert(
                binding.invocation_id,
                record(plan_key(), InvocationLifecycleFact::Reserved),
            );
        }
        // One own-owned record that is not a candidate of the durable plan.
        let extra = bindings(3)[2].invocation_id;
        facts
            .invocations
            .insert(extra, record(plan_key(), InvocationLifecycleFact::Reserved));
        assert!(facts.validate_against_plan(&plan_key(), &plan).is_err());
    }

    #[test]
    fn extra_foreign_record_in_exact_reservation_is_corruption() {
        let plan = bindings(2);
        let mut facts = InvocationFactsSnapshot::empty();
        facts.plan = Some(PlanFacts {
            plan_key: plan_key(),
            bindings: plan.clone(),
        });
        for binding in &plan {
            facts.invocations.insert(
                binding.invocation_id,
                record(plan_key(), InvocationLifecycleFact::Reserved),
            );
        }
        let extra = bindings(3)[2].invocation_id;
        facts.invocations.insert(
            extra,
            record(other_plan_key(), InvocationLifecycleFact::Reserved),
        );
        assert!(facts.validate_against_plan(&plan_key(), &plan).is_err());
    }

    #[test]
    fn extra_record_outside_the_candidate_set_without_a_plan_is_corruption() {
        let plan = bindings(1);
        let mut facts = InvocationFactsSnapshot::empty();
        let extra = bindings(3)[2].invocation_id;
        facts.invocations.insert(
            extra,
            record(other_plan_key(), InvocationLifecycleFact::Reserved),
        );
        assert!(facts.validate_against_plan(&plan_key(), &plan).is_err());
    }

    #[test]
    fn duplicate_candidate_ids_in_the_plan_are_corruption() {
        let mut plan = bindings(2);
        plan[1].invocation_id = plan[0].invocation_id;
        let mut facts = InvocationFactsSnapshot::empty();
        facts.plan = Some(PlanFacts {
            plan_key: plan_key(),
            bindings: plan.clone(),
        });
        facts.invocations.insert(
            plan[0].invocation_id,
            record(plan_key(), InvocationLifecycleFact::Reserved),
        );
        assert!(facts.validate_against_plan(&plan_key(), &plan).is_err());
    }
}
