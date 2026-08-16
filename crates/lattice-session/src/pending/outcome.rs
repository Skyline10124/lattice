// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Typed Turn outcome and closure origin (ADR 0005 §15, GitHub #144).
//!
//! This is the journal's outcome authority. The legacy
//! `TurnCommitStatus`/`WorkspaceDisposition`/`terminal_reason` fields on the
//! conversation manifest are a projection for today's readers; #139-B deletes
//! them in the production hard cut. Free text is never logic authority here:
//! every failure, cancellation, and interruption reason is typed, and bounded
//! diagnostics live in the attempt records owned by `lattice-conversation`.

use lancet_protocol::InvocationId;
use lattice_conversation::{AttemptInterruption, TokenUsage, ToolCallValidation};
use serde::{Deserialize, Serialize};

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

/// The typed outcome of one accepted Turn (ADR 0005 §15). This replaces
/// `TurnCommitStatus + WorkspaceDisposition + terminal_reason` as the
/// journal authority; a projection onto the legacy conversation manifest
/// vocabulary exists only until the #139-B hard cut.
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

impl TurnOutcome {
    /// The legacy `TurnCommitStatus` projection used by today's
    /// conversation manifest. Diagnostic only; the typed value remains the
    /// authority. #139-B deletes this mapping together with the legacy
    /// fields.
    pub fn legacy_status(&self) -> crate::graph::TurnCommitStatus {
        use crate::graph::TurnCommitStatus;
        match self {
            Self::Completed { .. } => TurnCommitStatus::Completed,
            Self::Failed { .. } | Self::Interrupted { .. } => TurnCommitStatus::Error,
            Self::Cancelled { .. } => TurnCommitStatus::Cancelled,
        }
    }
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
            usage.prompt_tokens += attempt.prompt_tokens;
            usage.completion_tokens += attempt.completion_tokens;
            usage.total_tokens += attempt.total_tokens;
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
    fn legacy_status_projection_is_total_and_deterministic() {
        let cases = [
            (
                TurnOutcome::Completed {
                    finish_reason: TurnFinishReason::Stop,
                },
                crate::graph::TurnCommitStatus::Completed,
            ),
            (
                TurnOutcome::Failed {
                    reason: TurnFailure::ModelRequestFailed,
                },
                crate::graph::TurnCommitStatus::Error,
            ),
            (
                TurnOutcome::Interrupted {
                    reason: TurnInterruption::ProcessInterruptedBeforeModelAttempt,
                },
                crate::graph::TurnCommitStatus::Error,
            ),
            (
                TurnOutcome::Cancelled {
                    reason: TurnCancellation::UserCancelled,
                },
                crate::graph::TurnCommitStatus::Cancelled,
            ),
        ];
        for (outcome, status) in cases {
            assert_eq!(outcome.legacy_status(), status);
            // Serde round-trip keeps the typed value intact.
            let back: TurnOutcome = serde_json::from_value(serde_json::to_value(&outcome).unwrap())
                .unwrap();
            assert_eq!(back, outcome);
        }
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
    fn usage_aggregate_empty_is_none() {
        assert!(TurnUsageAggregate::derive(&[]).is_none());
    }
}
