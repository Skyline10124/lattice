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
//! Call-scoped cancellation state is the sole cancellation authority
//! (#70, Codeberg #48): the epoch-abort path here, the pre-call check, and
//! the guest-logical-error path in `runtime::call` all produce the shared
//! [`cancelled_error`]; a guest-controlled error string never classifies a
//! cancellation.
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

/// Build the shared structured cancellation error (#70). Every call-scoped
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
///
/// `detail` must not embed Tool data. The message reaches the durable audit row
/// through `compact_error_summary`, and audit records paths, hashes and object
/// ids — never content (root `AGENTS.md`). Use [`schema_violation_detail`] to
/// turn a `jsonschema` error into a location-only description.
pub(crate) fn schema_mismatch(method: &str, detail: impl Into<String>) -> CepError {
    lancet_protocol::error::invalid_params(format!("{method}: {}", detail.into()))
}

/// Describe a JSON Schema validation failure by *where* it failed, never by the
/// value that failed.
///
/// `jsonschema`'s `Display` embeds the offending instance — for a rejected
/// `session.todo_write` call that is the user's whole todo list, which then lands
/// verbatim in `cep_audit_entries.result_or_error`. The instance pointer and the
/// schema keyword are enough for a caller to fix the call and for an operator to
/// see what was refused.
pub(crate) fn schema_violation_detail(
    what: &str,
    error: &jsonschema::ValidationError<'_>,
) -> String {
    let location = error.instance_path().to_string();
    let location = if location.is_empty() {
        "(root)".to_string()
    } else {
        location
    };
    // `keyword_location` is the schema pointer, which is authored by us (the
    // manifest), not by the caller — safe to name.
    format!(
        "{what} violates the declared schema at {location} (schema {})",
        error.schema_path()
    )
}

/// Build an `CapabilityViolation` error when the guest's result CellRef is not
/// in the descriptor's capability cells.
pub(crate) fn effect_violation(method: &str, cell_ref: &str) -> CepError {
    result_cell_violation(
        method,
        cell_ref,
        "not in declared capabilities",
        "not in descriptor capability cells",
    )
}

/// Build an `CapabilityViolation` error when the result CellRef is declared by
/// the descriptor but outside the set admitted for this call (the effect
/// ceiling, D3). Same code as a declaration violation — the admitted set is
/// an intersection, not a separate error family.
pub(crate) fn effect_not_admitted(method: &str, cell_ref: &str) -> CepError {
    result_cell_violation(
        method,
        cell_ref,
        "not in the admitted capability set",
        "result cell declared but not admitted for this call",
    )
}

fn result_cell_violation(method: &str, cell_ref: &str, summary: &str, violation: &str) -> CepError {
    CepError::new(
        CepErrorCode::CapabilityViolation,
        format!("{method}: result cell {cell_ref} {summary}"),
        lancet_protocol::CepErrorData::Capability(Box::new(
            lancet_protocol::error::CapabilityErrorData {
                method: method.into(),
                violations: vec![format!("result cell {cell_ref} {violation}")],
                cell_ref: Some(cell_ref.to_owned()),
                capability: None,
            },
        )),
        Retriability::Permanent,
    )
}
