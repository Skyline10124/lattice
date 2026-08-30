// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Typed Turn outcome and closure origin (ADR 0005 §15, GitHub #144).
//!
//! This is the sole durable and public Turn outcome authority. Free text is
//! never logic authority here:
//! every failure, cancellation, and interruption reason is typed, and bounded
//! diagnostics live in the attempt records owned by `lattice-conversation`.

use lancet_protocol::InvocationId;
use lattice_conversation::{AttemptInterruption, TokenUsage, ToolCallValidation};
use serde::{Deserialize, Serialize};

use super::binding::PlannedBinding;
use super::facts::{
    ExecutionOutcome, NotExecutedReason, ResultMaterial, UnavailableMaterialReason,
};
use crate::{Result, SessionError};
/// Whether a Turn outcome was decided by the live owner or produced by
/// reopen reconciliation (ADR 0005 §15).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClosureOrigin {
    /// The live Turn owner produced this outcome.
    Live,
    /// Reopen reconciliation closed the Turn; execution was never continued.
    Reconciled,
}

impl ClosureOrigin {
    /// Stable wire spelling.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Live => "live",
            Self::Reconciled => "reconciled",
        }
    }
}

/// Project-global WorkspaceHistory cut recorded by every committed Turn.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceHistoryCheckpoint {
    /// No LATTICE mutation has ever been recorded for this project.
    Empty,
    /// The current project-global WorkspaceHistory object identifier.
    Oid(String),
}

impl WorkspaceHistoryCheckpoint {
    pub fn as_oid(&self) -> Option<&str> {
        match self {
            Self::Empty => None,
            Self::Oid(oid) => Some(oid),
        }
    }

    /// Convert the authoritative `WorkspaceHistory::current()` result into a
    /// typed checkpoint. Empty is produced only for a genuinely absent ref;
    /// malformed or non-canonical object ids fail closed.
    pub fn from_current(current: Option<String>) -> crate::Result<Self> {
        let checkpoint = current.map(Self::Oid).unwrap_or(Self::Empty);
        checkpoint.validate()?;
        Ok(checkpoint)
    }

    /// Validate a persisted checkpoint OID as canonical lowercase SHA-1 hex.
    pub fn validate(&self) -> crate::Result<()> {
        let Self::Oid(oid) = self else {
            return Ok(());
        };
        if oid.len() != 40
            || !oid
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(crate::SessionError::JournalCorruption {
                detail:
                    "workspace-history checkpoint OID must be exactly 40 lowercase hex characters"
                        .into(),
            });
        }
        let parsed = gix::ObjectId::from_hex(oid.as_bytes()).map_err(|error| {
            crate::SessionError::JournalCorruption {
                detail: format!("workspace-history checkpoint OID is invalid: {error}"),
            }
        })?;
        if parsed.to_string() != *oid {
            return Err(crate::SessionError::JournalCorruption {
                detail: "workspace-history checkpoint OID failed its canonical round-trip".into(),
            });
        }
        Ok(())
    }
}

/// Why a completed Turn finished (ADR 0005 §15 `Completed { finish_reason }`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnFinishReason {
    /// The model produced a complete message with no Tool calls.
    Stop,
}

impl TurnFinishReason {
    /// Stable wire spelling.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Stop => "stop",
        }
    }
}

/// Typed reason a Turn failed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnFailure {
    /// A provider request failed before any stream event and no retry
    /// remained (non-retryable classification, or the frozen retry budget
    /// was exhausted).
    ModelRequestFailed,
    /// The code-defined `MAX_MODEL_STEPS` limit was reached; already closed
    /// messages and ToolResults are preserved (ADR 0005 §10).
    ModelStepLimitExceeded,
    /// The provider finished but the complete output was invalid
    /// (`ModelAttemptRejected`); the durable typed rejection is carried
    /// verbatim.
    InvalidToolCall {
        /// Why the batch failed validation.
        validation: ToolCallValidation,
    },
}

/// Typed reason a Turn was cancelled (cancellation was the controlling
/// cause; already durable invocation facts and conversation closures
/// remain, ADR 0005 §13).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnCancellation {
    /// The user cancelled the Turn.
    UserCancelled,
}

/// Typed reason a Turn was interrupted. Every reopen closure maps to one
/// variant; reasons never carry free text (ADR 0005 §14).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnInterruption {
    /// The journal tip was `TurnStarted`: the process died before any model
    /// attempt was published.
    ProcessInterruptedBeforeModelAttempt,
    /// The journal tip was `ModelAttemptStarted`: the attempt never reached
    /// a terminal classification.
    ProcessInterruptedDuringModelAttempt,
    /// The journal tip was a retryable `ModelAttemptFailed`: the process
    /// died inside the retry window (before the retry attempt published
    /// anything durable).
    ProcessInterruptedDuringModelRetry,
    /// A recorded attempt interruption is the controlling cause; the durable
    /// typed reason is carried verbatim.
    ModelAttempt(AttemptInterruption),
    /// The journal tip was `BindingPlanned` or `Bound`: the model produced
    /// Tool calls and the process died inside the binding window before
    /// any dispatch the facts can prove.
    ProcessInterruptedDuringToolBinding,
    /// The journal tip was `StepClosed(ContinueModel)`: reopen never
    /// continues the provider, so the Turn closes interrupted.
    WhileAwaitingModelContinuation,
    /// The journal tip was inside the ordered step-closing loop: remaining
    /// bindings close `NotExecuted` and the Turn closes without continuing.
    ProcessInterruptedDuringStepClosing,
    /// A dispatched invocation has no terminal fact: execution may have
    /// occurred and is conservatively indeterminate (ADR 0005 §12).
    InvocationMayHaveOccurred {
        /// The affected invocation.
        invocation: InvocationId,
    },
}

/// Live-only context that durable invocation outcomes cannot recover.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LiveControllingCause {
    /// The live owner may continue to another model step.
    Continue,
    /// The live Turn deadline expired before the next dispatch.
    ModelAttemptTimeout,
}

/// One shared decision for whether a closed Tool batch may continue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolBatchControl {
    /// Only the live owner may continue provider execution.
    Continue,
    /// A durable cancellation controls the Turn outcome.
    Cancel(TurnCancellation),
    /// A durable indeterminate outcome, live deadline, or reopen gap interrupts the Turn.
    Interrupt(TurnInterruption),
}

/// Derive the Tool batch's controlling cause from exact ordered invocation outcomes.
///
/// Reopen passes `None` for `live_cause`, so durable outcomes that do not carry a
/// stronger cancellation or indeterminate cause close as
/// `ProcessInterruptedDuringStepClosing` rather than continuing the provider.
pub fn derive_tool_batch_control(
    bindings: &[PlannedBinding],
    outcomes: &[(ExecutionOutcome, ResultMaterial)],
    live_cause: Option<LiveControllingCause>,
) -> Result<ToolBatchControl> {
    if bindings.len() != outcomes.len() {
        return Err(SessionError::JournalCorruption {
            detail: format!(
                "Tool batch control has {} bindings but {} outcomes",
                bindings.len(),
                outcomes.len()
            ),
        });
    }

    let mut controlling = None;
    let mut saw_indeterminate = false;
    for (binding, (outcome, material)) in bindings.iter().zip(outcomes) {
        if matches!(outcome, ExecutionOutcome::NotExecuted { .. })
            && !matches!(
                material,
                ResultMaterial::Unavailable {
                    reason: UnavailableMaterialReason::NotExecuted,
                    digest: None,
                    byte_length: None,
                }
            )
        {
            return Err(SessionError::JournalCorruption {
                detail: format!(
                    "not-executed invocation {} has non-canonical material",
                    binding.invocation_id
                ),
            });
        }

        match outcome {
            ExecutionOutcome::Indeterminate { .. } => {
                saw_indeterminate = true;
                controlling.get_or_insert_with(|| {
                    ToolBatchControl::Interrupt(TurnInterruption::InvocationMayHaveOccurred {
                        invocation: binding.invocation_id,
                    })
                });
            }
            ExecutionOutcome::Cancelled
            | ExecutionOutcome::NotExecuted {
                reason: NotExecutedReason::CancelledBeforeDispatch,
            } => {
                controlling
                    .get_or_insert(ToolBatchControl::Cancel(TurnCancellation::UserCancelled));
            }
            ExecutionOutcome::NotExecuted {
                reason: NotExecutedReason::PrecedingInvocationIndeterminate,
            } if !saw_indeterminate => {
                return Err(SessionError::JournalCorruption {
                    detail: format!(
                        "invocation {} names a preceding indeterminate outcome that is absent",
                        binding.invocation_id
                    ),
                });
            }
            _ => {}
        }
    }

    if let Some(controlling) = controlling {
        return Ok(controlling);
    }
    Ok(match live_cause {
        Some(LiveControllingCause::Continue) => ToolBatchControl::Continue,
        Some(LiveControllingCause::ModelAttemptTimeout) => ToolBatchControl::Interrupt(
            TurnInterruption::ModelAttempt(AttemptInterruption::Timeout),
        ),
        None => ToolBatchControl::Interrupt(TurnInterruption::ProcessInterruptedDuringStepClosing),
    })
}
/// The typed outcome of one accepted Turn (ADR 0005 §15).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnOutcome {
    /// The Turn completed with a final model message.
    Completed {
        /// Why the model stopped.
        finish_reason: TurnFinishReason,
    },
    /// The Turn failed.
    Failed {
        /// Typed failure reason.
        reason: TurnFailure,
    },
    /// The Turn was cancelled; cancellation was the controlling cause.
    Cancelled {
        /// Typed cancellation reason.
        reason: TurnCancellation,
    },
    /// The Turn was interrupted (process failure, partial stream, limit,
    /// or reopen reconciliation).
    Interrupted {
        /// Typed interruption reason.
        reason: TurnInterruption,
    },
}

/// Derived per-Turn usage aggregate (ADR 0005 §10): the attempt facts remain
/// the source of truth; `StepClosed` has no independent cumulative usage
/// authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnUsageAggregate {
    /// Every recorded attempt reported usage.
    Complete {
        /// Sum of all per-attempt usages.
        usage: TokenUsage,
    },
    /// At least one attempt reported unknown usage.
    Partial {
        /// Sum of the known per-attempt usages.
        usage: TokenUsage,
    },
}

impl TurnUsageAggregate {
    /// Derive the aggregate from per-attempt usage facts. `None` per entry
    /// is unknown, never zero; an empty attempt list has nothing to report.
    pub fn derive(per_attempt: &[Option<TokenUsage>]) -> Option<Self> {
        if per_attempt.is_empty() {
            return None;
        }
        let complete = per_attempt.iter().all(Option::is_some);
        let mut usage = TokenUsage {
            prompt_tokens: 0,
            completion_tokens: 0,
            total_tokens: 0,
        };
        for attempt in per_attempt.iter().flatten() {
            usage = usage.saturating_add(*attempt);
        }
        Some(if complete {
            Self::Complete { usage }
        } else {
            Self::Partial { usage }
        })
    }

    /// The summed usage regardless of completeness.
    pub fn usage(&self) -> TokenUsage {
        match self {
            Self::Complete { usage } | Self::Partial { usage } => *usage,
        }
    }
}

/// Re-exported attempt vocabulary re-used by interruption and rejection
/// reasons so downstream code needs no direct conversation-crate import for
/// these journal payloads.
pub use lattice_conversation::AttemptOutcome;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closure_origin_wire_spellings() {
        assert_eq!(ClosureOrigin::Live.as_str(), "live");
        assert_eq!(ClosureOrigin::Reconciled.as_str(), "reconciled");
        let json = serde_json::to_string(&ClosureOrigin::Reconciled).unwrap();
        assert_eq!(json, "\"reconciled\"");
    }

    #[test]
    fn usage_aggregate_complete_when_all_attempts_reported() {
        let aggregate = TurnUsageAggregate::derive(&[
            Some(TokenUsage {
                prompt_tokens: 10,
                completion_tokens: 5,
                total_tokens: 15,
            }),
            Some(TokenUsage {
                prompt_tokens: 1,
                completion_tokens: 1,
                total_tokens: 2,
            }),
        ])
        .expect("non-empty");
        assert_eq!(
            aggregate,
            TurnUsageAggregate::Complete {
                usage: TokenUsage {
                    prompt_tokens: 11,
                    completion_tokens: 6,
                    total_tokens: 17
                }
            }
        );
    }

    #[test]
    fn usage_aggregate_partial_when_any_attempt_unknown() {
        let aggregate = TurnUsageAggregate::derive(&[
            Some(TokenUsage {
                prompt_tokens: 10,
                completion_tokens: 5,
                total_tokens: 15,
            }),
            None,
        ])
        .expect("non-empty");
        assert!(matches!(aggregate, TurnUsageAggregate::Partial { .. }));
        // Missing usage stays unknown: the known attempt sums alone.
        assert_eq!(aggregate.usage().total_tokens, 15);
    }

    #[test]
    fn usage_aggregate_saturates_each_counter() {
        let aggregate = TurnUsageAggregate::derive(&[
            Some(TokenUsage {
                prompt_tokens: u64::MAX,
                completion_tokens: u64::MAX - 1,
                total_tokens: u64::MAX - 2,
            }),
            Some(TokenUsage {
                prompt_tokens: 1,
                completion_tokens: 2,
                total_tokens: 3,
            }),
        ])
        .expect("non-empty");
        assert_eq!(
            aggregate.usage(),
            TokenUsage {
                prompt_tokens: u64::MAX,
                completion_tokens: u64::MAX,
                total_tokens: u64::MAX,
            }
        );
    }

    #[test]
    fn usage_aggregate_empty_is_none() {
        assert!(TurnUsageAggregate::derive(&[]).is_none());
    }

    #[test]
    fn workspace_checkpoint_accepts_only_canonical_history_oids() {
        assert_eq!(
            WorkspaceHistoryCheckpoint::from_current(None).unwrap(),
            WorkspaceHistoryCheckpoint::Empty
        );
        let oid = "0123456789abcdef0123456789abcdef01234567".to_string();
        assert_eq!(
            WorkspaceHistoryCheckpoint::from_current(Some(oid.clone())).unwrap(),
            WorkspaceHistoryCheckpoint::Oid(oid)
        );
        for malformed in [
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            "0123456789ABCDEF0123456789ABCDEF01234567",
            "g123456789abcdef0123456789abcdef01234567",
            "0123456789abcdef0123456789abcdef0123456",
        ] {
            assert!(
                WorkspaceHistoryCheckpoint::from_current(Some(malformed.into())).is_err(),
                "accepted malformed checkpoint {malformed}"
            );
        }
    }
}
