// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Per-attempt records: usage, interruption, and rejection (ADR 0005 §7/§10).
//!
//! Every provider attempt of a model step is recorded separately — a retry
//! after a pre-stream failure gets its own `attempt_ordinal` and its own
//! record. Usage is the provider-reported `Option<TokenUsage>`; missing usage
//! is unknown, never zero, and failed/interrupted attempts retain usage when
//! the provider supplied it.

use lancet_protocol::ModelAttemptKey;
use serde::{Deserialize, Serialize};

use crate::limits::LimitKind;
use crate::message::ProviderOrigin;
use crate::tool_call::ToolCallValidation;
use crate::usage::TokenUsage;

/// A completed provider output rejected before binding.
///
/// A rejected attempt retains its bounded assembled output and diagnostics
/// for Session diagnosis but produces no `MessageComplete`, `BindingPlanned`,
/// `InvocationId`, or transcript content.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttemptRejection {
    /// The provider finished (`Done`) but at least one ToolCall in the
    /// complete batch is invalid.
    InvalidToolCall {
        /// Why the batch failed validation.
        validation: ToolCallValidation,
    },
}

/// Why a provider attempt was interrupted before a complete valid message.
///
/// Partial reasoning or assistant output from an interrupted attempt is
/// diagnostic state, not committed conversation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttemptInterruption {
    /// A stream error after the first stream event (never retried).
    StreamError,
    /// The provider request or stream exceeded its timeout.
    Timeout,
    /// The caller cancelled the attempt.
    Cancelled,
    /// A protocol hard limit was crossed during assembly.
    OutputLimitExceeded {
        /// Which limit was crossed.
        kind: LimitKind,
        /// The limit in bytes.
        limit: usize,
        /// The byte count that would have resulted.
        actual: usize,
    },
    /// The process died mid-attempt (recorded by reopen reconciliation).
    ProcessInterrupted,
}

/// The terminal classification of one provider attempt.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttemptOutcome {
    /// The attempt produced a complete valid message (zero-Tool) or a valid
    /// Tool-bearing message that entered binding.
    Complete,
    /// The provider finished but the complete output is invalid.
    Rejected(AttemptRejection),
    /// The attempt was interrupted before a complete valid message.
    Interrupted(AttemptInterruption),
}

/// One provider attempt of one model step: key, origin, usage, outcome.
///
/// Retry attempts are recorded separately under their own
/// `attempt_ordinal`; there is no in-place update of a prior record.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelAttemptRecord {
    /// Which attempt of which step of which Turn.
    pub key: ModelAttemptKey,
    /// Provider wire family the attempt ran against.
    pub origin: ProviderOrigin,
    /// Provider-reported usage; `None` is unknown, never zero.
    pub usage: Option<TokenUsage>,
    /// Terminal classification.
    pub outcome: AttemptOutcome,
}

#[cfg(test)]
mod tests {
    use super::*;
    use lancet_protocol::TurnId;

    fn key(attempt_ordinal: u32) -> ModelAttemptKey {
        ModelAttemptKey {
            turn_id: TurnId::parse("3f8b2c1a-7d4e-4f5a-8b9c-0d1e2f3a4b5c").expect("turn id"),
            model_step_ordinal: 0,
            attempt_ordinal,
        }
    }

    #[test]
    fn retry_attempts_are_separate_records() {
        let first = ModelAttemptRecord {
            key: key(0),
            origin: ProviderOrigin::Anthropic,
            usage: None,
            outcome: AttemptOutcome::Interrupted(AttemptInterruption::StreamError),
        };
        let second = ModelAttemptRecord {
            key: key(1),
            origin: ProviderOrigin::Anthropic,
            usage: Some(TokenUsage {
                prompt_tokens: 3,
                completion_tokens: 0,
                total_tokens: 3,
            }),
            outcome: AttemptOutcome::Complete,
        };
        assert_ne!(first.key, second.key);
        // Missing usage on the first attempt stays unknown; it is not folded
        // into zero or into the retry's usage.
        assert_eq!(first.usage, None);
        assert_ne!(second.usage, None);
    }

    #[test]
    fn attempt_record_serde_roundtrip_all_outcomes() {
        let outcomes = [
            AttemptOutcome::Complete,
            AttemptOutcome::Rejected(AttemptRejection::InvalidToolCall {
                validation: ToolCallValidation::ArgumentsNotObject { call_ordinal: 1 },
            }),
            AttemptOutcome::Interrupted(AttemptInterruption::OutputLimitExceeded {
                kind: LimitKind::AssistantContent,
                limit: 2 * 1024 * 1024,
                actual: 2 * 1024 * 1024 + 1,
            }),
        ];
        for outcome in outcomes {
            let record = ModelAttemptRecord {
                key: key(0),
                origin: ProviderOrigin::Gemini,
                usage: None,
                outcome,
            };
            let json = serde_json::to_string(&record).expect("serialize");
            let back: ModelAttemptRecord = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(record, back);
        }
    }

    #[test]
    fn interrupted_and_cancelled_attempts_retain_supplied_usage() {
        // ADR 0005 §10: failed/interrupted attempts retain usage when the
        // provider supplied it; a provider that reports usage alongside a
        // cancellation must not have that fact dropped by the record.
        for interruption in [
            AttemptInterruption::Cancelled,
            AttemptInterruption::StreamError,
            AttemptInterruption::Timeout,
        ] {
            let record = ModelAttemptRecord {
                key: key(0),
                origin: ProviderOrigin::OpenAiCompatible,
                usage: Some(TokenUsage {
                    prompt_tokens: 41,
                    completion_tokens: 7,
                    total_tokens: 48,
                }),
                outcome: AttemptOutcome::Interrupted(interruption),
            };
            let json = serde_json::to_string(&record).expect("serialize");
            let back: ModelAttemptRecord = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(
                back.usage.as_ref().map(|usage| usage.total_tokens),
                Some(48)
            );
        }
    }
}
