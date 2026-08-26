// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Internal execution receipt for coordinator-bound Tool dispatch (#139-B).
//!
//! Ordinary JSON-RPC callers continue to use `CepClient::call_raw` and see
//! only the Tool result. The Turn coordinator uses
//! `CepClient::call_raw_with_receipt` to obtain the content-free Cell
//! evidence triple alongside that result. This module is transport-internal:
//! it must not depend on `lancet-audit`.

use lancet_protocol::{CellEvidenceTriple, CepError, ToolResult};
use serde_json::Value;

/// One internal server→client dispatch response: JSON-RPC result plus evidence.
#[derive(Debug)]
pub struct InternalCallResponse {
    /// The ordinary JSON-RPC Tool result envelope.
    pub tool_result: ToolResult,
    /// Typed non-secret Cell evidence for terminal publication.
    pub evidence: CellEvidenceTriple,
}

impl InternalCallResponse {
    /// Split into the coordinator-facing receipt (result value + evidence).
    pub fn into_receipt(self) -> ExecutionReceipt {
        let result = match self.tool_result {
            ToolResult::Success { result, .. } => Ok(result),
            ToolResult::Error { error, .. } => {
                let cep = error.data.unwrap_or_else(|| {
                    CepError::internal_error(format!("{} ({})", error.message, error.code))
                });
                Err(cep)
            }
        };
        ExecutionReceipt {
            result,
            evidence: self.evidence,
        }
    }
}

/// Coordinator-facing receipt: success/error value plus evidence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionReceipt {
    /// Canonical Tool result JSON, or the structured CEP error.
    pub result: Result<Value, CepError>,
    /// Content-free Cell evidence triple.
    pub evidence: CellEvidenceTriple,
}
