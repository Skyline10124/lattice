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
//! Two dimensions, as for model attempts: the outcome is the end shape, and
//! `IndeterminateCause` or the returned `CepError` carries the reason. Only
//! `NotExecuted` means the call was never handed to LANCET and guarantees no
//! external effect; every other outcome is recorded after the slot crossed
//! `Started`. Policy or validation refusals are `Failed` with a Validation,
//! Permission or Guard `CepError`; cancellation after dispatch is
//! `Indeterminate`.

use serde::{Deserialize, Serialize};

/// Closed sum of Tool execution outcomes for conversation closure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolResultOutcome {
    /// LANCET returned a JSON result.
    Succeeded,
    /// LANCET returned a `CepError`; its family and retriability say why.
    Failed,
    /// The coordinator never handed the call to LANCET; the Step's stop cause
    /// says why. The only outcome that guarantees no external effect.
    NotExecuted,
    /// The slot crossed `Started` without a determinate result: effects are
    /// unknown.
    Indeterminate,
}

impl ToolResultOutcome {
    /// Stable wire/digest spelling.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
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
