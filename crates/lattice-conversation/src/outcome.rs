// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Provider-neutral Tool result closure (ADR 0005 §7/§11).
//!
//! The execution outcome is a closed sum. A `ToolResult` conversation part
//! must close its ToolCall through the provider result block with exactly one
//! of these outcomes; it is never re-labeled after the fact. The
//! InvocationStore (#139-A) owns the durable terminal record with the same
//! vocabulary plus typed reasons; this is the conversation-facing closure.

use serde::{Deserialize, Serialize};

/// Closed sum of Tool execution outcomes for conversation closure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolResultOutcome {
    /// The Tool executed and returned a result.
    Succeeded,
    /// The Tool executed and produced a failure result.
    Failed,
    /// The call was rejected before execution (policy, guard, or validation).
    Rejected,
    /// Execution was cancelled after dispatch.
    Cancelled,
    /// The call never dispatched (e.g. an earlier binding in the batch stopped
    /// the batch).
    NotExecuted,
    /// The system cannot determine whether execution occurred (the durable
    /// `DispatchStarted` boundary was crossed without a terminal fact).
    Indeterminate,
}

impl ToolResultOutcome {
    /// Stable wire/digest spelling.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Rejected => "rejected",
            Self::Cancelled => "cancelled",
            Self::NotExecuted => "not_executed",
            Self::Indeterminate => "indeterminate",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outcome_wire_spellings_are_stable() {
        let cases = [
            (ToolResultOutcome::Succeeded, "succeeded"),
            (ToolResultOutcome::Failed, "failed"),
            (ToolResultOutcome::Rejected, "rejected"),
            (ToolResultOutcome::Cancelled, "cancelled"),
            (ToolResultOutcome::NotExecuted, "not_executed"),
            (ToolResultOutcome::Indeterminate, "indeterminate"),
        ];
        for (outcome, spelling) in cases {
            assert_eq!(outcome.as_str(), spelling);
            let json = serde_json::to_string(&outcome).expect("serialize");
            assert_eq!(json, format!("\"{spelling}\""));
            let back: ToolResultOutcome = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(outcome, back);
        }
    }
}
