// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Provider-neutral Tool result closure.
//!
//! The execution outcome is a closed sum. A `ToolResult` conversation part
//! must close its ToolCall through the provider result block with exactly one
//! of these outcomes; it is never re-labeled after the fact. The durable
//! invocation slot in `lattice-session` stores the same vocabulary plus the
//! outcome-shaped payload (see `docs/architecture/session.md#调用槽位`).
//!
//! Only `NotExecuted` means the call was never handed to LANCET. Every other
//! outcome is recorded after the slot crossed `Started`, so it may have had
//! external effects.

use serde::{Deserialize, Serialize};

/// Closed sum of Tool execution outcomes for conversation closure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolResultOutcome {
    /// LANCET returned a JSON result.
    Succeeded,
    /// LANCET returned a `CepError` outside the Validation, Permission and
    /// Guard families (other than `CallCancelled` / `OutcomeIndeterminate`).
    Failed,
    /// LANCET returned a Validation, Permission or Guard `CepError`. The refusal
    /// may come from admission (the Component never ran) or from a host
    /// operation refused mid-execution, after earlier host operations took
    /// effect; it does not mean "rejected before execution".
    Rejected,
    /// LANCET returned `CallCancelled`: cancelled after dispatch.
    Cancelled,
    /// The coordinator never handed the call to LANCET (cancelled or out of
    /// deadline before dispatch, a preceding call was indeterminate, or the
    /// Turn stopped). The only outcome that guarantees no external effect.
    NotExecuted,
    /// The slot crossed `Started` but no result was recorded before the Step
    /// closed (stop or crash), or LANCET returned `OutcomeIndeterminate`:
    /// effects are unknown.
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
