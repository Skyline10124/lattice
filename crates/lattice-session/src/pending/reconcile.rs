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
    ExecutionOutcome, IndeterminateReason, InvocationFact, InvocationFactsSnapshot,
    NotExecutedReason, ResultMaterial, UnavailableMaterialReason,
};
use super::outcome::{
    ClosureOrigin, TurnFailure, TurnFinishReason, TurnInterruption, TurnOutcome,
    TurnUsageAggregate,
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

/// The pure decision function. `Err` is typed corruption: facts that
/// cannot describe the durable stage (partial or different reservations,
/// impossible orderings, digest mismatches) fail closed instead of being
/// guessed into an outcome.
pub fn reconcile(
    snapshot: &PendingSnapshot,
    facts: &InvocationFactsSnapshot,
) -> Result<ReconciliationPlan> {
    snapshot.validate()?;
    let (outcome, mut actions) = match &snapshot.stage {
        PendingStage::TurnStarted { .. } => {
            let reason = TurnInterruption::ProcessInterruptedBeforeModelAttempt;
            (interrupted(reason.clone()), vec![step_closed_interrupt(reason)])
        }
        PendingStage::ModelAttemptStarted { .. } => {
            let reason = TurnInterruption::ProcessInterruptedDuringModelAttempt;
            (interrupted(reason.clone()), vec![step_closed_interrupt(reason)])
        }
        PendingStage::ModelAttemptFailed { key, retryable, .. } => {
            let root = require_root(snapshot)?;
            let exhausted = key.attempt_ordinal + 1 >= root.config.limits.max_model_attempts;
            if *retryable && !exhausted {
                let reason = TurnInterruption::ProcessInterruptedDuringModelRetry;
                (interrupted(reason.clone()), vec![step_closed_interrupt(reason)])
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
            binding_window_closure(snapshot, plan, facts, true)?
        }
        PendingStage::Bound { plan } => binding_window_closure(snapshot, plan, facts, false)?,
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
        let messages = candidate_messages(snapshot, root);
        actions.push(ReconciliationAction::PublishTurnClosing {
            candidate: build_candidate(snapshot, root, outcome.clone(), messages),
        });
        actions.push(ReconciliationAction::FinalizeTurn);
    }

    Ok(ReconciliationPlan {
        session_id: snapshot.session_id,
        turn_id: snapshot.turn_id,
        outcome,
        origin: ClosureOrigin::Reconciled,
        actions,
    })
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
) -> Result<(TurnOutcome, Vec<ReconciliationAction>)> {
    facts.validate_against_plan(&plan.plan_key, &plan.bindings)?;
    let mut actions = Vec::new();
    if facts.plan.is_none() {
        actions.push(ReconciliationAction::ReserveExactPlan {
            plan_key: plan.plan_key,
            bindings: plan.bindings.clone(),
        });
    }
    if publish_bound {
        actions.push(ReconciliationAction::PublishBound {
            plan: plan.clone(),
        });
    }

    // A terminal fact in the binding window is an ordering violation: the
    // terminal follows the draft, the draft follows the candidate pin, and
    // the pin lives in a StepClosing snapshot.
    if plan
        .bindings
        .iter()
        .any(|b| matches!(facts.state_of(&b.invocation_id), Some(InvocationFact::Terminal { .. })))
    {
        return Err(SessionError::JournalCorruption {
            detail: "terminal invocation fact before any StepClosing snapshot".to_string(),
        });
    }

    let dispatch_at = plan.bindings.iter().position(|b| {
        matches!(
            facts.state_of(&b.invocation_id),
            Some(InvocationFact::DispatchStarted)
        )
    });

    match dispatch_at {
        Some(index) => {
            // The batch is ordered: later bindings cannot have been reached.
            for binding in plan.bindings.iter().skip(index + 1) {
                if facts.state_of(&binding.invocation_id) != Some(&InvocationFact::Reserved) {
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
            let classifications =
                classifications_for(&plan.bindings, 0, Some(index), None);
            actions.extend(closing_loop_actions(
                plan.plan_key,
                &plan.bindings,
                &[],
                &classifications,
            ));
            actions.push(step_closed_interrupt(reason.clone()));
            Ok((interrupted(reason), actions))
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
            Ok((interrupted(reason), actions))
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
    facts.validate_against_plan(&state.plan_key, &state.bindings)?;
    let cursor = state.closure_cursor as usize;

    // Bindings beyond the cursor must still be reserved: the batch is
    // strictly ordered and reopen never dispatches.
    for binding in state.bindings.iter().skip(cursor + 1) {
        if facts.state_of(&binding.invocation_id) != Some(&InvocationFact::Reserved) {
            return Err(SessionError::JournalCorruption {
                detail: format!(
                    "binding {} moved past the closure cursor without closing first",
                    binding.invocation_id
                ),
            });
        }
    }

    let current = state.bindings.get(cursor);
    let current_fact: Option<&InvocationFact> =
        current.and_then(|binding| facts.state_of(&binding.invocation_id));
    let candidate = state.current_candidate.as_ref();

    // (indeterminate ordinal, resume classification for the cursor binding)
    let (indeterminate_at, resume) = match (current, current_fact, candidate) {
        (None, _, _) => (None, None),
        (
            Some(_),
            Some(InvocationFact::Terminal {
                outcome,
                material,
            }),
            Some(draft),
        ) => {
            // Available material must be the pinned candidate's exact
            // content; a mismatch is corruption (ADR 0005 §12).
            if let ResultMaterial::Available { digest, byte_length } = material
                && (*digest != draft.content_digest || *byte_length != draft.byte_length)
            {
                return Err(SessionError::JournalCorruption {
                    detail: "terminal material does not match the pinned draft candidate"
                        .to_string(),
                });
            }
            (None, Some((outcome.clone(), material.clone())))
        }
        (Some(_), Some(InvocationFact::Terminal { .. }), None) => {
            return Err(SessionError::JournalCorruption {
                detail: "terminal invocation fact without a pinned draft candidate".to_string(),
            });
        }
        (Some(_), Some(InvocationFact::DispatchStarted), _) => (Some(cursor), None),
        (Some(_), Some(InvocationFact::Reserved), Some(_)) => {
            return Err(SessionError::JournalCorruption {
                detail: "pinned draft candidate for a binding that never dispatched".to_string(),
            });
        }
        (Some(_), Some(InvocationFact::Reserved), None) => (None, None),
        (Some(_), None, _) => {
            return Err(SessionError::JournalCorruption {
                detail: "closure-cursor binding has no reservation record".to_string(),
            });
        }
    };

    let mut actions = Vec::new();
    let classifications =
        classifications_for(&state.bindings, cursor, indeterminate_at, resume);
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
            if index == from && let Some(resume) = resume.clone() {
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
        ResultMaterial::Available { digest, byte_length } => MaterialAvailability::Available {
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
        _ => snapshot.root.as_ref().ok_or_else(|| SessionError::JournalCorruption {
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
fn candidate_messages(snapshot: &PendingSnapshot, root: &TurnStartedFacts) -> Vec<GraphMessage> {
    let mut messages = vec![user_transcript(root)];
    match &snapshot.stage {
        PendingStage::MessageComplete { message, .. } => messages.push(native_transcript(message)),
        PendingStage::StepClosed {
            kind: StepClosedKind::FinishTurn { final_message },
        } => messages.push(native_transcript(final_message)),
        PendingStage::StepClosed {
            kind: StepClosedKind::ContinueModel { ordered_tool_results, .. },
        } => {
            for closure in ordered_tool_results {
                messages.push(closure_transcript(closure));
            }
        }
        PendingStage::StepClosing(state) => {
            for closure in &state.closures {
                messages.push(closure_transcript(closure));
            }
        }
        _ => {}
    }
    messages
}

fn build_candidate(
    snapshot: &PendingSnapshot,
    root: &TurnStartedFacts,
    outcome: TurnOutcome,
    messages: Vec<GraphMessage>,
) -> TurnCommitCandidate {
    // The durable journal tip's own committer time: recovery introduces no
    // new clock. A manually built snapshot without one fails closed.
    let created_at = snapshot
        .recorded_at
        .and_then(|seconds| chrono::DateTime::from_timestamp(seconds, 0))
        .map(|time| time.to_rfc3339())
        .unwrap_or_else(|| root.accepted_at.clone());
    let commit_message = crate::graph::derive_commit_message(&messages);
    TurnCommitCandidate {
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
    }
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
        MaterialAvailability::Available { digest, byte_length } => serde_json::json!({
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
        let message = NativeMessage::new(
            lattice_conversation::MessageRole::Assistant,
            None,
            calls,
        );
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

    fn facts_with_plan(plan: &BindingPlan, states: Vec<InvocationFact>) -> InvocationFactsSnapshot {
        let mut facts = InvocationFactsSnapshot::empty();
        facts.plan = Some(super::super::facts::PlanFacts {
            plan_key: plan.plan_key,
            bindings: plan.bindings.clone(),
        });
        for (binding, state) in plan.bindings.iter().zip(states) {
            facts.invocations.insert(binding.invocation_id, state);
        }
        facts
    }

    fn empty_facts() -> InvocationFactsSnapshot {
        InvocationFactsSnapshot::empty()
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
        assert!(!kinds.iter().any(|k| **k == *"dispatch" || **k == *"continue"));
    }

    #[test]
    fn turn_started_without_attempt_closes_interrupted_before_model() {
        let snap = PendingSnapshot {
            predecessor: None,
            stage: PendingStage::TurnStarted(root()),
            ..snapshot(PendingStage::TurnStarted(root()))
        };
        let plan = reconcile(&snap, &empty_facts()).expect("plan");
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
        let plan = reconcile(&snap, &empty_facts()).expect("plan");
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
        let plan = reconcile(&snap, &empty_facts()).expect("plan");
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
        let plan = reconcile(&snap, &empty_facts()).expect("plan");
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
        let plan = reconcile(&snap, &empty_facts()).expect("plan");
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
        let plan = reconcile(&snap, &empty_facts()).expect("plan");
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
        let plan = reconcile(&snap, &empty_facts()).expect("plan");
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
        let plan = reconcile(&snap, &empty_facts()).expect("plan");
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
        assert_eq!(candidate.created_at, chrono::DateTime::from_timestamp(STEP_TIME, 0).unwrap().to_rfc3339());
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
        let plan = reconcile(&snap, &empty_facts()).expect("plan");
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
        let result = reconcile(&snap, &empty_facts()).expect("plan");
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
            assert!(plan_two.bindings.iter().any(|b| b.invocation_id == *invocation));
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
        let facts = facts_with_plan(&plan_one, vec![InvocationFact::Reserved]);
        let result = reconcile(&snap, &facts).expect("plan");
        assert!(result
            .actions
            .iter()
            .any(|a| matches!(a, ReconciliationAction::PublishBound { .. })));
        assert!(!result
            .actions
            .iter()
            .any(|a| matches!(a, ReconciliationAction::ReserveExactPlan { .. })));
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
        partial
            .invocations
            .insert(plan_two.bindings[0].invocation_id, InvocationFact::Reserved);
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
            wrong
                .invocations
                .insert(binding.invocation_id, InvocationFact::Reserved);
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
            vec![InvocationFact::Reserved, InvocationFact::Reserved],
        );
        let result = reconcile(&snap, &facts).expect("plan");
        assert!(matches!(
            interrupted_reason(&result),
            TurnInterruption::ProcessInterruptedDuringToolBinding
        ));
        let before_dispatch = result
            .actions
            .iter()
            .filter(|a| matches!(
                a,
                ReconciliationAction::TerminalizeInvocation {
                    outcome: ExecutionOutcome::NotExecuted {
                        reason: NotExecutedReason::BeforeDispatch
                    },
                    ..
                }
            ))
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
            vec![InvocationFact::DispatchStarted, InvocationFact::Reserved],
        );
        let result = reconcile(&snap, &facts).expect("plan");
        assert!(matches!(
            interrupted_reason(&result),
            TurnInterruption::InvocationMayHaveOccurred { invocation }
                if *invocation == plan_two.bindings[0].invocation_id
        ));
        let terminalizes: Vec<_> = result
            .actions
            .iter()
            .filter_map(|a| match a {
                ReconciliationAction::TerminalizeInvocation { outcome, .. } => Some(outcome.clone()),
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
            vec![InvocationFact::Terminal {
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
                InvocationFact::Terminal {
                    outcome: ExecutionOutcome::Succeeded,
                    material: ResultMaterial::Available {
                        digest: draft.content_digest,
                        byte_length: draft.byte_length,
                    },
                },
                InvocationFact::Reserved,
            ],
        );
        let result = reconcile(&snap, &facts).expect("plan");
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
        assert_eq!(candidate.messages.len(), 2); // user + closure 0
        assert_eq!(candidate.messages[1].role, "tool");
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
            vec![InvocationFact::Terminal {
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
        let facts =
            facts_with_plan(&plan_one, vec![InvocationFact::DispatchStarted]);
        let result = reconcile(&snap, &facts).expect("plan");
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
            vec![InvocationFact::Reserved, InvocationFact::DispatchStarted],
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
        let result = reconcile(&snap, &empty_facts()).expect("plan");
        assert!(matches!(
            result.outcome,
            TurnOutcome::Completed {
                finish_reason: TurnFinishReason::Stop
            }
        ));
        // No new StepClosed action: the step is already closed.
        assert!(!result
            .actions
            .iter()
            .any(|a| matches!(a, ReconciliationAction::PublishStepClosed { .. })));

        // ContinueModel -> interrupted, never a next provider request.
        let snap = snapshot(PendingStage::StepClosed {
            kind: StepClosedKind::ContinueModel {
                ordered_tool_results: vec![],
                conversation_digest: [0u8; 32],
            },
        });
        let result = reconcile(&snap, &empty_facts()).expect("plan");
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
        let result = reconcile(&snap, &empty_facts()).expect("plan");
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
        let result = reconcile(&snap, &empty_facts()).expect("plan");
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
        let result = reconcile(&snap, &empty_facts()).expect("plan");
        assert_eq!(result.actions, vec![ReconciliationAction::FinalizeTurn]);
        assert_eq!(result.outcome, candidate.outcome);

        let snap = snapshot(PendingStage::Committed(super::super::contract::CommittedFacts {
            turn_id: turn_id(),
            candidate: candidate.clone(),
        }));
        let result = reconcile(&snap, &empty_facts()).expect("plan");
        assert_eq!(result.actions, vec![ReconciliationAction::FinalizeTurn]);
    }
}
