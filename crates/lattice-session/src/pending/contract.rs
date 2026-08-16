// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! The versioned PendingTurn snapshot contract and pure stage-transition
//! validation (ADR 0005 §9, GitHub #144).
//!
//! Every snapshot is one immutable journal commit whose payload is the
//! canonical JSON of [`PendingSnapshot`] at `pending.json`, whose git parent
//! is the predecessor snapshot (for `TurnStarted`: the frozen parent
//! conversation tip), and whose publication under
//! `refs/lattice/sessions/<SessionId>/active-turn` is one validated CAS.
//! `Committed` is a journal object vocabulary member but is never published
//! through `active-turn`; the final SessionStore ref transaction is its only
//! publication path.

use lancet_protocol::{ModelAttemptKey, SessionId, TurnId};
use lattice_conversation::{
    AttemptInterruption, AttemptRejection, NativeMessage, TokenUsage, UnboundToolCall,
};
use serde::{Deserialize, Serialize};

use super::binding::BindingPlan;
use super::config::{MAX_MODEL_STEPS, TurnStartedFacts};
use super::outcome::{ClosureOrigin, TurnOutcome, TurnUsageAggregate};
use super::step::{StepClosedKind, StepClosingState};
use crate::graph::GraphMessage;
use crate::{Result, SessionError};

/// Schema version of the `pending.json` payload.
pub const PENDING_SNAPSHOT_SCHEMA_VERSION: u32 = 1;

/// The twelve durable states of one pending Turn (ADR 0005 §9).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "stage")]
pub enum PendingStage {
    /// The durable request-acceptance boundary: parent tip, user message,
    /// explicit `TurnId`, complete frozen configuration.
    TurnStarted(TurnStartedFacts),
    /// An outbound provider request was about to start.
    ModelAttemptStarted {
        /// Which attempt of which step of which Turn.
        key: ModelAttemptKey,
    },
    /// A failure before any stream event; retryable classification is
    /// recorded (ADR 0005 §10).
    ModelAttemptFailed {
        /// Which attempt failed.
        key: ModelAttemptKey,
        /// The adapter's pre-stream retryability classification.
        retryable: bool,
        /// Provider-reported usage of the failed attempt, when supplied.
        usage: Option<TokenUsage>,
    },
    /// Partial stream, limit, cancellation, timeout, or crash.
    ModelAttemptInterrupted {
        /// Which attempt was interrupted.
        key: ModelAttemptKey,
        /// Durable typed interruption reason.
        reason: AttemptInterruption,
        /// Provider-reported usage, when supplied.
        usage: Option<TokenUsage>,
    },
    /// Provider `Done` with an invalid complete output.
    ModelAttemptRejected {
        /// Which attempt produced the invalid output.
        key: ModelAttemptKey,
        /// Durable typed rejection.
        rejection: AttemptRejection,
        /// Provider-reported usage, when supplied.
        usage: Option<TokenUsage>,
    },
    /// A valid complete zero-Tool message. A Tool-bearing valid message is
    /// published atomically as `BindingPlanned`; there is no Tool-bearing
    /// `MessageComplete` gap.
    MessageComplete {
        /// Which attempt completed.
        key: ModelAttemptKey,
        /// The complete valid message (validated zero-Tool).
        message: NativeMessage<UnboundToolCall>,
        /// Usage of the completing attempt, when reported.
        usage: Option<TokenUsage>,
    },
    /// The exact ordered binding plan with the embedded complete unbound
    /// Tool-bearing message.
    BindingPlanned {
        /// The plan.
        plan: BindingPlan,
        /// Usage of the completing attempt, when reported.
        usage: Option<TokenUsage>,
    },
    /// The entire exact plan is durably reserved.
    Bound {
        /// The plan, byte-equal to the `BindingPlanned` predecessor.
        plan: BindingPlan,
    },
    /// The ordered closing loop over the plan's bindings.
    StepClosing(StepClosingState),
    /// Every binding of the step is closed.
    StepClosed {
        /// The closed sum kind.
        kind: StepClosedKind,
    },
    /// The exact canonical `Committed` and TurnCommit candidate identities
    /// are pinned (including every commit-time field); the final ref
    /// transaction has not necessarily run.
    TurnClosing(TurnClosingFacts),
    /// The immutable final journal object constructed with the TurnCommit
    /// candidate. Never published through `active-turn`.
    Committed(CommittedFacts),
}

impl PendingStage {
    /// The stable stage name (wire spelling of the serde tag).
    pub fn stage_name(&self) -> &'static str {
        match self {
            Self::TurnStarted(_) => "turn_started",
            Self::ModelAttemptStarted { .. } => "model_attempt_started",
            Self::ModelAttemptFailed { .. } => "model_attempt_failed",
            Self::ModelAttemptInterrupted { .. } => "model_attempt_interrupted",
            Self::ModelAttemptRejected { .. } => "model_attempt_rejected",
            Self::MessageComplete { .. } => "message_complete",
            Self::BindingPlanned { .. } => "binding_planned",
            Self::Bound { .. } => "bound",
            Self::StepClosing(_) => "step_closing",
            Self::StepClosed { .. } => "step_closed",
            Self::TurnClosing(_) => "turn_closing",
            Self::Committed(_) => "committed",
        }
    }

    /// Validate this stage's own payload invariants (identity consistency is
    /// checked against the snapshot envelope by [`PendingSnapshot::validate`]).
    pub fn validate(&self) -> Result<()> {
        match self {
            Self::TurnStarted(facts) => facts.validate(),
            Self::ModelAttemptStarted { key } => validate_attempt_key(key),
            Self::ModelAttemptFailed { key, .. }
            | Self::ModelAttemptInterrupted { key, .. }
            | Self::ModelAttemptRejected { key, .. } => validate_attempt_key(key),
            Self::MessageComplete { key, message, .. } => {
                validate_attempt_key(key)?;
                if message.tool_calls().next().is_some() {
                    return Err(SessionError::JournalCorruption {
                        detail: "MessageComplete must be zero-Tool; a Tool-bearing valid \
                                 message is BindingPlanned"
                            .to_string(),
                    });
                }
                Ok(())
            }
            Self::BindingPlanned { plan, .. } => plan.validate(),
            Self::Bound { plan } => plan.validate(),
            Self::StepClosing(state) => state.validate(),
            Self::StepClosed { kind } => validate_step_closed(kind),
            Self::TurnClosing(facts) => facts.validate(),
            Self::Committed(facts) => facts.validate(),
        }
    }
}

fn validate_attempt_key(key: &ModelAttemptKey) -> Result<()> {
    if key.model_step_ordinal >= MAX_MODEL_STEPS {
        return Err(SessionError::JournalCorruption {
            detail: format!(
                "model step ordinal {} reaches the code-defined limit {MAX_MODEL_STEPS}",
                key.model_step_ordinal
            ),
        });
    }
    Ok(())
}

fn validate_step_closed(kind: &StepClosedKind) -> Result<()> {
    use lattice_conversation::MessageSemantic;
    match kind {
        StepClosedKind::FinishTurn { final_message } => {
            if final_message.tool_calls().next().is_some() {
                return Err(SessionError::JournalCorruption {
                    detail: "FinishTurn final message must be zero-Tool".to_string(),
                });
            }
            for part in &final_message.parts {
                if let MessageSemantic::ToolResult { .. } = part.semantic {
                    return Err(SessionError::JournalCorruption {
                        detail: "FinishTurn final message must not carry tool results"
                            .to_string(),
                    });
                }
            }
            Ok(())
        }
        StepClosedKind::ContinueModel {
            ordered_tool_results, ..
        } => {
            for closure in ordered_tool_results {
                closure.binding.validate()?;
            }
            Ok(())
        }
        StepClosedKind::FailTurn { .. }
        | StepClosedKind::CancelTurn { .. }
        | StepClosedKind::InterruptTurn { .. } => Ok(()),
    }
}

/// The exact pinned TurnCommit candidate: every identity- and commit-time
/// field of the final conversation commit, frozen before the final ref
/// transaction is attempted. Recovery never reconstructs these from a new
/// clock or mutable input (ADR 0005 §6/§9).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TurnCommitCandidate {
    /// Owning session (store id form).
    pub session_id: String,
    /// The Turn's explicit identity.
    pub turn_id: TurnId,
    /// Logical conversation parents; `parent_ids[0]` is the frozen parent
    /// conversation tip of `TurnStarted`.
    pub parent_ids: Vec<String>,
    /// Transcript content in order (user message, closures, final message).
    pub messages: Vec<GraphMessage>,
    /// Agent name for the commit projection.
    pub agent_name: String,
    /// Frozen profile snapshot hash.
    pub profile_snapshot_hash: String,
    /// Frozen canonical profile snapshot JSON.
    pub profile_snapshot_json: String,
    /// Frozen model selection.
    pub model: String,
    /// Frozen provider selection.
    pub provider: String,
    /// The typed Turn outcome authority.
    pub outcome: TurnOutcome,
    /// Whether this outcome was decided live or by reconciliation.
    pub origin: ClosureOrigin,
    /// Commit message.
    pub commit_message: String,
    /// Derived per-Turn usage aggregate, when any attempt reported usage.
    pub usage_aggregate: Option<TurnUsageAggregate>,
    /// Project-global workspace-history checkpoint, when the closing Turn
    /// records one (ADR 0005 §15).
    pub workspace_history_oid: Option<String>,
    /// Pinned commit-time timestamp (RFC 3339). Reconciled closures pin the
    /// durable journal tip's commit time; a live owner pins its own clock at
    /// pinning time. Either way the value is frozen here before publication.
    pub created_at: String,
}

impl TurnCommitCandidate {
    /// The legacy conversation-manifest projection inputs derived from the
    /// typed outcome. Diagnostic only until the #139-B hard cut deletes the
    /// legacy fields; the typed value remains the authority.
    pub fn legacy_status(&self) -> crate::graph::TurnCommitStatus {
        self.outcome.legacy_status()
    }

    fn validate(&self) -> Result<()> {
        if self.session_id.is_empty() || self.agent_name.is_empty() {
            return Err(SessionError::JournalCorruption {
                detail: "candidate session/agent must not be empty".to_string(),
            });
        }
        if self.messages.is_empty() {
            return Err(SessionError::JournalCorruption {
                detail: "candidate must carry the user message".to_string(),
            });
        }
        chrono::DateTime::parse_from_rfc3339(&self.created_at)
            .map_err(|e| {
                SessionError::JournalCorruption {
                    detail: format!("candidate created_at is not RFC 3339: {e}"),
                }
            })
            .map(|_| ())
    }
}

/// The facts pinned by `TurnClosing`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TurnClosingFacts {
    /// The Turn being closed.
    pub turn_id: TurnId,
    /// The pinned TurnCommit candidate.
    pub candidate: TurnCommitCandidate,
}

impl TurnClosingFacts {
    fn validate(&self) -> Result<()> {
        if self.candidate.turn_id != self.turn_id {
            return Err(SessionError::JournalCorruption {
                detail: "TurnClosing candidate names a different Turn".to_string(),
            });
        }
        self.candidate.validate()
    }
}

/// The payload of the final `Committed` journal object: the data used to
/// construct the TurnCommit, never its OID (ADR 0005 §9).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CommittedFacts {
    /// The Turn being committed.
    pub turn_id: TurnId,
    /// The exact candidate the TurnCommit was constructed from.
    pub candidate: TurnCommitCandidate,
}

impl CommittedFacts {
    fn validate(&self) -> Result<()> {
        if self.candidate.turn_id != self.turn_id {
            return Err(SessionError::JournalCorruption {
                detail: "Committed candidate names a different Turn".to_string(),
            });
        }
        self.candidate.validate()
    }
}

/// One immutable PendingTurn journal snapshot. `recorded_at` carries the
/// journal commit's own committer time (durable); it is metadata filled by
/// the store on read, never part of the canonical payload bytes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PendingSnapshot {
    /// Payload schema version; must equal
    /// [`PENDING_SNAPSHOT_SCHEMA_VERSION`].
    pub schema_version: u32,
    /// Owning session.
    pub session_id: SessionId,
    /// The pending Turn's identity; identical across the whole chain.
    pub turn_id: TurnId,
    /// Predecessor snapshot commit oid hex; `None` only for `TurnStarted`.
    pub predecessor: Option<String>,
    /// The durable state this snapshot records.
    pub stage: PendingStage,
    /// Committer time of the journal commit this snapshot was read from
    /// (unix seconds); filled by the store, skipped on the wire.
    #[serde(skip)]
    pub recorded_at: Option<i64>,
}

impl PendingSnapshot {
    /// Validate the envelope: schema version, identity presence, the
    /// stage's own payload, and stage/turn consistency.
    pub fn validate(&self) -> Result<()> {
        if self.schema_version != PENDING_SNAPSHOT_SCHEMA_VERSION {
            return Err(SessionError::JournalCorruption {
                detail: format!(
                    "pending snapshot schema version {} is not {}",
                    self.schema_version, PENDING_SNAPSHOT_SCHEMA_VERSION
                ),
            });
        }
        self.stage.validate()?;
        match &self.stage {
            PendingStage::TurnStarted(facts) => {
                if self.predecessor.is_some() {
                    return Err(SessionError::JournalCorruption {
                        detail: "TurnStarted must be the first journal snapshot".to_string(),
                    });
                }
                if facts.turn_id != self.turn_id || facts.session_id != self.session_id {
                    return Err(SessionError::JournalCorruption {
                        detail: "TurnStarted facts do not match the snapshot identity"
                            .to_string(),
                    });
                }
            }
            PendingStage::ModelAttemptStarted { key }
            | PendingStage::ModelAttemptFailed { key, .. }
            | PendingStage::ModelAttemptInterrupted { key, .. }
            | PendingStage::ModelAttemptRejected { key, .. }
            | PendingStage::MessageComplete { key, .. } => {
                if key.turn_id != self.turn_id {
                    return Err(SessionError::JournalCorruption {
                        detail: "attempt key does not match the snapshot Turn".to_string(),
                    });
                }
            }
            PendingStage::BindingPlanned { plan, .. } | PendingStage::Bound { plan } => {
                if plan.plan_key.turn_id != self.turn_id {
                    return Err(SessionError::JournalCorruption {
                        detail: "plan key does not match the snapshot Turn".to_string(),
                    });
                }
            }
            PendingStage::StepClosing(state) => {
                if state.plan_key.turn_id != self.turn_id {
                    return Err(SessionError::JournalCorruption {
                        detail: "step closing plan does not match the snapshot Turn".to_string(),
                    });
                }
            }
            PendingStage::StepClosed { .. } => {}
            PendingStage::TurnClosing(facts) => {
                if facts.turn_id != self.turn_id {
                    return Err(SessionError::JournalCorruption {
                        detail: "TurnClosing facts do not match the snapshot Turn".to_string(),
                    });
                }
            }
            PendingStage::Committed(facts) => {
                if facts.turn_id != self.turn_id {
                    return Err(SessionError::JournalCorruption {
                        detail: "Committed facts do not match the snapshot Turn".to_string(),
                    });
                }
            }
        }
        Ok(())
    }
}

/// Pure stage-transition validation: may `next` legally follow `prev` in one
/// Turn's journal (ADR 0005 §9/§10)? Structural stage-shape rules; the
/// publication CAS remains the authority for race safety.
///
/// Rules:
///
/// - `TurnStarted` is only legal as the first snapshot (no predecessor).
/// - A model step starts from `TurnStarted` (step 0) or
///   `StepClosed(ContinueModel)` (the next ordinal), or a retry attempt
///   starts from a retryable `ModelAttemptFailed` of the same step.
/// - `ModelAttemptFailed`/`Interrupted`/`Rejected`/`MessageComplete`/
///   `BindingPlanned` follow `ModelAttemptStarted` of the same attempt key.
/// - `MessageComplete` must be zero-Tool; `BindingPlanned` must carry at
///   least one binding.
/// - `Bound` follows `BindingPlanned` with the byte-equal exact plan.
/// - `StepClosing` follows `Bound` (cursor 0) or `StepClosing` (advance the
///   lowest unclosed ordinal only: pin the current candidate, then extend
///   closures by exactly one, never both at once).
/// - `StepClosed` follows `StepClosing` with every binding closed, or closes
///   the Turn early from `TurnStarted`/attempt stages/`MessageComplete`/
///   `BindingPlanned`/`Bound` (reopen closure paths).
/// - `TurnClosing` follows `StepClosed`.
/// - `Committed` never follows anything through `active-turn`.
pub fn validate_transition(prev: &PendingStage, next: &PendingStage) -> Result<()> {
    use PendingStage::*;
    let mismatch = |detail: String| Err(SessionError::InvalidInput(detail));
    let same_attempt = |started: &lancet_protocol::ModelAttemptKey,
                        done: &lancet_protocol::ModelAttemptKey|
     -> Result<()> {
        if started == done {
            Ok(())
        } else {
            mismatch(format!(
                "attempt outcome {done:?} does not follow started attempt {started:?}"
            ))
        }
    };
    match (prev, next) {
        // A retry attempt after a pre-stream retryable failure of the same
        // step (ADR 0005 §10): new attempt ordinal, no new model step.
        (
            ModelAttemptFailed { key: failed, retryable: true, .. },
            ModelAttemptStarted { key: started },
        ) => {
            if started.turn_id == failed.turn_id
                && started.model_step_ordinal == failed.model_step_ordinal
                && started.attempt_ordinal == failed.attempt_ordinal + 1
            {
                Ok(())
            } else {
                mismatch(format!(
                    "retry attempt {started:?} does not follow failed attempt {failed:?}"
                ))
            }
        }
        // The next model step after a ContinueModel step closure.
        (
            StepClosed { kind: StepClosedKind::ContinueModel { .. } },
            ModelAttemptStarted { key },
        ) => validate_attempt_key(key),
        // The first model step starts from TurnStarted.
        (TurnStarted(_), ModelAttemptStarted { key }) => {
            if key.model_step_ordinal == 0 && key.attempt_ordinal == 0 {
                Ok(())
            } else {
                mismatch(format!(
                    "the first model step must be step 0 attempt 0, got {key:?}"
                ))
            }
        }
        // Attempt outcomes follow the started attempt of the same key.
        (ModelAttemptStarted { key: started }, ModelAttemptFailed { key: done, .. }) => {
            same_attempt(started, done)
        }
        (ModelAttemptStarted { key: started }, ModelAttemptInterrupted { key: done, .. }) => {
            same_attempt(started, done)
        }
        (ModelAttemptStarted { key: started }, ModelAttemptRejected { key: done, .. }) => {
            same_attempt(started, done)
        }
        (ModelAttemptStarted { key: started }, MessageComplete { key: done, .. }) => {
            same_attempt(started, done)
        }
        (ModelAttemptStarted { key: started }, BindingPlanned { plan, .. }) => {
            if plan.plan_key.turn_id == started.turn_id
                && plan.plan_key.model_step_ordinal == started.model_step_ordinal
            {
                Ok(())
            } else {
                mismatch(
                    "BindingPlanned step does not match the completing attempt".to_string(),
                )
            }
        }
        // Bound follows the byte-equal exact plan.
        (BindingPlanned { plan: planned, .. }, Bound { plan: bound }) => {
            if planned == bound {
                Ok(())
            } else {
                mismatch("Bound must carry the byte-equal exact planned plan".to_string())
            }
        }
        // The closing loop starts at cursor 0 over the bound plan.
        (Bound { plan }, StepClosing(state)) => {
            if state.closure_cursor == 0
                && state.closures.is_empty()
                && state.current_candidate.is_none()
                && state.plan_key == plan.plan_key
                && state.bindings == plan.bindings
            {
                Ok(())
            } else {
                mismatch("StepClosing must start at cursor 0 over the bound plan".to_string())
            }
        }
        // Within StepClosing: pin the current candidate, then close exactly
        // that binding; never both in one transition, never a skip.
        (StepClosing(prev_state), StepClosing(next_state)) => {
            if prev_state.plan_key != next_state.plan_key
                || prev_state.bindings != next_state.bindings
            {
                return mismatch("StepClosing must keep the same plan and bindings".to_string());
            }
            match (&prev_state.current_candidate, &next_state.current_candidate) {
                (None, Some(candidate)) => {
                    if prev_state.closure_cursor == next_state.closure_cursor
                        && prev_state.closures == next_state.closures
                    {
                        let index = next_state.closure_cursor as usize;
                        match next_state.bindings.get(index) {
                            Some(binding) if binding.invocation_id == candidate.invocation_id => {
                                Ok(())
                            }
                            _ => mismatch(
                                "candidate must pin the lowest unclosed binding".to_string(),
                            ),
                        }
                    } else {
                        mismatch("pinning a candidate must not move the cursor".to_string())
                    }
                }
                (Some(_), None) => {
                    if next_state.closure_cursor == prev_state.closure_cursor + 1
                        && next_state.closures.len() == prev_state.closures.len() + 1
                        && next_state.closures[..prev_state.closures.len()]
                            == prev_state.closures[..]
                    {
                        Ok(())
                    } else {
                        mismatch(
                            "only the lowest unclosed ordinal may advance, by exactly one"
                                .to_string(),
                        )
                    }
                }
                (None, None) => mismatch(
                    "StepClosing must pin a candidate or advance the cursor".to_string(),
                ),
                (Some(_), Some(_)) => mismatch(
                    "cursor advance must publish the pinned candidate's closure and clear it"
                        .to_string(),
                ),
            }
        }
        // StepClosed after every binding is closed.
        (StepClosing(state), StepClosed { .. }) => {
            if state.all_closed() {
                Ok(())
            } else {
                mismatch("StepClosed requires every binding closed".to_string())
            }
        }
        // Reopen closure paths close the step directly from pre-execution
        // stages; a live coordinator also closes failed/cancelled/interrupted
        // turns this way.
        (
            TurnStarted(_)
                | ModelAttemptStarted { .. }
                | ModelAttemptFailed { .. }
                | ModelAttemptInterrupted { .. }
                | ModelAttemptRejected { .. }
                | MessageComplete { .. }
                | BindingPlanned { .. }
                | Bound { .. },
            StepClosed { .. },
        ) => Ok(()),
        (StepClosed { .. }, TurnClosing(_)) => Ok(()),
        (_, Committed(_)) => mismatch(
            "Committed is never published through active-turn; the final ref \
             transaction is its only publication path"
                .to_string(),
        ),
        (prev, next) => mismatch(format!(
            "illegal stage transition {} -> {}",
            prev.stage_name(),
            next.stage_name()
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::config::{
        FrozenTurnConfig, ToolCatalogEntry, ToolCatalogSnapshot, TurnExecutionLimits,
    };
    use lancet_protocol::{ModelMessageAddress, PlanKey, SessionId, ToolCallAddress};
    use lattice_conversation::{
        MessagePart, MessageRole, MessageSemantic, ProviderOrigin,
    };

    const TURN: &str = "3f8b2c1a-7d4e-4f5a-8b9c-0d1e2f3a4b5c";
    const SESSION: &str = "3f8b2c1a7d4e4f5a8b9c0d1e2f3a4b5c";
    const INVOCATION: &str = "00000000-0000-4000-8000-000000000001";

    fn turn_id() -> TurnId {
        TurnId::parse(TURN).unwrap()
    }

    fn session_id() -> SessionId {
        SessionId::parse(SESSION).unwrap()
    }

    fn attempt_key(step: u32, attempt: u32) -> ModelAttemptKey {
        ModelAttemptKey {
            turn_id: turn_id(),
            model_step_ordinal: step,
            attempt_ordinal: attempt,
        }
    }

    fn turn_started() -> PendingStage {
        PendingStage::TurnStarted(TurnStartedFacts {
            turn_id: turn_id(),
            session_id: session_id(),
            parent_conversation_tip: None,
            user_message: NativeMessage::new(
                MessageRole::User,
                None,
                vec![MessagePart::semantic_only(MessageSemantic::Text {
                    text: "hello".to_string(),
                })],
            ),
            config: FrozenTurnConfig {
                agent_name: "coder".to_string(),
                model: "m".to_string(),
                provider: "p".to_string(),
                system_prompt: None,
                profile_snapshot_json: "{}".to_string(),
                policy_snapshot_json: "{}".to_string(),
                catalog: ToolCatalogSnapshot::new(
                    "b".to_string(),
                    vec![ToolCatalogEntry {
                        name: "t".to_string(),
                        descriptor_identity: "d".to_string(),
                        description: "t".to_string(),
                        input_schema: serde_json::json!({}),
                    }],
                )
                .unwrap(),
                limits: TurnExecutionLimits {
                    timeout_secs: 30,
                    max_model_steps: MAX_MODEL_STEPS,
                    max_model_attempts: 2,
                },
            },
        })
    }

    fn zero_tool_message() -> NativeMessage<UnboundToolCall> {
        NativeMessage::new(
            MessageRole::Assistant,
            Some(ProviderOrigin::Anthropic),
            vec![MessagePart::semantic_only(MessageSemantic::Text {
                text: "done".to_string(),
            })],
        )
    }

    fn one_call_plan() -> BindingPlan {
        let invocation = lancet_protocol::InvocationId::parse(INVOCATION).unwrap();
        let address = ToolCallAddress {
            message: ModelMessageAddress {
                session_id: session_id(),
                turn_id: turn_id(),
                model_step_ordinal: 0,
            },
            part_ordinal: 0,
        };
        let arguments = serde_json::json!({"a": 1});
        let args_digest = lattice_conversation::arguments_digest(&arguments);
        let binding = super::super::binding::PlannedBinding {
            binding_digest: super::super::binding::PlannedBinding::compute_binding_digest(
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
        };
        BindingPlan {
            plan_key: PlanKey {
                turn_id: turn_id(),
                model_step_ordinal: 0,
            },
            message: NativeMessage::new(
                MessageRole::Assistant,
                Some(ProviderOrigin::Anthropic),
                vec![MessagePart::semantic_only(MessageSemantic::ToolCall {
                    call: UnboundToolCall {
                        provider_call_id: None,
                        name: "t".to_string(),
                        arguments,
                    },
                })],
            ),
            bindings: vec![binding],
        }
    }

    fn step_closing_cursor_0(plan: &BindingPlan) -> super::super::step::StepClosingState {
        super::super::step::StepClosingState {
            plan_key: plan.plan_key,
            bindings: plan.bindings.clone(),
            closure_cursor: 0,
            closures: vec![],
            current_candidate: None,
        }
    }

    #[test]
    fn turn_started_must_be_first() {
        let started = turn_started();
        assert!(matches!(
            validate_transition(&started, &PendingStage::ModelAttemptStarted { key: attempt_key(0, 0) }),
            Ok(())
        ));
        // A second TurnStarted after anything is illegal.
        assert!(validate_transition(&started, &turn_started()).is_err());
        // The first attempt must be step 0 attempt 0.
        assert!(
            validate_transition(
                &started,
                &PendingStage::ModelAttemptStarted { key: attempt_key(1, 0) }
            )
            .is_err()
        );
    }

    #[test]
    fn retry_only_after_retryable_pre_stream_failure_of_next_ordinal() {
        let started = PendingStage::ModelAttemptStarted { key: attempt_key(0, 0) };
        let retryable = PendingStage::ModelAttemptFailed {
            key: attempt_key(0, 0),
            retryable: true,
            usage: None,
        };
        let terminal = PendingStage::ModelAttemptFailed {
            key: attempt_key(0, 0),
            retryable: false,
            usage: None,
        };
        assert!(validate_transition(&retryable, &PendingStage::ModelAttemptStarted { key: attempt_key(0, 1) }).is_ok());
        // Non-retryable failures never continue.
        assert!(validate_transition(&terminal, &PendingStage::ModelAttemptStarted { key: attempt_key(0, 1) }).is_err());
        // Retry must not skip an ordinal or change step.
        assert!(validate_transition(&retryable, &PendingStage::ModelAttemptStarted { key: attempt_key(0, 2) }).is_err());
        assert!(validate_transition(&retryable, &PendingStage::ModelAttemptStarted { key: attempt_key(1, 1) }).is_err());
        assert!(validate_transition(&started, &PendingStage::ModelAttemptFailed { key: attempt_key(0, 1), retryable: true, usage: None }).is_err());
    }

    #[test]
    fn attempt_outcomes_must_follow_the_same_started_attempt() {
        let started = PendingStage::ModelAttemptStarted { key: attempt_key(0, 0) };
        for stage in [
            PendingStage::ModelAttemptFailed { key: attempt_key(0, 0), retryable: false, usage: None },
            PendingStage::ModelAttemptInterrupted {
                key: attempt_key(0, 0),
                reason: AttemptInterruption::StreamError,
                usage: None,
            },
            PendingStage::ModelAttemptRejected {
                key: attempt_key(0, 0),
                rejection: AttemptRejection::InvalidToolCall {
                    validation: lattice_conversation::ToolCallValidation::MalformedArguments {
                        call_ordinal: 0,
                    },
                },
                usage: None,
            },
            PendingStage::MessageComplete {
                key: attempt_key(0, 0),
                message: zero_tool_message(),
                usage: None,
            },
        ] {
            assert!(validate_transition(&started, &stage).is_ok(), "{stage:?}");
        }
        assert!(
            validate_transition(
                &started,
                &PendingStage::MessageComplete { key: attempt_key(0, 1), message: zero_tool_message(), usage: None }
            )
            .is_err()
        );
    }

    #[test]
    fn bound_requires_the_byte_equal_plan_and_closing_starts_at_cursor_zero() {
        let planned = PendingStage::BindingPlanned {
            plan: one_call_plan(),
            usage: None,
        };
        let bound = PendingStage::Bound {
            plan: one_call_plan(),
        };
        assert!(validate_transition(&planned, &bound).is_ok());
        let mut drifted = one_call_plan();
        drifted.bindings[0].descriptor_identity = "other".to_string();
        assert!(validate_transition(
            &planned,
            &PendingStage::Bound { plan: drifted }
        )
        .is_err());

        let closing = PendingStage::StepClosing(step_closing_cursor_0(&one_call_plan()));
        assert!(validate_transition(&bound, &closing).is_ok());
        let mut jumped = step_closing_cursor_0(&one_call_plan());
        jumped.closure_cursor = 1;
        assert!(validate_transition(&bound, &PendingStage::StepClosing(jumped)).is_err());
    }

    #[test]
    fn committed_is_never_publishable_through_active_turn() {
        let started = turn_started();
        let facts = CommittedFacts {
            turn_id: turn_id(),
            candidate: candidate(),
        };
        assert!(validate_transition(&started, &PendingStage::Committed(facts)).is_err());
    }

    #[test]
    fn message_complete_must_be_zero_tool() {
        let mut snapshot = PendingSnapshot {
            schema_version: PENDING_SNAPSHOT_SCHEMA_VERSION,
            session_id: session_id(),
            turn_id: turn_id(),
            predecessor: None,
            stage: PendingStage::MessageComplete {
                key: attempt_key(0, 0),
                message: one_call_plan().message,
                usage: None,
            },
            recorded_at: None,
        };
        assert!(snapshot.validate().is_err());
        snapshot.stage = PendingStage::MessageComplete {
            key: attempt_key(0, 0),
            message: zero_tool_message(),
            usage: None,
        };
        snapshot.validate().expect("zero-tool message completes");
    }

    #[test]
    fn step_limit_is_enforced_on_attempt_keys() {
        let mut snapshot = PendingSnapshot {
            schema_version: PENDING_SNAPSHOT_SCHEMA_VERSION,
            session_id: session_id(),
            turn_id: turn_id(),
            predecessor: None,
            stage: PendingStage::ModelAttemptStarted { key: attempt_key(MAX_MODEL_STEPS, 0) },
            recorded_at: None,
        };
        assert!(
            snapshot.validate().is_err(),
            "a model step ordinal at the code-defined limit is corruption"
        );
        snapshot.stage = PendingStage::ModelAttemptStarted { key: attempt_key(MAX_MODEL_STEPS - 1, 0) };
        snapshot.validate().expect("the last legal step ordinal");
    }

    fn candidate() -> TurnCommitCandidate {
        TurnCommitCandidate {
            session_id: SESSION.to_string(),
            turn_id: turn_id(),
            parent_ids: vec![],
            messages: vec![crate::test_helpers::user_msg("hello")],
            agent_name: "coder".to_string(),
            profile_snapshot_hash: "h".to_string(),
            profile_snapshot_json: "{}".to_string(),
            model: "m".to_string(),
            provider: "p".to_string(),
            outcome: TurnOutcome::Interrupted {
                reason: super::super::outcome::TurnInterruption::ProcessInterruptedBeforeModelAttempt,
            },
            origin: ClosureOrigin::Reconciled,
            commit_message: "hello".to_string(),
            usage_aggregate: None,
            workspace_history_oid: None,
            created_at: "2026-08-16T00:00:00Z".to_string(),
        }
    }

    #[test]
    fn turn_closing_and_committed_facts_validates() {
        let closing = PendingStage::TurnClosing(TurnClosingFacts {
            turn_id: turn_id(),
            candidate: candidate(),
        });
        closing.validate().expect("valid closing");
        let committed = PendingStage::Committed(CommittedFacts {
            turn_id: turn_id(),
            candidate: candidate(),
        });
        committed.validate().expect("valid committed");
        assert!(validate_transition(&PendingStage::StepClosed {
            kind: super::super::step::StepClosedKind::InterruptTurn {
                reason: super::super::outcome::TurnInterruption::ProcessInterruptedBeforeModelAttempt,
            }
        }, &closing)
        .is_ok());
    }

    #[test]
    fn snapshot_serde_round_trip_preserves_stage() {
        let snapshot = PendingSnapshot {
            schema_version: PENDING_SNAPSHOT_SCHEMA_VERSION,
            session_id: session_id(),
            turn_id: turn_id(),
            predecessor: None,
            stage: turn_started(),
            recorded_at: None,
        };
        let value = serde_json::to_value(&snapshot).unwrap();
        assert!(value.get("recorded_at").is_none(), "recorded_at stays off the wire");
        let back: PendingSnapshot = serde_json::from_value(value).unwrap();
        assert_eq!(back, snapshot);
        back.validate().expect("round-tripped");
    }
}
