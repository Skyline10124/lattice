// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Synthetic Invocation facts snapshot (ADR 0005 §11/§14, GitHub #144).
//!
//! Reconciliation is a pure decision function over a Pending snapshot and a
//! pinned Invocation facts snapshot. Until the production InvocationStore
//! lands (#139-A qualification, #139-B hard cut), this vocabulary is the
//! session-side synthetic projection those tests run against: a pinned
//! query of one plan's reservation and per-invocation ledger state. It is
//! deliberately owned here, not in `lancet-audit`, so the reducer's input
//! contract stays a session-domain type and the production store maps into
//! it rather than the reverse.
//!
//! `Absent` is a query result, not a record: an invocation with no entry in
//! [`InvocationFactsSnapshot::invocations`] is absent. A partial plan
//! (some invocations recorded without a plan reservation) violates the
//! all-or-nothing reservation contract and fails closed as corruption.

use std::collections::BTreeMap;

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

/// One invocation's recorded lifecycle state (ADR 0005 §11).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InvocationFact {
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
    /// Cancellation reached a reserved invocation before dispatch.
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

/// A pinned snapshot of the invocation facts for one pending Turn's plan.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct InvocationFactsSnapshot {
    /// The recorded reservation for this Turn's plan, when any exists.
    pub plan: Option<PlanFacts>,
    /// Recorded invocation states. No entry means absent.
    pub invocations: BTreeMap<InvocationId, InvocationFact>,
}

impl InvocationFactsSnapshot {
    /// An empty snapshot: no reservation, no invocations.
    pub fn empty() -> Self {
        Self::default()
    }

    /// The recorded state of one invocation; `None` means absent (a query
    /// result, not a record).
    pub fn state_of(&self, invocation: &InvocationId) -> Option<&InvocationFact> {
        self.invocations.get(invocation)
    }

    /// Validate the snapshot against the all-or-nothing reservation
    /// contract: either no plan and no invocation records, or a plan whose
    /// bindings are exactly the recorded invocations. A partial or
    /// different plan is corruption (ADR 0005 §11/§14).
    pub fn validate_against_plan(
        &self,
        plan_key: &PlanKey,
        bindings: &[PlannedBinding],
    ) -> Result<(), crate::SessionError> {
        match (&self.plan, self.invocations.is_empty()) {
            (None, true) => Ok(()), // absent plan
            (None, false) => Err(crate::SessionError::JournalCorruption {
                detail: "invocation records exist without a plan reservation".to_string(),
            }),
            (Some(recorded), _) => {
                if &recorded.plan_key != plan_key || recorded.bindings != bindings {
                    return Err(crate::SessionError::JournalCorruption {
                        detail: "recorded reservation differs from the durable exact plan"
                            .to_string(),
                    });
                }
                for binding in bindings {
                    if !self.invocations.contains_key(&binding.invocation_id) {
                        return Err(crate::SessionError::JournalCorruption {
                            detail: format!(
                                "partial reservation: invocation {} has no record",
                                binding.invocation_id
                            ),
                        });
                    }
                }
                Ok(())
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

    #[test]
    fn absent_plan_is_valid_when_nothing_is_recorded() {
        let facts = InvocationFactsSnapshot::empty();
        assert!(facts.plan.is_none());
        assert!(
            facts
                .validate_against_plan(&plan_key(), &bindings(2))
                .is_ok()
        );
    }

    #[test]
    fn partial_reservation_fails_closed() {
        let plan = bindings(2);
        let mut facts = InvocationFactsSnapshot::empty();
        facts.plan = Some(PlanFacts {
            plan_key: plan_key(),
            bindings: plan.clone(),
        });
        facts
            .invocations
            .insert(plan[0].invocation_id, InvocationFact::Reserved);
        assert!(facts.validate_against_plan(&plan_key(), &plan).is_err());

        // Records without any plan reservation are corruption too.
        let mut orphan = InvocationFactsSnapshot::empty();
        orphan
            .invocations
            .insert(plan[0].invocation_id, InvocationFact::Reserved);
        assert!(orphan.validate_against_plan(&plan_key(), &plan).is_err());
    }

    #[test]
    fn exact_reservation_validates_and_different_plan_fails() {
        let plan = bindings(2);
        let mut facts = InvocationFactsSnapshot::empty();
        facts.plan = Some(PlanFacts {
            plan_key: plan_key(),
            bindings: plan.clone(),
        });
        for binding in &plan {
            facts
                .invocations
                .insert(binding.invocation_id, InvocationFact::Reserved);
        }
        assert!(facts.validate_against_plan(&plan_key(), &plan).is_ok());

        let mut different = plan.clone();
        different[0].descriptor_identity = "other".to_string();
        assert!(
            facts
                .validate_against_plan(&plan_key(), &different)
                .is_err()
        );
    }

    #[test]
    fn state_of_reports_absent_without_a_record() {
        let facts = InvocationFactsSnapshot::empty();
        let invocation = InvocationId::parse("00000000-0000-4000-8000-000000000001").unwrap();
        assert!(facts.state_of(&invocation).is_none());
        let mut recorded = facts.clone();
        recorded
            .invocations
            .insert(invocation, InvocationFact::DispatchStarted);
        assert_eq!(
            recorded.state_of(&invocation),
            Some(&InvocationFact::DispatchStarted)
        );
    }
}
