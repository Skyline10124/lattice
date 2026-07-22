// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Error mapping — converts wasmtime traps, resource limits, and abort
//! reasons into structured `CepError` codes.
//!
//! Mapping priority:
//! 1. `abort = Cancelled` → `CallCancelled` (Permanent)
//! 2. `abort = Deadline`  → `ToolTimeout` (Transient)
//! 3. `abort = Memory`    → `ToolResourceExhausted` (resource = "memory")
//! 4. `abort = Instances` → `ToolResourceExhausted` (resource = "instances")
//! 5. fuel exhausted       → `ToolResourceExhausted` (resource = "fuel")
//! 6. other wasmtime trap → `ToolTrap` (Permanent)
//!
//! No guest backtrace, host path, or secret is exposed.

use lancet_protocol::{CepError, CepErrorCode, Retriability};

use crate::limits::AbortReason;
use crate::runtime::StoreState;

/// Map a wasmtime error to a `CepError` using the store's abort reason and
/// remaining fuel.
pub(crate) fn map_component_error(
    store: &wasmtime::Store<StoreState>,
    method: &str,
    err: wasmtime::Error,
) -> CepError {
    let data = store.data();
    if let Some(abort) = data.abort {
        return match abort {
            AbortReason::Cancelled => CepError::tool_execution(
                CepErrorCode::CallCancelled,
                method,
                "cancelled",
                None,
                Retriability::Permanent,
            ),
            AbortReason::Deadline => CepError::tool_execution(
                CepErrorCode::ToolTimeout,
                method,
                "timeout",
                None,
                Retriability::Transient,
            ),
            AbortReason::Memory => CepError::tool_execution(
                CepErrorCode::ToolResourceExhausted,
                method,
                "resource_exhausted",
                Some("memory".into()),
                Retriability::Transient,
            ),
            AbortReason::Instances => CepError::tool_execution(
                CepErrorCode::ToolResourceExhausted,
                method,
                "resource_exhausted",
                Some("instances".into()),
                Retriability::Transient,
            ),
        };
    }

    // Fuel exhaustion: wasmtime reports this as a trap. Check remaining fuel.
    if store.get_fuel().map(|f| f == 0).unwrap_or(false) {
        return CepError::tool_execution(
            CepErrorCode::ToolResourceExhausted,
            method,
            "resource_exhausted",
            Some("fuel".into()),
            Retriability::Transient,
        );
    }

    // Default: guest trap (unreachable, etc.).
    let _ = err; // suppress unused warning; the error details are not exposed
    CepError::tool_execution(
        CepErrorCode::ToolTrap,
        method,
        "trap",
        None,
        Retriability::Permanent,
    )
}

/// Build a `SchemaMismatch` error for params/result/result-cell validation.
pub(crate) fn schema_mismatch(method: &str, detail: impl Into<String>) -> CepError {
    lancet_protocol::error::invalid_params(format!("{method}: {}", detail.into()))
}

/// Build an `EffectViolation` error when the guest's result CellRef is not
/// in the descriptor's capability cells.
pub(crate) fn effect_violation(method: &str, cell_ref: &str) -> CepError {
    CepError::new(
        CepErrorCode::EffectViolation,
        format!("{method}: result cell {cell_ref} not in declared capabilities"),
        lancet_protocol::CepErrorData::Effect(Box::new(lancet_protocol::error::EffectErrorData {
            method: method.into(),
            declared_effect: vec![],
            authorized_effect: vec![],
            observed_effects: vec![],
            violations: vec![format!(
                "result cell {cell_ref} not in descriptor capability cells"
            )],
        })),
        Retriability::Permanent,
    )
}
