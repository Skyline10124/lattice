// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Why a provider attempt ended without a usable message (ADR 0005 §7/§10).

use serde::{Deserialize, Serialize};

use crate::limits::LimitKind;
use crate::tool_call::ToolCallValidation;

/// A completed provider output rejected before binding.
///
/// A rejected attempt retains its bounded assembled output and diagnostics
/// for Session diagnosis but produces no invocation slot, `InvocationId`, or
/// conversation-tree content.
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
}
