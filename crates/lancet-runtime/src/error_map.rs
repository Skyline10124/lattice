// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Error mapping — converts wasmtime traps, resource limits, and abort
//! reasons into structured `CepError` codes.
//!
//! Mapping priority:
//! 1. `abort = Cancelled`      → `CallCancelled` (Permanent)
//! 2. `abort = Deadline`       → `ToolTimeout` (Transient)
//! 3. `last_rejection = MemoryBytes`   → `ToolResourceExhausted` (resource = "memory")
//! 4. `last_rejection = TableElements` → `ToolResourceExhausted` (resource = "table")
//! 5. fuel exhausted           → `ToolResourceExhausted` (resource = "fuel")
//! 6. count limit exceeded     → `ToolResourceExhausted` (resource = "instances"/"memories"/"tables")
//! 7. other wasmtime trap      → `ToolTrap` (Permanent)
//!
//! Cancellation/deadline are checked before any resource rejection so their
//! priority is preserved. Memory/table byte-and-element growth rejections are
//! recorded by `RecordingLimits`; count-limit failures have no rejection
//! callback and are classified from the tool's static footprint (never from a
//! wasmtime/guest error string).
//!
//! Call-scoped cancellation state is the sole cancellation authority (#48):
//! the epoch-abort path here, the pre-call check, and the guest-logical-error
//! path in `runtime::call` all produce the shared [`cancelled_error`]; a
//! guest-controlled error string never classifies a cancellation.
//!
//! No guest backtrace, host path, or secret is exposed.

use lancet_protocol::{CepError, CepErrorCode, Retriability};

use crate::limits::{AbortReason, ResourceFootprint};
use crate::runtime::StoreState;

/// Map a wasmtime error to a `CepError` using the store's abort reason, the
/// limiter's recorded growth rejection, remaining fuel, and — for count limits
/// with no rejection callback — the tool's static `footprint` versus the
/// configured counts.
pub(crate) fn map_component_error(
    store: &wasmtime::Store<StoreState>,
    method: &str,
    err: wasmtime::Error,
    footprint: &ResourceFootprint,
) -> CepError {
    let data = store.data();

    // Cancellation/deadline (epoch) take priority; then a memory/table growth
    // rejection recorded by the limiter. `abort` wins over `last_rejection` so
    // a cancellation during growth is still reported as a cancellation.
    if let Some(reason) = data.abort.or(data.limits.last_rejection) {
        return match reason {
            AbortReason::Cancelled => cancelled_error(method),
            AbortReason::Deadline => CepError::tool_execution(
                CepErrorCode::ToolTimeout,
                method,
                "timeout",
                None,
                Retriability::Transient,
            ),
            AbortReason::MemoryBytes => resource_exhausted(method, "memory"),
            AbortReason::TableElements => resource_exhausted(method, "table"),
        };
    }

    // Fuel exhaustion: wasmtime reports this as a trap. Check remaining fuel.
    if store.get_fuel().map(|f| f == 0).unwrap_or(false) {
        return resource_exhausted(method, "fuel");
    }

    // Count-limit exhaustion (instances/memories/tables). These have no
    // rejection callback, so classify from the component's static footprint
    // against the configured counts, in wasmtime's own enforcement order. A
    // successful instantiation implies the footprint fit, so this is a no-op on
    // the `call_run` path.
    if let Some(resource) = footprint.first_exceeded(&data.count_limits) {
        return resource_exhausted(method, resource);
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

/// Build the shared structured cancellation error (#48). Every call-scoped
/// cancellation observation point (host pre-check, guest cooperative exit,
/// epoch interruption) maps to this exact error.
pub(crate) fn cancelled_error(method: &str) -> CepError {
    CepError::tool_execution(
        CepErrorCode::CallCancelled,
        method,
        "cancelled",
        None,
        Retriability::Permanent,
    )
}

/// Build a `ToolResourceExhausted` error carrying a fixed, non-leaking resource
/// label (`"memory"`, `"table"`, `"fuel"`, `"instances"`, `"memories"`,
/// `"tables"`).
fn resource_exhausted(method: &str, resource: &str) -> CepError {
    CepError::tool_execution(
        CepErrorCode::ToolResourceExhausted,
        method,
        "resource_exhausted",
        Some(resource.into()),
        Retriability::Transient,
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
