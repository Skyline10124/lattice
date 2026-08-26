// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Versioned PendingTurn snapshots and pure edge validation (ADR 0005 §9).

use lancet_protocol::{ModelAttemptKey, SessionId, TurnId};
use lattice_conversation::{
    AttemptInterruption, AttemptRejection, BoundToolCall, MessageRole, NativeMessage, TokenUsage,
    UnboundToolCall,
};
use serde::{Deserialize, Serialize};

use super::binding::{BindingPlan, PlannedBinding};
use super::config::{MAX_MODEL_ATTEMPTS, MAX_MODEL_STEPS, TurnStartedFacts};
use super::outcome::{ClosureOrigin, TurnOutcome, TurnUsageAggregate};
use super::step::{StepClosedKind, StepClosingState, closure_transcript, step_conversation_digest};
use crate::{Result, SessionError};

/// Schema version of the `pending.json` payload.
pub const PENDING_SNAPSHOT_SCHEMA_VERSION: u32 = 3;

/// The twelve durable states of one pending Turn.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "stage")]
pub enum PendingStage {
    /// The durable request-acceptance boundary.
    TurnStarted(TurnStartedFacts),
    /// An outbound provider request was about to start.
    ModelAttemptStarted {
        /// Which attempt of which step of which Turn.
        key: ModelAttemptKey,
    },
    /// A failure before any stream event.
    ModelAttemptFailed {
        /// Which attempt failed.
        key: ModelAttemptKey,
        /// The adapter's pre-stream retryability classification.
        retryable: bool,
        /// Provider-reported usage, when supplied.
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
    /// A valid complete zero-Tool message.
    MessageComplete {
        /// Which attempt completed.
        key: ModelAttemptKey,
        /// The complete valid message.
        message: NativeMessage<UnboundToolCall>,
        /// Usage of the completing attempt, when reported.
        usage: Option<TokenUsage>,
    },
    /// The exact ordered binding plan and completing-attempt usage.
    BindingPlanned {
        /// The plan.
        plan: BindingPlan,
        /// Usage of the completing attempt, when reported.
        usage: Option<TokenUsage>,
    },
    /// The entire exact plan is durably reserved.
    Bound {
        /// The plan, byte-equal to `BindingPlanned`.
        plan: BindingPlan,
    },
    /// The ordered closing loop over the plan's bindings.
    StepClosing(StepClosingState),
    /// Every binding of the step is closed.
    StepClosed {
        /// The closed sum kind.
        kind: StepClosedKind,
    },
    /// The exact final TurnCommit candidate is pinned.
    TurnClosing(TurnClosingFacts),
    /// The immutable final journal object.
    Committed(CommittedFacts),
}

impl PendingStage {
    /// Stable wire name of this stage.
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

    /// Validate this stage's local payload invariants.
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
                        detail: "MessageComplete must be zero-Tool; a Tool-bearing valid message is BindingPlanned".into(),
                    });
                }
                Ok(())
            }
            Self::BindingPlanned { plan, .. } | Self::Bound { plan } => plan.validate(),
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
    if key.attempt_ordinal >= MAX_MODEL_ATTEMPTS {
        return Err(SessionError::JournalCorruption {
            detail: format!(
                "model attempt ordinal {} reaches the code-defined limit {MAX_MODEL_ATTEMPTS}",
                key.attempt_ordinal
            ),
        });
    }
    Ok(())
}

fn validate_binding_sessions<'a>(
    bindings: impl IntoIterator<Item = &'a PlannedBinding>,
    session_id: &SessionId,
) -> Result<()> {
    for binding in bindings {
        if binding.address.message.session_id != *session_id {
            return Err(SessionError::JournalCorruption {
                detail: format!(
                    "binding {} addresses session {} in the journal of {session_id}",
                    binding.invocation_id, binding.address.message.session_id
                ),
            });
        }
    }
    Ok(())
}

fn validate_candidate_session(
    candidate: &TurnCommitCandidate,
    session_id: &SessionId,
) -> Result<()> {
    if candidate.session_id != session_id.as_str() {
        return Err(SessionError::JournalCorruption {
            detail: format!(
                "pinned candidate names session '{}' in the journal of {session_id}",
                candidate.session_id
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
                    detail: "FinishTurn final message must be zero-Tool".into(),
                });
            }
            if final_message
                .parts
                .iter()
                .any(|part| matches!(part.semantic, MessageSemantic::ToolResult { .. }))
            {
                return Err(SessionError::JournalCorruption {
                    detail: "FinishTurn final message must not carry tool results".into(),
                });
            }
            Ok(())
        }
        StepClosedKind::ContinueModel {
            model_step_ordinal,
            ordered_tool_results,
            ..
        } => {
            if *model_step_ordinal >= MAX_MODEL_STEPS {
                return Err(SessionError::JournalCorruption {
                    detail: format!(
                        "closed model step {model_step_ordinal} reaches the code-defined limit {MAX_MODEL_STEPS}"
                    ),
                });
            }
            if ordered_tool_results.is_empty() {
                return Err(SessionError::JournalCorruption {
                    detail: "ContinueModel closes a binding plan and must carry its closures"
                        .into(),
                });
            }
            for closure in ordered_tool_results {
                closure.binding.validate()?;
                if closure.binding.address.message.model_step_ordinal != *model_step_ordinal {
                    return Err(SessionError::JournalCorruption {
                        detail: format!(
                            "ContinueModel names step {model_step_ordinal} but closes a binding of step {}",
                            closure.binding.address.message.model_step_ordinal
                        ),
                    });
                }
            }
            Ok(())
        }
        StepClosedKind::FailTurn { .. }
        | StepClosedKind::CancelTurn { .. }
        | StepClosedKind::InterruptTurn { .. } => Ok(()),
    }
}

/// Every immutable TurnCommit input except the workspace-history checkpoint.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TurnCommitCandidateBasis {
    /// Owning session.
    pub session_id: String,
    /// Turn identity.
    pub turn_id: TurnId,
    /// Logical conversation side parents.
    pub side_parent_ids: Vec<String>,
    /// Exact Turn transcript.
    pub messages: Vec<NativeMessage<BoundToolCall>>,
    /// Agent projection.
    pub agent_name: String,
    /// Frozen profile digest.
    pub profile_snapshot_hash: String,
    /// Frozen profile JSON.
    pub profile_snapshot_json: String,
    /// Frozen model.
    pub model: String,
    /// Frozen provider.
    pub provider: String,
    /// Typed Turn outcome.
    pub outcome: TurnOutcome,
    /// Live or reconciled closure authority.
    pub origin: ClosureOrigin,
    /// Commit message.
    pub commit_message: String,
    /// Derived usage aggregate.
    pub usage_aggregate: Option<TurnUsageAggregate>,
    /// Pinned RFC 3339 commit time.
    pub created_at: String,
}

impl TurnCommitCandidateBasis {
    /// Add the authoritative workspace-history checkpoint.
    pub fn with_workspace_history_checkpoint(
        self,
        workspace_history_checkpoint: super::outcome::WorkspaceHistoryCheckpoint,
    ) -> Result<TurnCommitCandidate> {
        workspace_history_checkpoint.validate()?;
        let candidate = TurnCommitCandidate {
            session_id: self.session_id,
            turn_id: self.turn_id,
            side_parent_ids: self.side_parent_ids,
            messages: self.messages,
            agent_name: self.agent_name,
            profile_snapshot_hash: self.profile_snapshot_hash,
            profile_snapshot_json: self.profile_snapshot_json,
            model: self.model,
            provider: self.provider,
            outcome: self.outcome,
            origin: self.origin,
            commit_message: self.commit_message,
            usage_aggregate: self.usage_aggregate,
            workspace_history_checkpoint,
            created_at: self.created_at,
        };
        candidate.validate()?;
        Ok(candidate)
    }
}

/// Exact pinned TurnCommit candidate.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TurnCommitCandidate {
    /// Owning session.
    pub session_id: String,
    /// Turn identity.
    pub turn_id: TurnId,
    /// Logical conversation side parents.
    pub side_parent_ids: Vec<String>,
    /// Exact Turn transcript.
    pub messages: Vec<NativeMessage<BoundToolCall>>,
    /// Agent projection.
    pub agent_name: String,
    /// Frozen profile digest.
    pub profile_snapshot_hash: String,
    /// Frozen profile JSON.
    pub profile_snapshot_json: String,
    /// Frozen model.
    pub model: String,
    /// Frozen provider.
    pub provider: String,
    /// Typed Turn outcome.
    pub outcome: TurnOutcome,
    /// Live or reconciled closure authority.
    pub origin: ClosureOrigin,
    /// Commit message.
    pub commit_message: String,
    /// Derived usage aggregate.
    pub usage_aggregate: Option<TurnUsageAggregate>,
    /// Project-global workspace-history checkpoint.
    pub workspace_history_checkpoint: super::outcome::WorkspaceHistoryCheckpoint,
    /// Pinned RFC 3339 commit time.
    pub created_at: String,
}

impl TurnCommitCandidate {
    fn validate(&self) -> Result<()> {
        if self.session_id.is_empty() || self.agent_name.is_empty() {
            return Err(SessionError::JournalCorruption {
                detail: "candidate session/agent must not be empty".into(),
            });
        }
        if self.messages.is_empty() {
            return Err(SessionError::JournalCorruption {
                detail: "candidate must carry the user message".into(),
            });
        }
        chrono::DateTime::parse_from_rfc3339(&self.created_at).map_err(|error| {
            SessionError::JournalCorruption {
                detail: format!("candidate created_at is not RFC 3339: {error}"),
            }
        })?;
        self.workspace_history_checkpoint.validate()
    }

    fn basis(&self) -> TurnCommitCandidateBasis {
        TurnCommitCandidateBasis {
            session_id: self.session_id.clone(),
            turn_id: self.turn_id,
            side_parent_ids: self.side_parent_ids.clone(),
            messages: self.messages.clone(),
            agent_name: self.agent_name.clone(),
            profile_snapshot_hash: self.profile_snapshot_hash.clone(),
            profile_snapshot_json: self.profile_snapshot_json.clone(),
            model: self.model.clone(),
            provider: self.provider.clone(),
            outcome: self.outcome.clone(),
            origin: self.origin,
            commit_message: self.commit_message.clone(),
            usage_aggregate: self.usage_aggregate,
            created_at: self.created_at.clone(),
        }
    }
}

/// Facts pinned by `TurnClosing`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TurnClosingFacts {
    /// Turn being closed.
    pub turn_id: TurnId,
    /// Exact pinned candidate.
    pub candidate: TurnCommitCandidate,
}

impl TurnClosingFacts {
    fn validate(&self) -> Result<()> {
        if self.candidate.turn_id != self.turn_id {
            return Err(SessionError::JournalCorruption {
                detail: "TurnClosing candidate names a different Turn".into(),
            });
        }
        self.candidate.validate()
    }
}

/// Payload of the final `Committed` journal object.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CommittedFacts {
    /// Turn being committed.
    pub turn_id: TurnId,
    /// Exact candidate used to construct the TurnCommit.
    pub candidate: TurnCommitCandidate,
}

impl CommittedFacts {
    fn validate(&self) -> Result<()> {
        if self.candidate.turn_id != self.turn_id {
            return Err(SessionError::JournalCorruption {
                detail: "Committed candidate names a different Turn".into(),
            });
        }
        self.candidate.validate()
    }
}

/// One immutable PendingTurn journal snapshot.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PendingSnapshot {
    /// Payload schema version.
    pub schema_version: u32,
    /// Owning session.
    pub session_id: SessionId,
    /// Turn identity.
    pub turn_id: TurnId,
    /// Predecessor snapshot object id.
    pub predecessor: Option<String>,
    /// Sole cumulative post-user transcript authority.
    pub post_user_transcript: Vec<NativeMessage<BoundToolCall>>,
    /// Durable state.
    pub stage: PendingStage,
    /// Committer time, populated by the store on read.
    #[serde(skip)]
    pub recorded_at: Option<i64>,
    /// Chain-root facts, populated by the store on read.
    #[serde(skip)]
    pub root: Option<TurnStartedFacts>,
    /// Per-attempt usage derived from the validated lineage.
    #[serde(skip)]
    pub attempt_usages: Vec<Option<TokenUsage>>,
}

impl PendingSnapshot {
    /// Validate envelope and stage-local invariants.
    pub fn validate(&self) -> Result<()> {
        if self.schema_version != PENDING_SNAPSHOT_SCHEMA_VERSION {
            return Err(SessionError::JournalCorruption {
                detail: format!(
                    "pending snapshot schema version {} is not {}",
                    self.schema_version, PENDING_SNAPSHOT_SCHEMA_VERSION
                ),
            });
        }
        if let Some(root) = &self.root
            && (root.turn_id != self.turn_id || root.session_id != self.session_id)
        {
            return Err(SessionError::JournalCorruption {
                detail: "chain root facts do not match the snapshot identity".into(),
            });
        }
        validate_post_user_transcript(&self.post_user_transcript)?;
        self.stage.validate()?;
        match &self.stage {
            PendingStage::TurnStarted(facts) => {
                if !self.post_user_transcript.is_empty() {
                    return Err(SessionError::JournalCorruption {
                        detail: "TurnStarted must have an empty post-user transcript".into(),
                    });
                }
                if self.predecessor.is_some() {
                    return Err(SessionError::JournalCorruption {
                        detail: "TurnStarted must be the first journal snapshot".into(),
                    });
                }
                if facts.turn_id != self.turn_id || facts.session_id != self.session_id {
                    return Err(SessionError::JournalCorruption {
                        detail: "TurnStarted facts do not match the snapshot identity".into(),
                    });
                }
            }
            PendingStage::ModelAttemptStarted { key }
            | PendingStage::ModelAttemptFailed { key, .. }
            | PendingStage::ModelAttemptInterrupted { key, .. }
            | PendingStage::ModelAttemptRejected { key, .. } => self.validate_attempt(key)?,
            PendingStage::MessageComplete { key, message, .. } => {
                self.validate_attempt(key)?;
                let expected = message.clone().bind_zero_tool().map_err(|_| {
                    SessionError::JournalCorruption {
                        detail: "MessageComplete contains a ToolCall".into(),
                    }
                })?;
                if self.post_user_transcript.last() != Some(&expected) {
                    return Err(SessionError::JournalCorruption {
                        detail: "MessageComplete transcript does not end in its message".into(),
                    });
                }
            }
            PendingStage::BindingPlanned { plan, .. } | PendingStage::Bound { plan } => {
                if self.post_user_transcript.last() != Some(&plan.bound_message()?) {
                    return Err(SessionError::JournalCorruption {
                        detail: "binding stage transcript does not end in its bound assistant"
                            .into(),
                    });
                }
                if plan.plan_key.turn_id != self.turn_id {
                    return Err(SessionError::JournalCorruption {
                        detail: "plan key does not match the snapshot Turn".into(),
                    });
                }
                validate_binding_sessions(&plan.bindings, &self.session_id)?;
            }
            PendingStage::StepClosing(state) => {
                state.validate_transcript(&self.post_user_transcript)?;
                if state.plan_key.turn_id != self.turn_id {
                    return Err(SessionError::JournalCorruption {
                        detail: "step closing plan does not match the snapshot Turn".into(),
                    });
                }
                validate_binding_sessions(&state.bindings, &self.session_id)?;
                if let Some(candidate) = &state.current_candidate
                    && candidate.message.session_id != self.session_id
                {
                    return Err(SessionError::JournalCorruption {
                        detail: format!(
                            "pinned draft candidate addresses session {} in the journal of {}",
                            candidate.message.session_id, self.session_id
                        ),
                    });
                }
            }
            PendingStage::StepClosed { kind } => {
                validate_step_closed_transcript(kind, &self.post_user_transcript)?;
                if let StepClosedKind::ContinueModel {
                    ordered_tool_results,
                    ..
                } = kind
                {
                    validate_binding_sessions(
                        ordered_tool_results.iter().map(|closure| &closure.binding),
                        &self.session_id,
                    )?;
                }
            }
            PendingStage::TurnClosing(facts) => {
                if facts.turn_id != self.turn_id {
                    return Err(SessionError::JournalCorruption {
                        detail: "TurnClosing facts do not match the snapshot Turn".into(),
                    });
                }
                validate_candidate_session(&facts.candidate, &self.session_id)?;
                self.validate_candidate_transcript(&facts.candidate)?;
            }
            PendingStage::Committed(facts) => {
                if facts.turn_id != self.turn_id {
                    return Err(SessionError::JournalCorruption {
                        detail: "Committed facts do not match the snapshot Turn".into(),
                    });
                }
                validate_candidate_session(&facts.candidate, &self.session_id)?;
                self.validate_candidate_transcript(&facts.candidate)?;
            }
        }
        Ok(())
    }

    fn validate_attempt(&self, key: &ModelAttemptKey) -> Result<()> {
        validate_attempt_key(key)?;
        if key.turn_id != self.turn_id {
            return Err(SessionError::JournalCorruption {
                detail: "attempt key does not match the snapshot Turn".into(),
            });
        }
        if let Some(root) = &self.root
            && key.attempt_ordinal >= root.config.limits.max_model_attempts
        {
            return Err(SessionError::JournalCorruption {
                detail: format!(
                    "model attempt ordinal {} reaches the frozen per-step limit {}",
                    key.attempt_ordinal, root.config.limits.max_model_attempts
                ),
            });
        }
        Ok(())
    }

    /// Exact full Turn transcript.
    pub fn turn_messages(&self) -> Result<Vec<NativeMessage<BoundToolCall>>> {
        let root = self.turn_started_facts()?;
        let mut messages = vec![root.user_message.clone().bind_zero_tool().map_err(|_| {
            SessionError::JournalCorruption {
                detail: "TurnStarted user message contains a ToolCall".into(),
            }
        })?];
        messages.extend(self.post_user_transcript.clone());
        Ok(messages)
    }

    /// Derive all journal-owned candidate fields from one authoritative seam.
    pub fn turn_commit_candidate_basis(
        &self,
        outcome: TurnOutcome,
        origin: ClosureOrigin,
    ) -> Result<TurnCommitCandidateBasis> {
        let root = self.turn_started_facts()?;
        let recorded_at = self
            .recorded_at
            .ok_or_else(|| SessionError::JournalCorruption {
                detail: "snapshot carries no journal committer time".into(),
            })?;
        let created_at = chrono::DateTime::from_timestamp(recorded_at, 0)
            .ok_or_else(|| SessionError::JournalCorruption {
                detail: format!("journal committer time {recorded_at} is not representable"),
            })?
            .to_rfc3339();
        let messages = self.turn_messages()?;
        Ok(TurnCommitCandidateBasis {
            session_id: self.session_id.to_string(),
            turn_id: self.turn_id,
            side_parent_ids: Vec::new(),
            commit_message: crate::graph::derive_commit_message(&messages),
            messages,
            agent_name: root.config.agent_name.clone(),
            profile_snapshot_hash: super::encode_hex(&root.config.digest()?),
            profile_snapshot_json: root.config.profile_snapshot_json.clone(),
            model: root.config.model.clone(),
            provider: root.config.provider.clone(),
            outcome,
            origin,
            usage_aggregate: TurnUsageAggregate::derive(&self.attempt_usages),
            created_at,
        })
    }

    fn turn_started_facts(&self) -> Result<&TurnStartedFacts> {
        match &self.stage {
            PendingStage::TurnStarted(facts) => Ok(facts),
            _ => self
                .root
                .as_ref()
                .ok_or_else(|| SessionError::JournalCorruption {
                    detail: "snapshot carries no chain-root TurnStarted facts".into(),
                }),
        }
    }

    fn validate_candidate_transcript(&self, candidate: &TurnCommitCandidate) -> Result<()> {
        if (self.root.is_some() || matches!(self.stage, PendingStage::TurnStarted(_)))
            && candidate.messages != self.turn_messages()?
        {
            return Err(SessionError::JournalCorruption {
                detail: "pinned candidate transcript differs from the journal authority".into(),
            });
        }
        Ok(())
    }
}

fn validate_post_user_transcript(messages: &[NativeMessage<BoundToolCall>]) -> Result<()> {
    for (index, message) in messages.iter().enumerate() {
        match message.role {
            MessageRole::Assistant => {
                if message.parts.iter().any(|part| {
                    matches!(
                        part.semantic,
                        lattice_conversation::MessageSemantic::ToolResult { .. }
                    )
                }) {
                    return Err(SessionError::JournalCorruption {
                        detail: format!("assistant transcript message {index} contains ToolResult"),
                    });
                }
            }
            MessageRole::Tool => {
                let [part] = message.parts.as_slice() else {
                    return Err(SessionError::JournalCorruption {
                        detail: format!(
                            "tool transcript message {index} is not one exact ToolResult"
                        ),
                    });
                };
                if message.provider_origin.is_some()
                    || !matches!(
                        part.semantic,
                        lattice_conversation::MessageSemantic::ToolResult { .. }
                    )
                {
                    return Err(SessionError::JournalCorruption {
                        detail: format!(
                            "tool transcript message {index} is not one exact ToolResult"
                        ),
                    });
                }
            }
            MessageRole::System | MessageRole::User => {
                return Err(SessionError::JournalCorruption {
                    detail: format!("post-user transcript contains root role at message {index}"),
                });
            }
        }
    }
    Ok(())
}

fn validate_step_closed_transcript(
    kind: &StepClosedKind,
    transcript: &[NativeMessage<BoundToolCall>],
) -> Result<()> {
    match kind {
        StepClosedKind::FinishTurn { final_message } => {
            let expected = final_message.clone().bind_zero_tool().map_err(|_| {
                SessionError::JournalCorruption {
                    detail: "FinishTurn message contains a ToolCall".into(),
                }
            })?;
            if transcript.last() == Some(&expected) {
                Ok(())
            } else {
                Err(SessionError::JournalCorruption {
                    detail: "FinishTurn transcript does not end in its final message".into(),
                })
            }
        }
        StepClosedKind::ContinueModel {
            model_step_ordinal,
            ordered_tool_results,
            conversation_digest,
        } => {
            let first =
                ordered_tool_results
                    .first()
                    .ok_or_else(|| SessionError::JournalCorruption {
                        detail: "ContinueModel carries no ToolResult closures".into(),
                    })?;
            let state = StepClosingState {
                plan_key: lancet_protocol::PlanKey {
                    turn_id: first.binding.address.message.turn_id,
                    model_step_ordinal: *model_step_ordinal,
                },
                bindings: ordered_tool_results
                    .iter()
                    .map(|closure| closure.binding.clone())
                    .collect(),
                closure_cursor: ordered_tool_results.len() as u32,
                closures: ordered_tool_results.clone(),
                current_candidate: None,
            };
            state.validate_transcript(transcript)?;
            let suffix_len = ordered_tool_results.len().checked_add(1).ok_or_else(|| {
                SessionError::JournalCorruption {
                    detail: "ContinueModel transcript suffix length overflowed".into(),
                }
            })?;
            let assistant_index = transcript.len().checked_sub(suffix_len).ok_or_else(|| {
                SessionError::JournalCorruption {
                    detail: "ContinueModel transcript omits its bound assistant".into(),
                }
            })?;
            let assistant =
                transcript
                    .get(assistant_index)
                    .ok_or_else(|| SessionError::JournalCorruption {
                        detail: "ContinueModel transcript omits its bound assistant".into(),
                    })?;
            if step_conversation_digest(assistant, ordered_tool_results) != *conversation_digest {
                return Err(SessionError::JournalCorruption {
                    detail: "ContinueModel conversation digest differs from its transcript".into(),
                });
            }
            Ok(())
        }
        StepClosedKind::FailTurn { .. }
        | StepClosedKind::CancelTurn { .. }
        | StepClosedKind::InterruptTurn { .. } => Ok(()),
    }
}

/// Derive the sole transcript bytes for a legal next stage.
pub(crate) fn next_post_user_transcript(
    prev: &PendingSnapshot,
    next: &PendingStage,
) -> Result<Vec<NativeMessage<BoundToolCall>>> {
    validate_transition(&prev.stage, next)?;
    let mut transcript = prev.post_user_transcript.clone();
    match next {
        PendingStage::MessageComplete { message, .. } => {
            transcript.push(message.clone().bind_zero_tool().map_err(|_| {
                SessionError::InvalidInput("MessageComplete contains a ToolCall".into())
            })?);
        }
        PendingStage::BindingPlanned { plan, .. } => transcript.push(plan.bound_message()?),
        PendingStage::StepClosed {
            kind: StepClosedKind::FinishTurn { final_message },
        } => {
            let message = final_message.clone().bind_zero_tool().map_err(|_| {
                SessionError::InvalidInput("FinishTurn message contains a ToolCall".into())
            })?;
            if transcript.last() != Some(&message) {
                transcript.push(message);
            }
        }
        PendingStage::StepClosing(next_state) => {
            if let PendingStage::StepClosing(prev_state) = &prev.stage
                && next_state.closure_cursor == prev_state.closure_cursor + 1
            {
                let closure = next_state.closures.last().ok_or_else(|| {
                    SessionError::InvalidInput("cursor advance omitted its closure".into())
                })?;
                let content = match &closure.material {
                    super::step::MaterialAvailability::Available { .. } => Some(
                        prev_state
                            .current_candidate
                            .as_ref()
                            .ok_or_else(|| {
                                SessionError::InvalidInput(
                                    "available closure has no pinned ResultDraft".into(),
                                )
                            })?
                            .canonical_tool_content
                            .clone(),
                    ),
                    super::step::MaterialAvailability::Unavailable { .. } => None,
                };
                transcript.push(closure_transcript(closure, content));
            }
        }
        _ => {}
    }
    Ok(transcript)
}

/// Map a closed step to its only legal Turn outcome.
pub(crate) fn step_closed_turn_outcome(kind: &StepClosedKind) -> TurnOutcome {
    match kind {
        StepClosedKind::FinishTurn { .. } => TurnOutcome::Completed {
            finish_reason: super::outcome::TurnFinishReason::Stop,
        },
        StepClosedKind::ContinueModel { .. } => TurnOutcome::Interrupted {
            reason: super::outcome::TurnInterruption::WhileAwaitingModelContinuation,
        },
        StepClosedKind::FailTurn { reason } => TurnOutcome::Failed {
            reason: reason.clone(),
        },
        StepClosedKind::CancelTurn { reason } => TurnOutcome::Cancelled { reason: *reason },
        StepClosedKind::InterruptTurn { reason } => TurnOutcome::Interrupted {
            reason: reason.clone(),
        },
    }
}

fn authenticate_snapshot_edge(prev: &PendingSnapshot, next: &PendingSnapshot) -> Result<()> {
    match (&prev.stage, &next.stage) {
        (
            PendingStage::StepClosing(state),
            PendingStage::StepClosed {
                kind:
                    StepClosedKind::ContinueModel {
                        ordered_tool_results,
                        conversation_digest,
                        ..
                    },
            },
        ) => {
            if ordered_tool_results != &state.closures {
                return Err(SessionError::JournalCorruption {
                    detail: "ContinueModel closures differ from the preceding StepClosing state"
                        .into(),
                });
            }
            let suffix_len = state.closures.len().checked_add(1).ok_or_else(|| {
                SessionError::JournalCorruption {
                    detail: "StepClosing transcript suffix length overflowed".into(),
                }
            })?;
            let assistant_index = prev
                .post_user_transcript
                .len()
                .checked_sub(suffix_len)
                .ok_or_else(|| SessionError::JournalCorruption {
                    detail: "StepClosing transcript omits its bound assistant".into(),
                })?;
            let assistant = prev
                .post_user_transcript
                .get(assistant_index)
                .ok_or_else(|| SessionError::JournalCorruption {
                    detail: "StepClosing transcript omits its bound assistant".into(),
                })?;
            if *conversation_digest != step_conversation_digest(assistant, &state.closures) {
                return Err(SessionError::JournalCorruption {
                    detail:
                        "ContinueModel digest is not derived from the preceding StepClosing state"
                            .into(),
                });
            }
        }
        (
            PendingStage::StepClosed { kind },
            PendingStage::TurnClosing(TurnClosingFacts { candidate, .. }),
        ) => {
            candidate.workspace_history_checkpoint.validate()?;
            let expected =
                prev.turn_commit_candidate_basis(step_closed_turn_outcome(kind), candidate.origin)?;
            if candidate.basis() != expected {
                return Err(SessionError::JournalCorruption {
                    detail: "TurnClosing candidate differs from the preceding journal authority"
                        .into(),
                });
            }
        }
        (PendingStage::TurnClosing(previous), PendingStage::Committed(committed))
            if previous.turn_id != committed.turn_id
                || previous.candidate != committed.candidate =>
        {
            return Err(SessionError::JournalCorruption {
                detail: "Committed must preserve the exact TurnClosing candidate".into(),
            });
        }
        _ => {}
    }
    Ok(())
}

/// Validate one complete persisted journal edge.
pub(crate) fn validate_snapshot_transition(
    prev: &PendingSnapshot,
    next: &PendingSnapshot,
) -> Result<()> {
    prev.validate()?;
    next.validate()?;
    if prev.session_id != next.session_id || prev.turn_id != next.turn_id {
        return Err(SessionError::JournalCorruption {
            detail: "journal edge changes SessionId or TurnId".into(),
        });
    }
    authenticate_snapshot_edge(prev, next)?;
    let expected = match (&prev.stage, &next.stage) {
        (PendingStage::TurnClosing(previous), PendingStage::Committed(committed))
            if previous.turn_id == committed.turn_id
                && previous.candidate == committed.candidate =>
        {
            prev.post_user_transcript.clone()
        }
        (
            PendingStage::BindingPlanned { plan: original, .. },
            PendingStage::BindingPlanned { plan: revised, .. },
        ) if is_candidate_revision_of(&prev.stage, &next.stage) => {
            let mut transcript = prev.post_user_transcript.clone();
            let last = transcript
                .last_mut()
                .ok_or_else(|| SessionError::JournalCorruption {
                    detail: "candidate revision has no bound assistant to replace".into(),
                })?;
            if *last != original.bound_message()? {
                return Err(SessionError::JournalCorruption {
                    detail: "candidate revision predecessor transcript differs from its plan"
                        .into(),
                });
            }
            *last = revised.bound_message()?;
            transcript
        }
        (PendingStage::BindingPlanned { .. }, PendingStage::BindingPlanned { .. }) => {
            return Err(SessionError::JournalCorruption {
                detail: "BindingPlanned revision changed more than candidate identities and derived bindings"
                    .into(),
            });
        }
        _ => next_post_user_transcript(prev, &next.stage).map_err(|error| {
            SessionError::JournalCorruption {
                detail: format!("illegal journal transition: {error}"),
            }
        })?,
    };
    if next.post_user_transcript != expected {
        return Err(SessionError::JournalCorruption {
            detail: "cumulative transcript is omitted, duplicated, forged, reordered, or prefix-divergent"
                .into(),
        });
    }
    Ok(())
}

/// Whether `revised` is the sole legal same-stage candidate revision.
pub(crate) fn is_candidate_revision_of(original: &PendingStage, revised: &PendingStage) -> bool {
    let (
        PendingStage::BindingPlanned {
            plan: original,
            usage: original_usage,
        },
        PendingStage::BindingPlanned {
            plan: revised,
            usage: revised_usage,
        },
    ) = (original, revised)
    else {
        return false;
    };
    if original_usage != revised_usage {
        return false;
    }
    use std::collections::BTreeSet;
    if original.plan_key != revised.plan_key
        || original.message != revised.message
        || original.bindings.len() != revised.bindings.len()
    {
        return false;
    }
    let original_ids: BTreeSet<_> = original
        .bindings
        .iter()
        .map(|binding| binding.invocation_id)
        .collect();
    let mut revised_ids = BTreeSet::new();
    original
        .bindings
        .iter()
        .zip(&revised.bindings)
        .all(|(before, after)| {
            !original_ids.contains(&after.invocation_id)
                && before.address == after.address
                && before.method == after.method
                && before.descriptor_identity == after.descriptor_identity
                && before.arguments_digest == after.arguments_digest
                && after.binding_digest
                    == PlannedBinding::compute_binding_digest(
                        &after.invocation_id,
                        &after.address,
                        &after.method,
                        &after.descriptor_identity,
                        &after.arguments_digest,
                    )
                && revised_ids.insert(after.invocation_id)
        })
}

/// Pure structural stage-transition validation.
pub fn validate_transition(prev: &PendingStage, next: &PendingStage) -> Result<()> {
    use PendingStage::*;
    let mismatch = |detail: String| Err(SessionError::InvalidInput(detail));
    let same_attempt = |started: &ModelAttemptKey, done: &ModelAttemptKey| -> Result<()> {
        if started == done {
            Ok(())
        } else {
            mismatch(format!(
                "attempt outcome {done:?} does not follow started attempt {started:?}"
            ))
        }
    };
    match (prev, next) {
        (
            ModelAttemptFailed {
                key: failed,
                retryable: true,
                ..
            },
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
        (
            StepClosed {
                kind:
                    StepClosedKind::ContinueModel {
                        model_step_ordinal, ..
                    },
                ..
            },
            ModelAttemptStarted { key },
        ) => {
            validate_attempt_key(key)?;
            if key.model_step_ordinal == model_step_ordinal + 1 && key.attempt_ordinal == 0 {
                Ok(())
            } else {
                mismatch(format!(
                    "the step after closing step {model_step_ordinal} must be step {} attempt 0, got {key:?}",
                    model_step_ordinal + 1
                ))
            }
        }
        (TurnStarted(_), ModelAttemptStarted { key }) => {
            if key.model_step_ordinal == 0 && key.attempt_ordinal == 0 {
                Ok(())
            } else {
                mismatch(format!(
                    "the first model step must be step 0 attempt 0, got {key:?}"
                ))
            }
        }
        (ModelAttemptStarted { key: started }, ModelAttemptFailed { key: done, .. })
        | (ModelAttemptStarted { key: started }, ModelAttemptInterrupted { key: done, .. })
        | (ModelAttemptStarted { key: started }, ModelAttemptRejected { key: done, .. })
        | (ModelAttemptStarted { key: started }, MessageComplete { key: done, .. }) => {
            same_attempt(started, done)
        }
        (ModelAttemptStarted { key: started }, BindingPlanned { plan, .. }) => {
            if plan.plan_key.turn_id == started.turn_id
                && plan.plan_key.model_step_ordinal == started.model_step_ordinal
            {
                Ok(())
            } else {
                mismatch("BindingPlanned step does not match the completing attempt".into())
            }
        }
        (BindingPlanned { plan: planned, .. }, Bound { plan: bound }) => {
            if planned == bound {
                Ok(())
            } else {
                mismatch("Bound must carry the byte-equal exact planned plan".into())
            }
        }
        (Bound { plan }, StepClosing(state)) => {
            if state.closure_cursor == 0
                && state.closures.is_empty()
                && state.current_candidate.is_none()
                && state.plan_key == plan.plan_key
                && state.bindings == plan.bindings
            {
                Ok(())
            } else {
                mismatch("StepClosing must start at cursor 0 over the bound plan".into())
            }
        }
        (StepClosing(prev_state), StepClosing(next_state)) => {
            if prev_state.plan_key != next_state.plan_key
                || prev_state.bindings != next_state.bindings
            {
                return mismatch("StepClosing must keep the same plan and bindings".into());
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
                            _ => mismatch("candidate must pin the lowest unclosed binding".into()),
                        }
                    } else {
                        mismatch("pinning a candidate must not move the cursor".into())
                    }
                }
                (Some(candidate), None) => {
                    if next_state.closure_cursor != prev_state.closure_cursor + 1
                        || next_state.closures.len() != prev_state.closures.len() + 1
                        || next_state.closures[..prev_state.closures.len()]
                            != prev_state.closures[..]
                    {
                        return mismatch(
                            "only the lowest unclosed ordinal may advance, by exactly one".into(),
                        );
                    }
                    let closure = next_state.closures.last().expect("extended by one");
                    if let super::step::MaterialAvailability::Available {
                        digest,
                        byte_length,
                    } = &closure.material
                        && (*digest != candidate.content_digest
                            || *byte_length != candidate.byte_length)
                    {
                        return mismatch(
                            "closure material does not match the pinned candidate".into(),
                        );
                    }
                    Ok(())
                }
                (None, None) => {
                    if next_state.closure_cursor == prev_state.closure_cursor + 1
                        && next_state.closures.len() == prev_state.closures.len() + 1
                        && next_state.closures[..prev_state.closures.len()]
                            == prev_state.closures[..]
                    {
                        let closure = next_state.closures.last().expect("extended by one");
                        match (&closure.outcome, &closure.material) {
                            (
                                lattice_conversation::ToolResultOutcome::NotExecuted,
                                super::step::MaterialAvailability::Unavailable {
                                    reason: super::facts::UnavailableMaterialReason::NotExecuted,
                                    digest: None,
                                    byte_length: None,
                                },
                            )
                            | (
                                lattice_conversation::ToolResultOutcome::Indeterminate,
                                super::step::MaterialAvailability::Unavailable {
                                    reason: super::facts::UnavailableMaterialReason::DraftPublicationFailed,
                                    digest: None,
                                    byte_length: None,
                                },
                            ) => Ok(()),
                            _ => mismatch(
                                "closing without a pinned candidate requires exact conservative material"
                                    .into(),
                            ),
                        }
                    } else {
                        mismatch(
                            "only the lowest unclosed ordinal may advance, by exactly one".into(),
                        )
                    }
                }
                (Some(_), Some(_)) => mismatch(
                    "cursor advance must publish the pinned candidate's closure and clear it".into(),
                ),
            }
        }
        (StepClosing(state), StepClosed { kind }) => {
            if !state.all_closed() {
                return mismatch("StepClosed requires every binding closed".into());
            }
            if let StepClosedKind::ContinueModel {
                model_step_ordinal, ..
            } = kind
                && *model_step_ordinal != state.plan_key.model_step_ordinal
            {
                return mismatch(format!(
                    "ContinueModel names step {model_step_ordinal} but closes plan step {}",
                    state.plan_key.model_step_ordinal
                ));
            }
            Ok(())
        }
        (
            MessageComplete { message, .. },
            StepClosed {
                kind: StepClosedKind::FinishTurn { final_message },
            },
        ) => {
            if message == final_message {
                Ok(())
            } else {
                mismatch("FinishTurn message differs from MessageComplete".into())
            }
        }
        (
            TurnStarted(_)
            | ModelAttemptStarted { .. }
            | ModelAttemptFailed { .. }
            | ModelAttemptInterrupted { .. }
            | ModelAttemptRejected { .. }
            | BindingPlanned { .. }
            | Bound { .. },
            StepClosed { .. },
        ) => Ok(()),
        (StepClosed { .. }, TurnClosing(_)) => Ok(()),
        (_, Committed(_)) => mismatch(
            "Committed is never published through active-turn; the final ref transaction is its only publication path"
                .into(),
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
    include!("contract_tests.rs");
}
