// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use lancet_protocol::error::PermissionErrorData;
use lancet_protocol::{CepError, CepErrorCode, CepErrorData, InvocationId, Retriability};
use lancet_runtime::AdmissionDenial;
use serde_json::{Value, json};

pub(super) fn admission_denied_error(method: &str, denial: &AdmissionDenial) -> CepError {
    CepError::new(
        CepErrorCode::CapabilityNotGranted,
        format!("{method}: {denial}"),
        CepErrorData::Permission(PermissionErrorData {
            required_capability: method.to_string(),
            granted_capabilities: denial.evidence.approved_capabilities.clone(),
        }),
        Retriability::Permanent,
    )
}

pub(super) fn call_cancelled_error(method: &str) -> CepError {
    CepError::tool_execution(
        CepErrorCode::CallCancelled,
        method,
        "cancelled",
        None,
        Retriability::Permanent,
    )
}

pub(super) fn is_incomplete_dispatch(result: &Result<Value, CepError>) -> bool {
    matches!(result, Err(err) if err.code == CepErrorCode::CallCancelled)
}

pub(super) async fn emit_cep_call_started(
    events: &lancet_dispatch::EventSink,
    invocation_id: InvocationId,
    method: &str,
    semantic_stage: &str,
    guard_stage: &str,
    policy_enforced: bool,
) {
    events
        .emit(
            "cep.call_started",
            json!({
                "invocation_id": invocation_id,
                "method": method,
                "semantic_stage": semantic_stage,
                "guard_stage": guard_stage,
                "policy_enforced": policy_enforced,
            }),
        )
        .await;
}

pub(super) async fn emit_cep_call_finished(
    events: &lancet_dispatch::EventSink,
    invocation_id: InvocationId,
    method: &str,
    result: &Result<Value, CepError>,
    semantic_stage: &str,
    guard_stage: &str,
    policy_enforced: bool,
) {
    let ok = result.is_ok();
    let error_code = result
        .as_ref()
        .err()
        .map(|error| format!("{:?}", error.code));
    let mut payload = json!({
        "invocation_id": invocation_id,
        "method": method,
        "ok": ok,
        "semantic_stage": semantic_stage,
        "guard_stage": guard_stage,
        "policy_enforced": policy_enforced,
    });
    if let Some(code) = error_code
        && let Some(obj) = payload.as_object_mut()
    {
        obj.insert("error_code".to_string(), Value::String(code.to_string()));
    }
    events.emit("cep.call_finished", payload).await;
}
