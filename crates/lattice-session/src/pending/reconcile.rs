// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! The pure reconciliation decision function (ADR 0005 §14, GitHub #144).
//!
//! `reconcile(PendingSnapshot, InvocationFactsSnapshot)` decides the one
//! closure of a pending Turn from durable facts only: it never sends a
//! provider request, reruns a completion hook, redispatches a Tool, changes
//! an uncertain binding identity, or guesses success or failure. Every
//! durable stage has exactly one closure; I/O executes the returned
//! [`ReconciliationPlan`] through narrow domain ports (the store's journal
//! publications, the synthetic facts port, and the final SessionStore
//! transaction).
//!
//! The TurnClosing candidate constructed here pins the durable journal
//! tip's own committer time as `created_at` — recovery introduces no new
//! clock. Its usage aggregate is conservatively `Partial`: the tip alone
//! can never prove that every attempt reported usage; the live coordinator
//! derives `Complete | Partial` from the full attempt chain.

use lancet_protocol::{InvocationId, PlanKey};
use lattice_conversation::{
    AttemptRejection, MessageSemantic, NativeMessage, TokenUsage, ToolResultOutcome,
    UnboundToolCall,
};
use serde::{Deserialize, Serialize};

use super::binding::{BindingPlan, PlannedBinding};
use super::config::TurnStartedFacts;
use super::contract::{PendingSnapshot, PendingStage, TurnCommitCandidate};
use super::encode_hex;
use super::facts::{
    ExecutionOutcome, IndeterminateReason, InvocationFactsSnapshot, InvocationLifecycleFact,
    NotExecutedReason, PlanValidation, ResultMaterial, UnavailableMaterialReason,
};
use super::outcome::{
    ClosureOrigin, TurnFailure, TurnFinishReason, TurnInterruption, TurnOutcome, TurnUsageAggregate,
};
use super::step::{ClosedBinding, MaterialAvailability, StepClosedKind, StepClosingState};
use crate::graph::GraphMessage;
use crate::{Result, SessionError};

/// One closure step the executor performs. Actions are ordered; the
/// executor replays them against the journal and the synthetic facts port.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReconciliationAction {
    /// Closure-only reservation of the exact durable plan (ADR 0005 §9: a
    /// crash at `BindingPlanned` with the plan absent may reserve the same
    /// candidates; it never creates new ones or executes a Tool).
    ReserveExactPlan {
        /// The plan's structural identity.
        plan_key: PlanKey,
        /// The exact ordered bindings to reserve.
        bindings: Vec<PlannedBinding>,
    },
    /// Terminalize one invocation in the facts port with a closed-sum
    /// outcome and independent result material.
    TerminalizeInvocation {
        /// The invocation to terminalize.
        invocation: InvocationId,
        /// The terminal execution outcome.
        outcome: ExecutionOutcome,
        /// The terminal result material.
        material: ResultMaterial,
    },
    /// Publish the `Bound` journal snapshot after the exact plan is
    /// durably reserved.
    PublishBound {
        /// The byte-equal exact plan.
        plan: BindingPlan,
    },
    /// Publish one `StepClosing` snapshot (the cursor-0 baseline after
    /// `Bound`, or one ordered closure advance).
    AdvanceStepClosing {
        /// The next cursor state; the store validates the transition.
        state: StepClosingState,
    },
    /// Publish the `StepClosed` snapshot.
    PublishStepClosed {
        /// The closed sum kind.
        kind: StepClosedKind,
    },
    /// Pin and publish the `TurnClosing` snapshot from the constructed
    /// candidate (all inputs durable; `created_at` is the tip's committer
    /// time).
    PublishTurnClosing {
        /// The exact candidate to pin.
        candidate: TurnCommitCandidate,
    },
    /// Retry the exact final SessionStore ref transaction (idempotent).
    FinalizeTurn,
}

/// The complete closure plan for one pending Turn.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReconciliationPlan {
    /// Owning session.
    pub session_id: lancet_protocol::SessionId,
    /// The Turn being closed.
    pub turn_id: lancet_protocol::TurnId,
    /// The one typed outcome this closure produces.
    pub outcome: TurnOutcome,
    /// Always `Reconciled`: this plan is a reopen closure; only a live
    /// owner produces `Live` outcomes.
    pub origin: ClosureOrigin,
    /// Ordered actions for the executor.
    pub actions: Vec<ReconciliationAction>,
}

/// A same-stage `BindingPlanned` candidate revision (ADR 0005 §9).
///
/// This is **not** a closure: the Turn is still open, no `TurnOutcome`
/// exists, and nothing caller-visible may be produced until the revision is
/// durable. It carries the exact identities the executor needs — session,
/// Turn, and the byte-exact durable plan whose candidates collided — so the
/// executor can verify the request against the pinned `active-turn` tip
/// before CAS-publishing the replacement.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CandidateRevisionRequest {
    /// Owning session.
    pub session_id: lancet_protocol::SessionId,
    /// The Turn whose binding plan must be revised.
    pub turn_id: lancet_protocol::TurnId,
    /// The exact durable `BindingPlanned` plan the revision replaces. The
    /// executor must find this byte-equal at the expected `active-turn`
    /// tip; only candidate IDs and their derived binding digests change.
    pub plan: BindingPlan,
    /// The candidate IDs the ledger records as owned by another plan, in
    /// binding order. Evidence of the observation, not an executor input.
    pub colliding_candidates: Vec<InvocationId>,
}

/// The one decision the reducer produces for a durable Pending stage.
///
/// Closing a Turn and revising colliding candidates are different kinds of
/// answer, so they are different types: a revision carries no `TurnOutcome`
/// and can never be mistaken for a closed Turn.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReconciliationDecision {
    /// The Turn closes; execute this exact plan.
    Closure(ReconciliationPlan),
    /// The candidate IDs are held by another plan. The I/O executor
    /// generates fresh UUIDs and CAS-publishes a complete replacement
    /// `BindingPlanned` revision; only then does a closure decision exist
    /// (#139-A decision D-A2, truth table #2).
    ReviseCandidates(CandidateRevisionRequest),
}

impl ReconciliationDecision {
    /// The closure plan, or `None` when a candidate revision must happen
    /// first.
    #[must_use]
    pub fn closure(&self) -> Option<&ReconciliationPlan> {
        match self {
            Self::Closure(plan) => Some(plan),
            Self::ReviseCandidates(_) => None,
        }
    }

    /// The candidate revision request, or `None` when the Turn closes.
    #[must_use]
    pub fn revision(&self) -> Option<&CandidateRevisionRequest> {
        match self {
            Self::ReviseCandidates(request) => Some(request),
            Self::Closure(_) => None,
        }
    }
}

/// One stage's decision before the Turn-level candidate/finalize framing is
/// appended. Only the binding window can ask for a candidate revision.
enum StageDecision {
    Close(TurnOutcome, Vec<ReconciliationAction>),
    ReviseCandidates(Vec<InvocationId>),
}

/// The pure decision function. `Err` is typed corruption: facts that
/// cannot describe the durable stage (partial or different reservations,
/// impossible orderings, digest mismatches) fail closed instead of being
/// guessed into an outcome.
pub fn reconcile(
    snapshot: &PendingSnapshot,
    facts: &InvocationFactsSnapshot,
) -> Result<ReconciliationDecision> {
    snapshot.validate()?;
    let (outcome, mut actions) = match &snapshot.stage {
        PendingStage::TurnStarted { .. } => {
            let reason = TurnInterruption::ProcessInterruptedBeforeModelAttempt;
            (
                interrupted(reason.clone()),
                vec![step_closed_interrupt(reason)],
            )
        }
        PendingStage::ModelAttemptStarted { .. } => {
            let reason = TurnInterruption::ProcessInterruptedDuringModelAttempt;
            (
                interrupted(reason.clone()),
                vec![step_closed_interrupt(reason)],
            )
        }
        PendingStage::ModelAttemptFailed { key, retryable, .. } => {
            let root = require_root(snapshot)?;
            let exhausted = key.attempt_ordinal + 1 >= root.config.limits.max_model_attempts;
            if *retryable && !exhausted {
                let reason = TurnInterruption::ProcessInterruptedDuringModelRetry;
                (
                    interrupted(reason.clone()),
                    vec![step_closed_interrupt(reason)],
                )
            } else {
                let reason = TurnFailure::ModelRequestFailed;
                (
                    TurnOutcome::Failed {
                        reason: reason.clone(),
                    },
                    vec![ReconciliationAction::PublishStepClosed {
                        kind: StepClosedKind::FailTurn { reason },
                    }],
                )
            }
        }
        PendingStage::ModelAttemptInterrupted { reason, .. } => {
            let turn_reason = TurnInterruption::ModelAttempt(*reason);
            (
                interrupted(turn_reason.clone()),
                vec![step_closed_interrupt(turn_reason)],
            )
        }
        PendingStage::ModelAttemptRejected { rejection, .. } => {
            let validation = match rejection {
                AttemptRejection::InvalidToolCall { validation } => validation.clone(),
            };
            let reason = TurnFailure::InvalidToolCall { validation };
            (
                TurnOutcome::Failed {
                    reason: reason.clone(),
                },
                vec![ReconciliationAction::PublishStepClosed {
                    kind: StepClosedKind::FailTurn { reason },
                }],
            )
        }
        PendingStage::MessageComplete { message, .. } => (
            TurnOutcome::Completed {
                finish_reason: TurnFinishReason::Stop,
            },
            vec![ReconciliationAction::PublishStepClosed {
                kind: StepClosedKind::FinishTurn {
                    final_message: message.clone(),
                },
            }],
        ),
        PendingStage::BindingPlanned { plan, .. } => {
            match binding_window_closure(snapshot, plan, facts, true)? {
                StageDecision::Close(outcome, actions) => (outcome, actions),
                StageDecision::ReviseCandidates(colliding) => {
                    // Not a closure: the Turn stays open until the revised
                    // candidates are durable.
                    return Ok(ReconciliationDecision::ReviseCandidates(
                        CandidateRevisionRequest {
                            session_id: snapshot.session_id,
                            turn_id: snapshot.turn_id,
                            plan: plan.clone(),
                            colliding_candidates: colliding,
                        },
                    ));
                }
            }
        }
        PendingStage::Bound { plan } => {
            match binding_window_closure(snapshot, plan, facts, false)? {
                StageDecision::Close(outcome, actions) => (outcome, actions),
                // `Bound` is published only after `reserve_batch` confirms
                // the entire exact plan durable (ADR 0005 §9). A foreign
                // owner here means the reservation was lost, not that fresh
                // candidates may be minted.
                StageDecision::ReviseCandidates(_) => {
                    return Err(SessionError::JournalCorruption {
                        detail: "Bound without the exact durable reservation it requires"
                            .to_string(),
                    });
                }
            }
        }
        PendingStage::StepClosing(state) => step_closing_closure(state, facts)?,
        PendingStage::StepClosed { kind } => step_closed_closure(kind)?,
        PendingStage::TurnClosing(closing) => (
            closing.candidate.outcome.clone(),
            vec![ReconciliationAction::FinalizeTurn],
        ),
        PendingStage::Committed(committed) => (
            committed.candidate.outcome.clone(),
            vec![ReconciliationAction::FinalizeTurn],
        ),
    };

    // Tips that already pinned a candidate only retry the exact final
    // transaction; every earlier tip appends the closure candidate and the
    // finalization.
    if !matches!(
        snapshot.stage,
        PendingStage::TurnClosing(_) | PendingStage::Committed(_)
    ) {
        let root = require_root(snapshot)?;
        // The complete closure set this plan produces (the last cursor state
        // of the closing loop), for the candidate's transcript framing.
        let closures: Vec<ClosedBinding> = actions
            .iter()
            .filter_map(|action| match action {
                ReconciliationAction::AdvanceStepClosing { state } => Some(state.closures.clone()),
                _ => None,
            })
            .next_back()
            .unwrap_or_default();
        let messages = candidate_messages(snapshot, root, &closures);
        actions.push(ReconciliationAction::PublishTurnClosing {
            candidate: build_candidate(snapshot, root, outcome.clone(), messages)?,
        });
        actions.push(ReconciliationAction::FinalizeTurn);
    }

    Ok(ReconciliationDecision::Closure(ReconciliationPlan {
        session_id: snapshot.session_id,
        turn_id: snapshot.turn_id,
        outcome,
        origin: ClosureOrigin::Reconciled,
        actions,
    }))
}

// ─── Stage closures ─────────────────────────────────────────────────────

/// `BindingPlanned` (plan absent or reserved) and `Bound` (reserved)
/// closures (ADR 0005 §14). A partial or different reservation is
/// corruption. A `DispatchStarted` fact without a terminal is
/// conservative `Indeterminate(MayHaveOccurred)`; remaining bindings close
/// `NotExecuted(PrecedingInvocationIndeterminate)`; otherwise every
/// binding closes `NotExecuted(BeforeDispatch)`.
fn binding_window_closure(
    _snapshot: &PendingSnapshot,
    plan: &BindingPlan,
    facts: &InvocationFactsSnapshot,
    publish_bound: bool,
) -> Result<StageDecision> {
    let mut actions = Vec::new();
    match facts.validate_against_plan(&plan.plan_key, &plan.bindings)? {
        PlanValidation::Collision => {
            // Truth table #2: candidate IDs owned by another plan. No
            // outcome exists yet — the caller must revise candidates first.
            let colliding = plan
                .bindings
                .iter()
                .filter(|binding| {
                    facts
                        .record_of(&binding.invocation_id)
                        .is_some_and(|record| record.owner != plan.plan_key)
                })
                .map(|binding| binding.invocation_id)
                .collect();
            return Ok(StageDecision::ReviseCandidates(colliding));
        }
        PlanValidation::AbsentPlan => {
            actions.push(ReconciliationAction::ReserveExactPlan {
                plan_key: plan.plan_key,
                bindings: plan.bindings.clone(),
            });
        }
        PlanValidation::ExactMatch => {} // idempotent: already reserved
    }
    if publish_bound {
        actions.push(ReconciliationAction::PublishBound { plan: plan.clone() });
    }

    // A terminal fact in the binding window is an ordering violation: the
    // terminal follows the draft, the draft follows the candidate pin, and
    // the pin lives in a StepClosing snapshot.
    if plan.bindings.iter().any(|b| {
        matches!(
            facts.lifecycle_of(&b.invocation_id),
            Some(InvocationLifecycleFact::Terminal { .. })
        )
    }) {
        return Err(SessionError::JournalCorruption {
            detail: "terminal invocation fact before any StepClosing snapshot".to_string(),
        });
    }

    let dispatch_at = plan.bindings.iter().position(|b| {
        matches!(
            facts.lifecycle_of(&b.invocation_id),
            Some(InvocationLifecycleFact::DispatchStarted)
        )
    });

    match dispatch_at {
        Some(index) => {
            // The batch is ordered: later bindings cannot have been reached.
            for binding in plan.bindings.iter().skip(index + 1) {
                if facts.lifecycle_of(&binding.invocation_id)
                    != Some(&InvocationLifecycleFact::Reserved)
                {
                    return Err(SessionError::JournalCorruption {
                        detail: format!(
                            "binding {} dispatched past the indeterminate ordinal",
                            binding.invocation_id
                        ),
                    });
                }
            }
            let reason = TurnInterruption::InvocationMayHaveOccurred {
                invocation: plan.bindings[index].invocation_id,
            };
            let classifications = classifications_for(&plan.bindings, 0, Some(index), None);
            actions.extend(closing_loop_actions(
                plan.plan_key,
                &plan.bindings,
                &[],
                &classifications,
            ));
            actions.push(step_closed_interrupt(reason.clone()));
            Ok(StageDecision::Close(interrupted(reason), actions))
        }
        None => {
            let reason = TurnInterruption::ProcessInterruptedDuringToolBinding;
            let classifications = classifications_for(&plan.bindings, 0, None, None);
            actions.extend(closing_loop_actions(
                plan.plan_key,
                &plan.bindings,
                &[],
                &classifications,
            ));
            actions.push(step_closed_interrupt(reason.clone()));
            Ok(StageDecision::Close(interrupted(reason), actions))
        }
    }
}

/// `StepClosing` closure: ordinals below the cursor are immutable; the
/// lowest unclosed ordinal closes from its Invocation fact and pinned
/// draft candidate (available material must match the candidate); later
/// ordinals cannot be skipped (ADR 0005 §14).
fn step_closing_closure(
    state: &StepClosingState,
    facts: &InvocationFactsSnapshot,
) -> Result<(TurnOutcome, Vec<ReconciliationAction>)> {
    match facts.validate_against_plan(&state.plan_key, &state.bindings)? {
        PlanValidation::ExactMatch => {}
        _ => {
            return Err(SessionError::JournalCorruption {
                detail: "step closing requires an exact reservation".to_string(),
            });
        }
    }
    let cursor = state.closure_cursor as usize;

    // Bindings beyond the cursor must still be reserved: the batch is
    // strictly ordered and reopen never dispatches.
    for binding in state.bindings.iter().skip(cursor + 1) {
        if facts.lifecycle_of(&binding.invocation_id) != Some(&InvocationLifecycleFact::Reserved) {
            return Err(SessionError::JournalCorruption {
                detail: format!(
                    "binding {} moved past the closure cursor without closing first",
                    binding.invocation_id
                ),
            });
        }
    }

    let current = state.bindings.get(cursor);
    let current_fact: Option<&InvocationLifecycleFact> =
        current.and_then(|binding| facts.lifecycle_of(&binding.invocation_id));
    let candidate = state.current_candidate.as_ref();

    // (indeterminate ordinal, resume classification for the cursor binding)
    let (indeterminate_at, resume) = match (current, current_fact, candidate) {
        (None, _, _) => (None, None),
        (Some(_), Some(InvocationLifecycleFact::Terminal { outcome, material }), Some(draft)) => {
            // Available material must be the pinned candidate's exact
            // content; a mismatch is corruption (ADR 0005 §12).
            if let ResultMaterial::Available {
                digest,
                byte_length,
            } = material
                && (*digest != draft.content_digest || *byte_length != draft.byte_length)
            {
                return Err(SessionError::JournalCorruption {
                    detail: "terminal material does not match the pinned draft candidate"
                        .to_string(),
                });
            }
            (None, Some((outcome.clone(), material.clone())))
        }
        (Some(_), Some(InvocationLifecycleFact::Terminal { .. }), None) => {
            return Err(SessionError::JournalCorruption {
                detail: "terminal invocation fact without a pinned draft candidate".to_string(),
            });
        }
        (Some(_), Some(InvocationLifecycleFact::DispatchStarted), _) => (Some(cursor), None),
        (Some(_), Some(InvocationLifecycleFact::Reserved), Some(_)) => {
            return Err(SessionError::JournalCorruption {
                detail: "pinned draft candidate for a binding that never dispatched".to_string(),
            });
        }
        (Some(_), Some(InvocationLifecycleFact::Reserved), None) => (None, None),
        (Some(_), None, _) => {
            return Err(SessionError::JournalCorruption {
                detail: "closure-cursor binding has no reservation record".to_string(),
            });
        }
    };

    let mut actions = Vec::new();
    let classifications = classifications_for(&state.bindings, cursor, indeterminate_at, resume);
    actions.extend(closing_loop_actions(
        state.plan_key,
        &state.bindings,
        &state.closures,
        &classifications,
    ));

    let reason = match indeterminate_at {
        Some(_) => TurnInterruption::InvocationMayHaveOccurred {
            invocation: state.bindings[cursor].invocation_id,
        },
        None => TurnInterruption::ProcessInterruptedDuringStepClosing,
    };
    actions.push(step_closed_interrupt(reason.clone()));
    Ok((interrupted(reason), actions))
}

/// `StepClosed` closure: terminal kinds map to their recorded outcome;
/// `ContinueModel` closes interrupted because only a live owner advances
/// to another provider attempt (ADR 0005 §14).
fn step_closed_closure(kind: &StepClosedKind) -> Result<(TurnOutcome, Vec<ReconciliationAction>)> {
    let outcome = match kind {
        StepClosedKind::FinishTurn { .. } => TurnOutcome::Completed {
            finish_reason: TurnFinishReason::Stop,
        },
        StepClosedKind::ContinueModel { .. } => {
            interrupted(TurnInterruption::WhileAwaitingModelContinuation)
        }
        StepClosedKind::FailTurn { reason } => TurnOutcome::Failed {
            reason: reason.clone(),
        },
        StepClosedKind::CancelTurn { reason } => TurnOutcome::Cancelled { reason: *reason },
        StepClosedKind::InterruptTurn { reason } => interrupted(reason.clone()),
    };
    Ok((outcome, Vec::new()))
}

// ─── Classification and action assembly ─────────────────────────────────

/// Per-binding terminalization classification for every binding from
/// `from` on. When `indeterminate_at` is given, that ordinal is
/// `Indeterminate(MayHaveOccurred)` with no material and every later one
/// is `NotExecuted(PrecedingInvocationIndeterminate)`; otherwise all are
/// `NotExecuted(BeforeDispatch)`. `resume` overrides the classification at
/// `from` (the cursor binding's durable terminal fact). Entries before
/// `from` are placeholders the ordered loop skips.
fn classifications_for(
    bindings: &[PlannedBinding],
    from: usize,
    indeterminate_at: Option<usize>,
    resume: Option<(ExecutionOutcome, ResultMaterial)>,
) -> Vec<(ExecutionOutcome, ResultMaterial)> {
    bindings
        .iter()
        .enumerate()
        .map(|(index, _)| {
            if index < from {
                return (
                    ExecutionOutcome::NotExecuted {
                        reason: NotExecutedReason::BeforeDispatch,
                    },
                    not_executed_material(),
                );
            }
            if index == from
                && let Some(resume) = resume.clone()
            {
                return resume;
            }
            match indeterminate_at {
                Some(at) if index == at => (
                    ExecutionOutcome::Indeterminate {
                        reason: IndeterminateReason::MayHaveOccurred,
                    },
                    ResultMaterial::Unavailable {
                        reason: UnavailableMaterialReason::DraftPublicationFailed,
                        digest: None,
                        byte_length: None,
                    },
                ),
                Some(_) => (
                    ExecutionOutcome::NotExecuted {
                        reason: NotExecutedReason::PrecedingInvocationIndeterminate,
                    },
                    not_executed_material(),
                ),
                None => (
                    ExecutionOutcome::NotExecuted {
                        reason: NotExecutedReason::BeforeDispatch,
                    },
                    not_executed_material(),
                ),
            }
        })
        .collect()
}

fn not_executed_material() -> ResultMaterial {
    ResultMaterial::Unavailable {
        reason: UnavailableMaterialReason::NotExecuted,
        digest: None,
        byte_length: None,
    }
}

/// Emit the ordered closing loop. When `closed` is empty the loop starts
/// with the cursor-0 baseline (the `Bound` -> `StepClosing` transition);
/// otherwise it resumes the existing tip at its cursor. Each binding is
/// terminalized, then closed by exactly one cursor advance.
fn closing_loop_actions(
    plan_key: PlanKey,
    bindings: &[PlannedBinding],
    closed: &[ClosedBinding],
    classifications: &[(ExecutionOutcome, ResultMaterial)],
) -> Vec<ReconciliationAction> {
    let mut actions = Vec::new();
    let mut closures = closed.to_vec();
    if closures.is_empty() {
        actions.push(ReconciliationAction::AdvanceStepClosing {
            state: StepClosingState {
                plan_key,
                bindings: bindings.to_vec(),
                closure_cursor: 0,
                closures: Vec::new(),
                current_candidate: None,
            },
        });
    }
    for (_index, (binding, (outcome, material))) in bindings
        .iter()
        .zip(classifications.iter())
        .enumerate()
        .skip(closures.len())
    {
        actions.push(ReconciliationAction::TerminalizeInvocation {
            invocation: binding.invocation_id,
            outcome: outcome.clone(),
            material: material.clone(),
        });
        closures.push(ClosedBinding {
            binding: binding.clone(),
            outcome: to_tool_result_outcome(outcome),
            material: to_material_availability(material),
        });
        actions.push(ReconciliationAction::AdvanceStepClosing {
            state: StepClosingState {
                plan_key,
                bindings: bindings.to_vec(),
                closure_cursor: closures.len() as u32,
                closures: closures.clone(),
                current_candidate: None,
            },
        });
    }
    actions
}

fn to_tool_result_outcome(outcome: &ExecutionOutcome) -> ToolResultOutcome {
    match outcome {
        ExecutionOutcome::Succeeded => ToolResultOutcome::Succeeded,
        ExecutionOutcome::Failed => ToolResultOutcome::Failed,
        ExecutionOutcome::Rejected => ToolResultOutcome::Rejected,
        ExecutionOutcome::Cancelled => ToolResultOutcome::Cancelled,
        ExecutionOutcome::NotExecuted { .. } => ToolResultOutcome::NotExecuted,
        ExecutionOutcome::Indeterminate { .. } => ToolResultOutcome::Indeterminate,
    }
}

fn to_material_availability(material: &ResultMaterial) -> MaterialAvailability {
    match material {
        ResultMaterial::Available {
            digest,
            byte_length,
        } => MaterialAvailability::Available {
            digest: *digest,
            byte_length: *byte_length,
        },
        ResultMaterial::Unavailable {
            reason,
            digest,
            byte_length,
        } => MaterialAvailability::Unavailable {
            reason: *reason,
            digest: *digest,
            byte_length: *byte_length,
        },
    }
}

// ─── Candidate construction (durable inputs only) ───────────────────────

fn interrupted(reason: TurnInterruption) -> TurnOutcome {
    TurnOutcome::Interrupted { reason }
}

fn step_closed_interrupt(reason: TurnInterruption) -> ReconciliationAction {
    ReconciliationAction::PublishStepClosed {
        kind: StepClosedKind::InterruptTurn { reason },
    }
}

fn require_root(snapshot: &PendingSnapshot) -> Result<&TurnStartedFacts> {
    // A `TurnStarted` tip is its own root; every later tip carries the
    // store-filled chain-root facts.
    match &snapshot.stage {
        PendingStage::TurnStarted(facts) => Ok(facts),
        _ => snapshot
            .root
            .as_ref()
            .ok_or_else(|| SessionError::JournalCorruption {
                detail: "snapshot carries no chain-root TurnStarted facts".to_string(),
            }),
    }
}

/// The tip-visible usage, conservatively `Partial`: reopen can never prove
/// every attempt reported usage (ADR 0005 §10).
fn tip_usage(snapshot: &PendingSnapshot) -> Option<TokenUsage> {
    match &snapshot.stage {
        PendingStage::ModelAttemptFailed { usage, .. }
        | PendingStage::ModelAttemptInterrupted { usage, .. }
        | PendingStage::ModelAttemptRejected { usage, .. }
        | PendingStage::MessageComplete { usage, .. }
        | PendingStage::BindingPlanned { usage, .. } => *usage,
        _ => None,
    }
}

/// Transcript messages of the closure candidate, assembled from durable
/// data: the frozen user message, the final message when one completed,
/// and explicit closure framing for closed bindings (never model-authored
/// speech, ADR 0005 §14).
fn candidate_messages(
    snapshot: &PendingSnapshot,
    root: &TurnStartedFacts,
    closures: &[ClosedBinding],
) -> Vec<GraphMessage> {
    let mut messages = vec![user_transcript(root)];
    match &snapshot.stage {
        PendingStage::MessageComplete { message, .. } => messages.push(native_transcript(message)),
        PendingStage::StepClosed {
            kind: StepClosedKind::FinishTurn { final_message },
        } => messages.push(native_transcript(final_message)),
        // ContinueModel closures are already durable in the snapshot; the
        // resumed closures from the plan append after them.
        PendingStage::StepClosed {
            kind:
                StepClosedKind::ContinueModel {
                    ordered_tool_results,
                    ..
                },
        } => {
            for closure in ordered_tool_results {
                messages.push(closure_transcript(closure));
            }
        }
        _ => {}
    }
    for closure in closures {
        messages.push(closure_transcript(closure));
    }
    messages
}

fn build_candidate(
    snapshot: &PendingSnapshot,
    root: &TurnStartedFacts,
    outcome: TurnOutcome,
    messages: Vec<GraphMessage>,
) -> Result<TurnCommitCandidate> {
    // The durable journal tip's own committer time: recovery introduces no
    // new clock, and it never substitutes another durable field for the one
    // it is missing. `recorded_at` is filled by the store on read, so a
    // snapshot without one did not come from the journal — fail closed
    // rather than pin a timestamp this tip cannot prove (ADR 0005 §6/§9).
    let recorded_at = snapshot
        .recorded_at
        .ok_or_else(|| SessionError::JournalCorruption {
            detail: "snapshot carries no journal committer time; a closure candidate cannot \
                     pin a durable created_at"
                .to_string(),
        })?;
    let created_at = chrono::DateTime::from_timestamp(recorded_at, 0)
        .ok_or_else(|| SessionError::JournalCorruption {
            detail: format!("journal committer time {recorded_at} is not a representable instant"),
        })?
        .to_rfc3339();
    let commit_message = crate::graph::derive_commit_message(&messages);
    Ok(TurnCommitCandidate {
        session_id: snapshot.session_id.to_string(),
        turn_id: snapshot.turn_id,
        side_parent_ids: Vec::new(),
        messages,
        agent_name: root.config.agent_name.clone(),
        profile_snapshot_hash: encode_hex(&root.config.digest().expect("validated config")),
        profile_snapshot_json: root.config.profile_snapshot_json.clone(),
        model: root.config.model.clone(),
        provider: root.config.provider.clone(),
        outcome,
        origin: ClosureOrigin::Reconciled,
        commit_message,
        usage_aggregate: tip_usage(snapshot).map(|usage| TurnUsageAggregate::Partial { usage }),
        workspace_history_oid: None,
        created_at,
    })
}

/// The frozen user message in transcript shape.
fn user_transcript(root: &TurnStartedFacts) -> GraphMessage {
    let mut content = String::new();
    for part in &root.user_message.parts {
        if let MessageSemantic::Text { text } = &part.semantic {
            if !content.is_empty() {
                content.push('\n');
            }
            content.push_str(text);
        }
    }
    GraphMessage {
        role: "user".to_string(),
        content,
        reasoning_content: None,
        tool_calls: None,
        tool_call_id: None,
        name: None,
    }
}

/// One provider-produced message in transcript shape: text parts join in
/// order (zero-Tool messages only).
fn native_transcript(message: &NativeMessage<UnboundToolCall>) -> GraphMessage {
    let mut content = String::new();
    for part in &message.parts {
        if let MessageSemantic::Text { text } = &part.semantic {
            if !content.is_empty() {
                content.push('\n');
            }
            content.push_str(text);
        }
    }
    GraphMessage {
        role: "assistant".to_string(),
        content,
        reasoning_content: None,
        tool_calls: None,
        tool_call_id: None,
        name: None,
    }
}

/// Explicit closure framing for one closed binding: canonical structured
/// content naming the invocation, its closure outcome, and material
/// availability. This is operator/operator-visible framing, never
/// model-authored speech.
fn closure_transcript(closure: &ClosedBinding) -> GraphMessage {
    let material = match &closure.material {
        MaterialAvailability::Available {
            digest,
            byte_length,
        } => serde_json::json!({
            "available": true,
            "digest": encode_hex(digest),
            "byte_length": byte_length,
        }),
        MaterialAvailability::Unavailable {
            reason,
            digest,
            byte_length,
        } => serde_json::json!({
            "available": false,
            "reason": match reason {
                UnavailableMaterialReason::DraftPublicationFailed => "draft_publication_failed",
                UnavailableMaterialReason::OutputTooLarge => "output_too_large",
                UnavailableMaterialReason::NotExecuted => "not_executed",
            },
            "digest": digest.map(|d| encode_hex(&d)),
            "byte_length": byte_length,
        }),
    };
    GraphMessage {
        role: "tool".to_string(),
        content: lancet_protocol::canonical_json(&serde_json::json!({
            "invocation_id": closure.binding.invocation_id.to_string(),
            "outcome": closure.outcome.as_str(),
            "material": material,
        })),
        reasoning_content: None,
        tool_calls: None,
        tool_call_id: None,
        name: Some(closure.binding.method.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lancet_protocol::{ModelAttemptKey, SessionId, TurnId};
    use lattice_conversation::{
        AttemptInterruption, AttemptRejection, MessagePart, ToolCallValidation,
    };

    const SESSION: &str = "3f8b2c1a7d4e4f5a8b9c0d1e2f3a4b5c";
    const TURN: &str = "3f8b2c1a-7d4e-4f5a-8b9c-0d1e2f3a4b5c";
    const STEP_TIME: i64 = 1_800_000_000;

    fn turn_id() -> TurnId {
        TurnId::parse(TURN).unwrap()
    }

    fn invocation(n: u32) -> InvocationId {
        InvocationId::parse(&format!("00000000-0000-4000-8000-{n:012x}")).unwrap()
    }

    fn key(step: u32, attempt: u32) -> ModelAttemptKey {
        ModelAttemptKey {
            turn_id: turn_id(),
            model_step_ordinal: step,
            attempt_ordinal: attempt,
        }
    }

    fn root() -> TurnStartedFacts {
        super::super::tests_support::turn_started_facts()
    }

    fn snapshot(stage: PendingStage) -> PendingSnapshot {
        let mut facts = root();
        facts.turn_id = turn_id();
        let mut snap = PendingSnapshot {
            schema_version: super::super::contract::PENDING_SNAPSHOT_SCHEMA_VERSION,
            session_id: SessionId::parse(SESSION).unwrap(),
            turn_id: turn_id(),
            predecessor: Some("0".repeat(40)),
            stage,
            recorded_at: Some(STEP_TIME),
            root: None,
        };
        if !matches!(&snap.stage, PendingStage::TurnStarted { .. }) {
            snap.root = Some(facts);
        }
        snap
    }

    fn text_message(text: &str) -> NativeMessage<UnboundToolCall> {
        NativeMessage::new(
            lattice_conversation::MessageRole::Assistant,
            None,
            vec![MessagePart::semantic_only(MessageSemantic::Text {
                text: text.to_string(),
            })],
        )
    }

    /// A plan with `count` bindings at part ordinals `0..count`.
    fn plan(count: usize) -> BindingPlan {
        let calls: Vec<MessagePart<UnboundToolCall>> = (0..count)
            .map(|i| {
                MessagePart::semantic_only(MessageSemantic::ToolCall {
                    call: UnboundToolCall {
                        provider_call_id: Some(format!("call-{i}")),
                        name: "observe.workspace".to_string(),
                        arguments: serde_json::json!({ "n": i }),
                    },
                })
            })
            .collect();
        let message = NativeMessage::new(lattice_conversation::MessageRole::Assistant, None, calls);
        let bindings = (0..count)
            .map(|i| {
                let address = lancet_protocol::ToolCallAddress {
                    message: lancet_protocol::ModelMessageAddress {
                        session_id: SessionId::parse(SESSION).unwrap(),
                        turn_id: turn_id(),
                        model_step_ordinal: 0,
                    },
                    part_ordinal: i as u32,
                };
                let args_digest =
                    lattice_conversation::arguments_digest(&serde_json::json!({ "n": i }));
                PlannedBinding {
                    invocation_id: invocation(i as u32 + 1),
                    binding_digest: super::super::binding::binding_digest(
                        &invocation(i as u32 + 1),
                        &address,
                        "observe.workspace",
                        "d",
                        &args_digest,
                    ),
                    address,
                    method: "observe.workspace".to_string(),
                    descriptor_identity: "d".to_string(),
                    arguments_digest: args_digest,
                }
            })
            .collect();
        BindingPlan {
            plan_key: PlanKey {
                turn_id: turn_id(),
                model_step_ordinal: 0,
            },
            message,
            bindings,
        }
    }

    fn facts_with_plan(
        plan: &BindingPlan,
        states: Vec<InvocationLifecycleFact>,
    ) -> InvocationFactsSnapshot {
        let mut facts = InvocationFactsSnapshot::empty();
        facts.plan = Some(super::super::facts::PlanFacts {
            plan_key: plan.plan_key,
            bindings: plan.bindings.clone(),
        });
        for (binding, lifecycle) in plan.bindings.iter().zip(states) {
            facts.invocations.insert(
                binding.invocation_id,
                super::super::facts::InvocationRecordFact {
                    owner: plan.plan_key,
                    lifecycle,
                },
            );
        }
        facts
    }

    fn empty_facts() -> InvocationFactsSnapshot {
        InvocationFactsSnapshot::empty()
    }

    /// Reconcile and require a closure decision.
    fn closure(
        snapshot: &PendingSnapshot,
        facts: &InvocationFactsSnapshot,
        what: &str,
    ) -> ReconciliationPlan {
        reconcile(snapshot, facts)
            .unwrap_or_else(|e| panic!("{what}: {e:?}"))
            .closure()
            .unwrap_or_else(|| panic!("{what}: expected a closure, got a candidate revision"))
            .clone()
    }

    fn interrupted_reason(plan: &ReconciliationPlan) -> &TurnInterruption {
        match &plan.outcome {
            TurnOutcome::Interrupted { reason } => reason,
            other => panic!("expected interrupted outcome, got {other:?}"),
        }
    }

    fn assert_stage_only_closure(plan: &ReconciliationPlan, expected_kind: &str) {
        // Exactly one StepClosed, exactly one TurnClosing, exactly one
        // FinalizeTurn, in order, and never a dispatch/continuation action.
        let kinds: Vec<&str> = plan
            .actions
            .iter()
            .map(|action| match action {
                ReconciliationAction::ReserveExactPlan { .. } => "reserve",
                ReconciliationAction::TerminalizeInvocation { .. } => "terminalize",
                ReconciliationAction::PublishBound { .. } => "bound",
                ReconciliationAction::AdvanceStepClosing { .. } => "step_closing",
                ReconciliationAction::PublishStepClosed { kind } => match kind {
                    StepClosedKind::FinishTurn { .. } => "closed_finish",
                    StepClosedKind::ContinueModel { .. } => "closed_continue",
                    StepClosedKind::FailTurn { .. } => "closed_fail",
                    StepClosedKind::CancelTurn { .. } => "closed_cancel",
                    StepClosedKind::InterruptTurn { .. } => "closed_interrupt",
                },
                ReconciliationAction::PublishTurnClosing { .. } => "turn_closing",
                ReconciliationAction::FinalizeTurn => "finalize",
            })
            .collect();
        assert_eq!(
            kinds.iter().filter(|k| k.starts_with("closed_")).count(),
            1,
            "exactly one StepClosed: {kinds:?}"
        );
        assert_eq!(
            kinds.iter().filter(|k| **k == "turn_closing").count(),
            1,
            "exactly one TurnClosing: {kinds:?}"
        );
        assert_eq!(
            kinds.iter().filter(|k| **k == "finalize").count(),
            1,
            "exactly one FinalizeTurn: {kinds:?}"
        );
        assert!(
            kinds.contains(&expected_kind),
            "expected {expected_kind} in {kinds:?}"
        );
        // Reopen never continues model/tool execution.
        assert!(
            !kinds
                .iter()
                .any(|k| **k == *"dispatch" || **k == *"continue")
        );
    }

    #[test]
    fn turn_started_without_attempt_closes_interrupted_before_model() {
        let snap = PendingSnapshot {
            predecessor: None,
            stage: PendingStage::TurnStarted(root()),
            ..snapshot(PendingStage::TurnStarted(root()))
        };
        let plan = closure(&snap, &empty_facts(), "plan");
        assert!(matches!(
            interrupted_reason(&plan),
            TurnInterruption::ProcessInterruptedBeforeModelAttempt
        ));
        assert_stage_only_closure(&plan, "closed_interrupt");
        assert_eq!(plan.origin, ClosureOrigin::Reconciled);
    }

    #[test]
    fn incomplete_model_attempt_started_closes_interrupted() {
        let snap = snapshot(PendingStage::ModelAttemptStarted { key: key(0, 0) });
        let plan = closure(&snap, &empty_facts(), "plan");
        assert!(matches!(
            interrupted_reason(&plan),
            TurnInterruption::ProcessInterruptedDuringModelAttempt
        ));
        assert_stage_only_closure(&plan, "closed_interrupt");
    }

    #[test]
    fn retryable_failure_between_retries_interrupts_and_exhaustion_fails() {
        // Budget of 2 (frozen root): attempt 0 failed retryable -> retry open.
        let snap = snapshot(PendingStage::ModelAttemptFailed {
            key: key(0, 0),
            retryable: true,
            usage: None,
        });
        let plan = closure(&snap, &empty_facts(), "plan");
        assert!(matches!(
            interrupted_reason(&plan),
            TurnInterruption::ProcessInterruptedDuringModelRetry
        ));

        // attempt 1 failed retryable: budget exhausted -> Failed.
        let snap = snapshot(PendingStage::ModelAttemptFailed {
            key: key(0, 1),
            retryable: true,
            usage: None,
        });
        let plan = closure(&snap, &empty_facts(), "plan");
        assert!(matches!(
            plan.outcome,
            TurnOutcome::Failed {
                reason: TurnFailure::ModelRequestFailed,
            }
        ));
        assert_stage_only_closure(&plan, "closed_fail");

        // Non-retryable fails immediately.
        let snap = snapshot(PendingStage::ModelAttemptFailed {
            key: key(0, 0),
            retryable: false,
            usage: None,
        });
        let plan = closure(&snap, &empty_facts(), "plan");
        assert!(matches!(
            plan.outcome,
            TurnOutcome::Failed {
                reason: TurnFailure::ModelRequestFailed,
            }
        ));
    }

    #[test]
    fn interrupted_attempt_passes_its_durable_reason_through() {
        let snap = snapshot(PendingStage::ModelAttemptInterrupted {
            key: key(0, 0),
            reason: AttemptInterruption::StreamError,
            usage: None,
        });
        let plan = closure(&snap, &empty_facts(), "plan");
        assert!(matches!(
            interrupted_reason(&plan),
            TurnInterruption::ModelAttempt(AttemptInterruption::StreamError)
        ));
    }

    #[test]
    fn rejected_attempt_fails_with_the_durable_rejection() {
        let snap = snapshot(PendingStage::ModelAttemptRejected {
            key: key(0, 0),
            rejection: AttemptRejection::InvalidToolCall {
                validation: ToolCallValidation::MalformedArguments { call_ordinal: 0 },
            },
            usage: None,
        });
        let plan = closure(&snap, &empty_facts(), "plan");
        assert!(matches!(
            plan.outcome,
            TurnOutcome::Failed {
                reason: TurnFailure::InvalidToolCall { .. }
            }
        ));
    }

    #[test]
    fn zero_tool_message_complete_finishes_the_turn() {
        let snap = snapshot(PendingStage::MessageComplete {
            key: key(0, 0),
            message: text_message("done"),
            usage: None,
        });
        let plan = closure(&snap, &empty_facts(), "plan");
        assert!(matches!(
            plan.outcome,
            TurnOutcome::Completed {
                finish_reason: TurnFinishReason::Stop
            }
        ));
        assert_stage_only_closure(&plan, "closed_finish");
        // The candidate carries the frozen user message plus the final
        // message, and pins the durable tip time as created_at.
        let candidate = plan
            .actions
            .iter()
            .find_map(|a| match a {
                ReconciliationAction::PublishTurnClosing { candidate } => Some(candidate),
                _ => None,
            })
            .expect("candidate");
        assert_eq!(candidate.messages.len(), 2);
        assert_eq!(candidate.messages[0].role, "user");
        assert_eq!(candidate.messages[1].content, "done");
        assert_eq!(
            candidate.created_at,
            chrono::DateTime::from_timestamp(STEP_TIME, 0)
                .unwrap()
                .to_rfc3339()
        );
    }

    #[test]
    fn reconciled_usage_aggregate_is_conservatively_partial() {
        let snap = snapshot(PendingStage::MessageComplete {
            key: key(0, 0),
            message: text_message("done"),
            usage: Some(TokenUsage {
                prompt_tokens: 7,
                completion_tokens: 3,
                total_tokens: 10,
            }),
        });
        let plan = closure(&snap, &empty_facts(), "plan");
        let candidate = plan
            .actions
            .iter()
            .find_map(|a| match a {
                ReconciliationAction::PublishTurnClosing { candidate } => Some(candidate),
                _ => None,
            })
            .expect("candidate");
        assert_eq!(
            candidate.usage_aggregate,
            Some(TurnUsageAggregate::Partial {
                usage: TokenUsage {
                    prompt_tokens: 7,
                    completion_tokens: 3,
                    total_tokens: 10,
                }
            })
        );
    }

    #[test]
    fn binding_planned_with_absent_plan_reserves_and_closes_not_executed() {
        let plan_two = plan(2);
        let snap = snapshot(PendingStage::BindingPlanned {
            plan: plan_two.clone(),
            usage: None,
        });
        let result = closure(&snap, &empty_facts(), "plan");
        assert!(matches!(
            interrupted_reason(&result),
            TurnInterruption::ProcessInterruptedDuringToolBinding
        ));
        // Closure-only reservation of the exact durable candidates.
        assert!(matches!(
            result.actions.first(),
            Some(ReconciliationAction::ReserveExactPlan { plan_key, bindings })
                if *plan_key == plan_two.plan_key && bindings == &plan_two.bindings
        ));
        let terminalizes: Vec<_> = result
            .actions
            .iter()
            .filter_map(|a| match a {
                ReconciliationAction::TerminalizeInvocation {
                    invocation,
                    outcome,
                    material,
                } => Some((invocation, outcome, material)),
                _ => None,
            })
            .collect();
        assert_eq!(terminalizes.len(), 2);
        for (invocation, outcome, material) in terminalizes {
            assert_eq!(
                outcome,
                &ExecutionOutcome::NotExecuted {
                    reason: NotExecutedReason::BeforeDispatch
                }
            );
            assert_eq!(
                material,
                &ResultMaterial::Unavailable {
                    reason: UnavailableMaterialReason::NotExecuted,
                    digest: None,
                    byte_length: None,
                }
            );
            assert!(
                plan_two
                    .bindings
                    .iter()
                    .any(|b| b.invocation_id == *invocation)
            );
        }
        // The closing loop is ordered and monotonic.
        let cursors: Vec<u32> = result
            .actions
            .iter()
            .filter_map(|a| match a {
                ReconciliationAction::AdvanceStepClosing { state } => Some(state.closure_cursor),
                _ => None,
            })
            .collect();
        assert_eq!(cursors, vec![0, 1, 2]);
        assert_stage_only_closure(&result, "closed_interrupt");
    }

    #[test]
    fn binding_planned_with_exact_reservation_publishes_bound_and_closes() {
        let plan_one = plan(1);
        let snap = snapshot(PendingStage::BindingPlanned {
            plan: plan_one.clone(),
            usage: None,
        });
        let facts = facts_with_plan(&plan_one, vec![InvocationLifecycleFact::Reserved]);
        let result = closure(&snap, &facts, "plan");
        assert!(
            result
                .actions
                .iter()
                .any(|a| matches!(a, ReconciliationAction::PublishBound { .. }))
        );
        assert!(
            !result
                .actions
                .iter()
                .any(|a| matches!(a, ReconciliationAction::ReserveExactPlan { .. }))
        );
    }

    #[test]
    fn partial_or_different_reservation_is_corruption() {
        let plan_two = plan(2);
        let snap = snapshot(PendingStage::BindingPlanned {
            plan: plan_two.clone(),
            usage: None,
        });
        // Partial: one binding recorded.
        let mut partial = InvocationFactsSnapshot::empty();
        partial.plan = Some(super::super::facts::PlanFacts {
            plan_key: plan_two.plan_key,
            bindings: plan_two.bindings.clone(),
        });
        partial.invocations.insert(
            plan_two.bindings[0].invocation_id,
            super::super::facts::InvocationRecordFact {
                owner: plan_two.plan_key,
                lifecycle: InvocationLifecycleFact::Reserved,
            },
        );
        assert!(matches!(
            reconcile(&snap, &partial),
            Err(SessionError::JournalCorruption { .. })
        ));

        // Different plan recorded.
        let mut different = plan(2);
        different.bindings[0].descriptor_identity = "other".to_string();
        let mut wrong = InvocationFactsSnapshot::empty();
        wrong.plan = Some(super::super::facts::PlanFacts {
            plan_key: different.plan_key,
            bindings: different.bindings.clone(),
        });
        for binding in &different.bindings {
            wrong.invocations.insert(
                binding.invocation_id,
                super::super::facts::InvocationRecordFact {
                    owner: different.plan_key,
                    lifecycle: InvocationLifecycleFact::Reserved,
                },
            );
        }
        assert!(matches!(
            reconcile(&snap, &wrong),
            Err(SessionError::JournalCorruption { .. })
        ));
    }

    #[test]
    fn bound_without_dispatch_closes_not_executed_before_dispatch() {
        let plan_two = plan(2);
        let snap = snapshot(PendingStage::Bound {
            plan: plan_two.clone(),
        });
        let facts = facts_with_plan(
            &plan_two,
            vec![
                InvocationLifecycleFact::Reserved,
                InvocationLifecycleFact::Reserved,
            ],
        );
        let result = closure(&snap, &facts, "plan");
        assert!(matches!(
            interrupted_reason(&result),
            TurnInterruption::ProcessInterruptedDuringToolBinding
        ));
        let before_dispatch = result
            .actions
            .iter()
            .filter(|a| {
                matches!(
                    a,
                    ReconciliationAction::TerminalizeInvocation {
                        outcome: ExecutionOutcome::NotExecuted {
                            reason: NotExecutedReason::BeforeDispatch
                        },
                        ..
                    }
                )
            })
            .count();
        assert_eq!(before_dispatch, 2);
    }

    #[test]
    fn dispatch_started_without_terminal_is_conservatively_indeterminate() {
        let plan_two = plan(2);
        let snap = snapshot(PendingStage::Bound {
            plan: plan_two.clone(),
        });
        let facts = facts_with_plan(
            &plan_two,
            vec![
                InvocationLifecycleFact::DispatchStarted,
                InvocationLifecycleFact::Reserved,
            ],
        );
        let result = closure(&snap, &facts, "plan");
        assert!(matches!(
            interrupted_reason(&result),
            TurnInterruption::InvocationMayHaveOccurred { invocation }
                if *invocation == plan_two.bindings[0].invocation_id
        ));
        let terminalizes: Vec<_> = result
            .actions
            .iter()
            .filter_map(|a| match a {
                ReconciliationAction::TerminalizeInvocation { outcome, .. } => {
                    Some(outcome.clone())
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            terminalizes[0],
            ExecutionOutcome::Indeterminate {
                reason: IndeterminateReason::MayHaveOccurred
            }
        );
        assert_eq!(
            terminalizes[1],
            ExecutionOutcome::NotExecuted {
                reason: NotExecutedReason::PrecedingInvocationIndeterminate
            }
        );
    }

    #[test]
    fn terminal_fact_in_the_binding_window_is_corruption() {
        let plan_one = plan(1);
        let snap = snapshot(PendingStage::Bound {
            plan: plan_one.clone(),
        });
        let facts = facts_with_plan(
            &plan_one,
            vec![InvocationLifecycleFact::Terminal {
                outcome: ExecutionOutcome::Succeeded,
                material: ResultMaterial::Unavailable {
                    reason: UnavailableMaterialReason::DraftPublicationFailed,
                    digest: None,
                    byte_length: None,
                },
            }],
        );
        assert!(matches!(
            reconcile(&snap, &facts),
            Err(SessionError::JournalCorruption { .. })
        ));
    }

    fn draft_for(binding: &PlannedBinding, content: &str) -> super::super::draft::ResultDraft {
        super::super::draft::ResultDraft::new(
            binding.invocation_id,
            binding.binding_digest,
            turn_id(),
            binding.address.message,
            binding.address.part_ordinal,
            content.to_string(),
        )
        .unwrap()
    }

    #[test]
    fn step_closing_resumes_the_cursor_binding_from_its_terminal_fact() {
        let plan_two = plan(2);
        let binding0 = &plan_two.bindings[0];
        let draft = draft_for(binding0, "{\"rows\":[]}");
        let closed0 = ClosedBinding {
            binding: binding0.clone(),
            outcome: ToolResultOutcome::Succeeded,
            material: MaterialAvailability::Available {
                digest: draft.content_digest,
                byte_length: draft.byte_length,
            },
        };
        let state = StepClosingState {
            plan_key: plan_two.plan_key,
            bindings: plan_two.bindings.clone(),
            closure_cursor: 1,
            closures: vec![closed0],
            current_candidate: None,
        };
        let snap = snapshot(PendingStage::StepClosing(state));
        let facts = facts_with_plan(
            &plan_two,
            vec![
                InvocationLifecycleFact::Terminal {
                    outcome: ExecutionOutcome::Succeeded,
                    material: ResultMaterial::Available {
                        digest: draft.content_digest,
                        byte_length: draft.byte_length,
                    },
                },
                InvocationLifecycleFact::Reserved,
            ],
        );
        let result = closure(&snap, &facts, "plan");
        assert!(matches!(
            interrupted_reason(&result),
            TurnInterruption::ProcessInterruptedDuringStepClosing
        ));
        // Earlier ordinals are immutable: no action touches binding 0.
        assert!(result.actions.iter().all(|a| match a {
            ReconciliationAction::TerminalizeInvocation { invocation, .. } => {
                *invocation != binding0.invocation_id
            }
            _ => true,
        }));
        // The cursor resumes at 1 and closes the remaining binding.
        let cursors: Vec<u32> = result
            .actions
            .iter()
            .filter_map(|a| match a {
                ReconciliationAction::AdvanceStepClosing { state } => Some(state.closure_cursor),
                _ => None,
            })
            .collect();
        assert_eq!(cursors, vec![2]);
        // The candidate keeps both the durable closure framing and the user
        // message.
        let candidate = result
            .actions
            .iter()
            .find_map(|a| match a {
                ReconciliationAction::PublishTurnClosing { candidate } => Some(candidate),
                _ => None,
            })
            .expect("candidate");
        // user + the durable closure 0 + the resumed closure 1 framing.
        assert_eq!(candidate.messages.len(), 3);
        assert_eq!(candidate.messages[1].role, "tool");
        assert_eq!(candidate.messages[2].role, "tool");
    }

    #[test]
    fn step_closing_material_mismatch_with_pinned_candidate_is_corruption() {
        let plan_one = plan(1);
        let binding0 = &plan_one.bindings[0];
        let draft = draft_for(binding0, "{\"rows\":[]}");
        let wrong = draft_for(binding0, "{\"rows\":[1]}");
        let state = StepClosingState {
            plan_key: plan_one.plan_key,
            bindings: plan_one.bindings.clone(),
            closure_cursor: 0,
            closures: vec![],
            current_candidate: Some(draft.clone()),
        };
        let snap = snapshot(PendingStage::StepClosing(state));
        let facts = facts_with_plan(
            &plan_one,
            vec![InvocationLifecycleFact::Terminal {
                outcome: ExecutionOutcome::Succeeded,
                material: ResultMaterial::Available {
                    digest: wrong.content_digest,
                    byte_length: wrong.byte_length,
                },
            }],
        );
        assert!(matches!(
            reconcile(&snap, &facts),
            Err(SessionError::JournalCorruption { .. })
        ));
    }

    #[test]
    fn step_closing_with_dispatch_started_and_no_terminal_is_indeterminate() {
        let plan_one = plan(1);
        let state = StepClosingState {
            plan_key: plan_one.plan_key,
            bindings: plan_one.bindings.clone(),
            closure_cursor: 0,
            closures: vec![],
            current_candidate: Some(draft_for(&plan_one.bindings[0], "{}")),
        };
        let snap = snapshot(PendingStage::StepClosing(state));
        let facts = facts_with_plan(&plan_one, vec![InvocationLifecycleFact::DispatchStarted]);
        let result = closure(&snap, &facts, "plan");
        assert!(matches!(
            interrupted_reason(&result),
            TurnInterruption::InvocationMayHaveOccurred { .. }
        ));
    }

    #[test]
    fn step_closing_skipped_ordinal_is_corruption() {
        let plan_two = plan(2);
        let state = StepClosingState {
            plan_key: plan_two.plan_key,
            bindings: plan_two.bindings.clone(),
            closure_cursor: 0,
            closures: vec![],
            current_candidate: None,
        };
        let snap = snapshot(PendingStage::StepClosing(state));
        // Binding 1 dispatched while binding 0 is still unclosed.
        let facts = facts_with_plan(
            &plan_two,
            vec![
                InvocationLifecycleFact::Reserved,
                InvocationLifecycleFact::DispatchStarted,
            ],
        );
        assert!(matches!(
            reconcile(&snap, &facts),
            Err(SessionError::JournalCorruption { .. })
        ));
    }

    #[test]
    fn step_closed_variants_map_to_their_unique_closures() {
        // FinishTurn -> Completed.
        let snap = snapshot(PendingStage::StepClosed {
            kind: StepClosedKind::FinishTurn {
                final_message: text_message("done"),
            },
        });
        let result = closure(&snap, &empty_facts(), "plan");
        assert!(matches!(
            result.outcome,
            TurnOutcome::Completed {
                finish_reason: TurnFinishReason::Stop
            }
        ));
        // No new StepClosed action: the step is already closed.
        assert!(
            !result
                .actions
                .iter()
                .any(|a| matches!(a, ReconciliationAction::PublishStepClosed { .. }))
        );

        // ContinueModel -> interrupted, never a next provider request.
        let closed_plan = plan(1);
        let snap = snapshot(PendingStage::StepClosed {
            kind: StepClosedKind::ContinueModel {
                model_step_ordinal: closed_plan.plan_key.model_step_ordinal,
                ordered_tool_results: vec![ClosedBinding {
                    binding: closed_plan.bindings[0].clone(),
                    outcome: ToolResultOutcome::NotExecuted,
                    material: MaterialAvailability::Unavailable {
                        reason: UnavailableMaterialReason::NotExecuted,
                        digest: None,
                        byte_length: None,
                    },
                }],
                conversation_digest: [0u8; 32],
            },
        });
        let result = closure(&snap, &empty_facts(), "plan");
        assert!(matches!(
            interrupted_reason(&result),
            TurnInterruption::WhileAwaitingModelContinuation
        ));

        // FailTurn / CancelTurn / InterruptTurn pass their typed reasons.
        let snap = snapshot(PendingStage::StepClosed {
            kind: StepClosedKind::FailTurn {
                reason: TurnFailure::ModelStepLimitExceeded,
            },
        });
        let result = closure(&snap, &empty_facts(), "plan");
        assert!(matches!(
            result.outcome,
            TurnOutcome::Failed {
                reason: TurnFailure::ModelStepLimitExceeded
            }
        ));

        let snap = snapshot(PendingStage::StepClosed {
            kind: StepClosedKind::CancelTurn {
                reason: super::super::outcome::TurnCancellation::UserCancelled,
            },
        });
        let result = closure(&snap, &empty_facts(), "plan");
        assert!(matches!(
            result.outcome,
            TurnOutcome::Cancelled {
                reason: super::super::outcome::TurnCancellation::UserCancelled
            }
        ));
    }

    #[test]
    fn turn_closing_and_committed_only_retry_the_exact_transaction() {
        let candidate = super::super::tests_support::completed_candidate();
        let snap = snapshot(PendingStage::TurnClosing(
            super::super::contract::TurnClosingFacts {
                turn_id: turn_id(),
                candidate: candidate.clone(),
            },
        ));
        let result = closure(&snap, &empty_facts(), "plan");
        assert_eq!(result.actions, vec![ReconciliationAction::FinalizeTurn]);
        assert_eq!(result.outcome, candidate.outcome);

        let snap = snapshot(PendingStage::Committed(
            super::super::contract::CommittedFacts {
                turn_id: turn_id(),
                candidate: candidate.clone(),
            },
        ));
        let result = closure(&snap, &empty_facts(), "plan");
        assert_eq!(result.actions, vec![ReconciliationAction::FinalizeTurn]);
    }

    // ─── Review-round reducer depth (F9/F11) ────────────────────────────

    #[test]
    fn a_snapshot_without_a_journal_commit_time_fails_closed() {
        // F9: `created_at` is the durable journal tip's own committer time.
        // A snapshot that carries none did not come from the store, so the
        // reducer cannot pin a durable candidate timestamp — and it must not
        // substitute another field. Fail closed instead of guessing.
        let mut snap = snapshot(PendingStage::MessageComplete {
            key: key(0, 0),
            message: text_message("done"),
            usage: None,
        });
        snap.recorded_at = None;
        let err = reconcile(&snap, &empty_facts()).expect_err("no durable commit time");
        assert!(
            matches!(err, SessionError::JournalCorruption { .. }),
            "got {err:?}"
        );

        // With the store-filled time the same snapshot closes normally, and
        // the candidate pins exactly that time (no new clock).
        snap.recorded_at = Some(STEP_TIME);
        let plan = closure(&snap, &empty_facts(), "plan");
        let pinned = plan
            .actions
            .iter()
            .find_map(|action| match action {
                ReconciliationAction::PublishTurnClosing { candidate } => Some(candidate),
                _ => None,
            })
            .expect("closure candidate");
        assert_eq!(
            pinned.created_at,
            chrono::DateTime::from_timestamp(STEP_TIME, 0)
                .expect("representable")
                .to_rfc3339()
        );
    }

    #[test]
    fn collision_at_binding_planned_requests_candidate_revision() {
        // Truth table #2 (D-A2): a candidate ID is owned by another plan,
        // no reservation exists for the current PlanKey. The reducer
        // returns a candidate-revision request — not a closure, and not a
        // provisional TurnOutcome that could be mistaken for one.
        let plan = plan(1);
        let other_key = PlanKey {
            turn_id: lancet_protocol::TurnId::parse("aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee")
                .unwrap(),
            model_step_ordinal: 0,
        };
        let mut facts = InvocationFactsSnapshot::empty();
        // The candidate ID is owned by another plan.
        facts.invocations.insert(
            plan.bindings[0].invocation_id,
            super::super::facts::InvocationRecordFact {
                owner: other_key,
                lifecycle: InvocationLifecycleFact::Reserved,
            },
        );
        let snap = snapshot(PendingStage::BindingPlanned {
            plan: plan.clone(),
            usage: None,
        });
        let decision = reconcile(&snap, &facts).expect("collision decision");
        assert!(
            decision.closure().is_none(),
            "a collision is not a Turn closure: {decision:?}"
        );
        let request = decision.revision().expect("candidate revision request");
        assert_eq!(request.session_id, snap.session_id);
        assert_eq!(request.turn_id, snap.turn_id);
        assert_eq!(request.plan, plan);
        assert_eq!(
            request.colliding_candidates,
            vec![plan.bindings[0].invocation_id]
        );
    }

    #[test]
    fn collision_at_bound_is_corruption_not_a_revision() {
        // `Bound` is published only after the exact plan is durably
        // reserved (ADR 0005 §9). A foreign owner there means the
        // reservation was lost; minting fresh candidates would invent one.
        let plan = plan(1);
        let other_key = PlanKey {
            turn_id: lancet_protocol::TurnId::parse("aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee")
                .unwrap(),
            model_step_ordinal: 0,
        };
        let mut facts = InvocationFactsSnapshot::empty();
        facts.invocations.insert(
            plan.bindings[0].invocation_id,
            super::super::facts::InvocationRecordFact {
                owner: other_key,
                lifecycle: InvocationLifecycleFact::Reserved,
            },
        );
        let snap = snapshot(PendingStage::Bound { plan: plan.clone() });
        assert!(matches!(
            reconcile(&snap, &facts),
            Err(SessionError::JournalCorruption { .. })
        ));
    }

    #[test]
    fn different_durable_reservation_is_corruption() {
        // Truth table #7: same PlanKey, different bindings → corruption.
        let plan = plan(1);
        let mut facts = InvocationFactsSnapshot::empty();
        let mut different = plan.bindings.clone();
        different[0].descriptor_identity = "another-descriptor".to_string();
        different[0].binding_digest = PlannedBinding::compute_binding_digest(
            &different[0].invocation_id,
            &different[0].address,
            &different[0].method,
            &different[0].descriptor_identity,
            &different[0].arguments_digest,
        );
        facts.plan = Some(super::super::facts::PlanFacts {
            plan_key: plan.plan_key,
            bindings: different,
        });
        for binding in &plan.bindings {
            facts.invocations.insert(
                binding.invocation_id,
                super::super::facts::InvocationRecordFact {
                    owner: plan.plan_key,
                    lifecycle: InvocationLifecycleFact::Reserved,
                },
            );
        }
        let snap = snapshot(PendingStage::BindingPlanned {
            plan: plan.clone(),
            usage: None,
        });
        let err = reconcile(&snap, &facts).expect_err("different reservation");
        assert!(
            matches!(err, SessionError::JournalCorruption { .. }),
            "got {err:?}"
        );
    }

    #[test]
    fn exact_durable_reservation_does_not_re_reserve() {
        // Truth table #4: exact match → continue without reserving.
        let plan = plan(1);
        let mut exact = InvocationFactsSnapshot::empty();
        exact.plan = Some(super::super::facts::PlanFacts {
            plan_key: plan.plan_key,
            bindings: plan.bindings.clone(),
        });
        for binding in &plan.bindings {
            exact.invocations.insert(
                binding.invocation_id,
                super::super::facts::InvocationRecordFact {
                    owner: plan.plan_key,
                    lifecycle: InvocationLifecycleFact::Reserved,
                },
            );
        }
        let snap = snapshot(PendingStage::BindingPlanned {
            plan: plan.clone(),
            usage: None,
        });
        let plan_out = closure(&snap, &exact, "exact reservation closes");
        assert!(
            !plan_out
                .actions
                .iter()
                .any(|a| matches!(a, ReconciliationAction::ReserveExactPlan { .. })),
            "an already-durable exact plan is never reserved again"
        );
    }
}
