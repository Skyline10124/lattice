// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Typed CEP error model.
//!
//! Error codes live in JSON-RPC reserved range `-32000..-32099`, segmented by
//! family. Every error carries typed `data` and a mandatory `Retriability` hint.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Hint for whether a caller should retry, replan, or treat the error as final.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Retriability {
    Permanent,
    Transient,
    RequiresReplan,
    Unknown,
}

/// High-level CEP error family.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CepErrorFamily {
    Validation,
    Permission,
    Execution,
    State,
    Guard,
    Capability,
    Reserved,
}

/// Closed enum of CEP error codes within `-32000..-32099`.
///
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[repr(i32)]
pub enum CepErrorCode {
    // Stable small codes embedded in model-visible ToolResult content.
    SchemaMismatch = 0,
    PathContainment = 1,
    TypeError = 2,
    CapabilityNotGranted = 3,
    ProjectRootViolation = 4,
    AmbientAuthorityDenied = 5,
    ExecutionFailed = 6,
    ToolTrap = 9,
    ToolTimeout = 10,
    ToolResourceExhausted = 11,
    CallCancelled = 12,
    /// The effect may have occurred, but durable outcome evidence could not be completed.
    OutcomeIndeterminate = 13,
    ResourceConflict = 15,
    GuardDenied = 20,
    ReplanRequired = 21,
    CapabilityViolation = 22,
    Reserved = 23,
}

impl CepErrorCode {
    pub fn as_i32(self) -> i32 {
        match self {
            Self::SchemaMismatch => -32001,
            Self::PathContainment => -32002,
            Self::TypeError => -32003,
            Self::CapabilityNotGranted => -32010,
            Self::ProjectRootViolation => -32011,
            Self::AmbientAuthorityDenied => -32012,
            Self::ExecutionFailed => -32020,
            Self::ToolTrap => -32023,
            Self::ToolTimeout => -32024,
            Self::ToolResourceExhausted => -32025,
            Self::CallCancelled => -32026,
            Self::OutcomeIndeterminate => -32027,
            Self::ResourceConflict => -32031,
            Self::GuardDenied => -32042,
            Self::ReplanRequired => -32043,
            Self::CapabilityViolation => -32050,
            Self::Reserved => -32060,
        }
    }

    pub fn family(self) -> CepErrorFamily {
        match self {
            Self::SchemaMismatch | Self::PathContainment | Self::TypeError => {
                CepErrorFamily::Validation
            }
            Self::CapabilityNotGranted
            | Self::ProjectRootViolation
            | Self::AmbientAuthorityDenied => CepErrorFamily::Permission,
            Self::ExecutionFailed
            | Self::ToolTrap
            | Self::ToolTimeout
            | Self::ToolResourceExhausted
            | Self::CallCancelled
            | Self::OutcomeIndeterminate => CepErrorFamily::Execution,
            Self::ResourceConflict => CepErrorFamily::State,
            Self::GuardDenied | Self::ReplanRequired => CepErrorFamily::Guard,
            Self::CapabilityViolation => CepErrorFamily::Capability,
            Self::Reserved => CepErrorFamily::Reserved,
        }
    }

    pub fn from_i32(code: i32) -> Option<Self> {
        Some(match code {
            -32001 => Self::SchemaMismatch,
            -32002 => Self::PathContainment,
            -32003 => Self::TypeError,
            -32010 => Self::CapabilityNotGranted,
            -32011 => Self::ProjectRootViolation,
            -32012 => Self::AmbientAuthorityDenied,
            -32020 => Self::ExecutionFailed,
            -32023 => Self::ToolTrap,
            -32024 => Self::ToolTimeout,
            -32025 => Self::ToolResourceExhausted,
            -32026 => Self::CallCancelled,
            -32027 => Self::OutcomeIndeterminate,
            -32031 => Self::ResourceConflict,
            -32042 => Self::GuardDenied,
            -32043 => Self::ReplanRequired,
            -32050 => Self::CapabilityViolation,
            -32060 => Self::Reserved,
            _ => return None,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ValidationErrorData {
    pub field: String,
    pub reason: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PermissionErrorData {
    pub required_capability: String,
    pub granted_capabilities: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ExecutionErrorData {
    pub operation: String,
    pub detail: Option<String>,
}

/// Structured error data for Component Tool execution failures
/// (#27 (Codeberg #5) slice C).
///
/// Carries the tool `method`, a coarse `category` (`"trap"`, `"timeout"`,
/// `"resource_exhausted"`, `"cancelled"`), and an optional `resource` name
/// for resource-exhaustion cases (`"memory"`, `"fuel"`, `"instances"`).
/// It deliberately exposes **no** guest backtrace, host path, or secret.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ToolExecutionErrorData {
    pub method: String,
    pub category: String,
    pub resource: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct StateErrorData {
    pub resource: String,
    pub reason: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct GuardErrorData {
    pub method: String,
    pub reason: String,
}

/// Component Cell/capability violation data.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CapabilityErrorData {
    pub method: String,
    pub violations: Vec<String>,
    /// Trust-free CellRef (`ctx:ext:scope`) that failed the ceiling or
    /// admitted-set check.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cell_ref: Option<String>,
    /// Capability name the call lacked, when the violation names one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capability: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ReservedErrorData {
    pub code: String,
    pub detail: String,
}

/// Tagged enum of typed error data — one variant per family.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CepErrorData {
    Validation(ValidationErrorData),
    Permission(PermissionErrorData),
    Execution(ExecutionErrorData),
    ToolExecution(ToolExecutionErrorData),
    State(StateErrorData),
    Guard(GuardErrorData),
    Capability(Box<CapabilityErrorData>),
    Reserved(ReservedErrorData),
}

/// A CEP error with typed data and mandatory retriability.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CepError {
    pub code: CepErrorCode,
    pub message: String,
    pub data: CepErrorData,
    pub retriability: Retriability,
}

impl CepError {
    pub fn new(
        code: CepErrorCode,
        message: impl Into<String>,
        data: CepErrorData,
        retriability: Retriability,
    ) -> Self {
        Self {
            code,
            message: message.into(),
            data,
            retriability,
        }
    }

    pub fn parse_error(message: impl Into<String>) -> Self {
        parse_error(message)
    }

    pub fn method_not_found(method: impl Into<String>) -> Self {
        method_not_found(method)
    }

    pub fn invalid_params(message: impl Into<String>) -> Self {
        invalid_params(message)
    }

    /// Build a Component Tool execution error (#27 slice C). `method` is the
    /// tool name, `category` is the coarse failure class (`"trap"`, `"timeout"`,
    /// `"resource_exhausted"`, `"cancelled"`), and `resource` names the
    /// exhausted resource for resource-exhaustion cases.
    pub fn tool_execution(
        code: CepErrorCode,
        method: impl Into<String>,
        category: impl Into<String>,
        resource: Option<String>,
        retriability: Retriability,
    ) -> Self {
        let method = method.into();
        let category = category.into();
        Self::new(
            code,
            format!("tool {method} failed: {category}"),
            CepErrorData::ToolExecution(ToolExecutionErrorData {
                method,
                category,
                resource,
            }),
            retriability,
        )
    }

    pub fn internal_error(message: impl Into<String>) -> Self {
        Self {
            code: CepErrorCode::ExecutionFailed,
            message: message.into(),
            data: CepErrorData::Execution(ExecutionErrorData {
                operation: "internal".into(),
                detail: None,
            }),
            retriability: Retriability::Transient,
        }
    }

    /// JSON-RPC wire code for this error (includes standard JSON-RPC codes).
    pub fn wire_code(&self) -> i32 {
        jsonrpc_wire_code(self)
    }
}

/// Standard JSON-RPC error codes (distinct from CEP family codes).
pub mod jsonrpc {
    pub const PARSE_ERROR: i32 = -32700;
    pub const METHOD_NOT_FOUND: i32 = -32601;
    pub const INVALID_PARAMS: i32 = -32602;
    pub const INTERNAL_ERROR: i32 = -32603;
}

/// Build a CEP error for JSON-RPC parse failures with wire code -32700.
pub fn parse_error(message: impl Into<String>) -> CepError {
    CepError {
        code: CepErrorCode::SchemaMismatch,
        message: message.into(),
        data: CepErrorData::Validation(ValidationErrorData {
            field: "jsonrpc".into(),
            reason: "parse error".into(),
        }),
        retriability: Retriability::Permanent,
    }
}

/// Build a CEP error for method-not-found with JSON-RPC code -32601.
pub fn method_not_found(method: impl Into<String>) -> CepError {
    let method = method.into();
    CepError {
        code: CepErrorCode::SchemaMismatch,
        message: format!("Method not found: {method}"),
        data: CepErrorData::Validation(ValidationErrorData {
            field: "method".into(),
            reason: format!("unknown method: {method}"),
        }),
        retriability: Retriability::Permanent,
    }
}

/// Build a CEP error for invalid params with JSON-RPC code -32602.
pub fn invalid_params(message: impl Into<String>) -> CepError {
    CepError {
        code: CepErrorCode::SchemaMismatch,
        message: message.into(),
        data: CepErrorData::Validation(ValidationErrorData {
            field: "params".into(),
            reason: "invalid params".into(),
        }),
        retriability: Retriability::Permanent,
    }
}

/// Convert a CEP error to its JSON-RPC wire representation.
pub fn to_jsonrpc_error(cep: &CepError) -> (i32, String, CepError) {
    let code = jsonrpc_wire_code(cep);
    (code, cep.message.clone(), cep.clone())
}

fn jsonrpc_wire_code(cep: &CepError) -> i32 {
    match &cep.data {
        CepErrorData::Validation(data)
            if data.field == "jsonrpc" && data.reason == "parse error" =>
        {
            jsonrpc::PARSE_ERROR
        }
        CepErrorData::Validation(data)
            if data.field == "method" && data.reason.starts_with("unknown method:") =>
        {
            jsonrpc::METHOD_NOT_FOUND
        }
        CepErrorData::Validation(data)
            if data.field == "params" && data.reason == "invalid params" =>
        {
            jsonrpc::INVALID_PARAMS
        }
        _ => cep.code.as_i32(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn family_range(family: CepErrorFamily) -> (i32, i32) {
        // (min, max) where min and max are negative and min <= max numerically
        match family {
            CepErrorFamily::Validation => (-32009, -32001),
            CepErrorFamily::Permission => (-32019, -32010),
            CepErrorFamily::Execution => (-32029, -32020),
            CepErrorFamily::State => (-32039, -32030),
            CepErrorFamily::Guard => (-32049, -32040),
            CepErrorFamily::Capability => (-32059, -32050),
            CepErrorFamily::Reserved => (-32099, -32060),
        }
    }

    #[test]
    fn every_family_has_code_in_range() {
        let codes = [
            CepErrorCode::SchemaMismatch,
            CepErrorCode::CapabilityNotGranted,
            CepErrorCode::ExecutionFailed,
            CepErrorCode::ToolTrap,
            CepErrorCode::ToolTimeout,
            CepErrorCode::ToolResourceExhausted,
            CepErrorCode::CallCancelled,
            CepErrorCode::ResourceConflict,
            CepErrorCode::GuardDenied,
            CepErrorCode::CapabilityViolation,
            CepErrorCode::Reserved,
        ];
        for code in codes {
            let i = code.as_i32();
            let (lo, hi) = family_range(code.family());
            assert!(
                i >= lo && i <= hi,
                "code {i} not in family range [{lo}, {hi}]"
            );
            assert!((-32099..=-32001).contains(&i), "code {i} outside CEP range");
        }
    }

    #[test]
    fn family_code_round_trips_through_jsonrpc_error_response() {
        let families = [
            (
                CepErrorCode::SchemaMismatch,
                CepErrorData::Validation(ValidationErrorData {
                    field: "path".into(),
                    reason: "bad".into(),
                }),
            ),
            (
                CepErrorCode::CapabilityNotGranted,
                CepErrorData::Permission(PermissionErrorData {
                    required_capability: "write_path".into(),
                    granted_capabilities: vec![],
                }),
            ),
            (
                CepErrorCode::ExecutionFailed,
                CepErrorData::Execution(ExecutionErrorData {
                    operation: "mutate.edit".into(),
                    detail: Some("install failed".into()),
                }),
            ),
            (
                CepErrorCode::ResourceConflict,
                CepErrorData::State(StateErrorData {
                    resource: "workspace".into(),
                    reason: "busy".into(),
                }),
            ),
            (
                CepErrorCode::GuardDenied,
                CepErrorData::Guard(GuardErrorData {
                    method: "observe.workspace".into(),
                    reason: "path denied".into(),
                }),
            ),
            (
                CepErrorCode::CapabilityViolation,
                CepErrorData::Capability(Box::new(CapabilityErrorData {
                    method: "observe.workspace".into(),
                    violations: vec![
                        "observed workspace_mutation not in declared effect ceiling".into(),
                    ],
                    cell_ref: None,
                    capability: None,
                })),
            ),
            (
                CepErrorCode::Reserved,
                CepErrorData::Reserved(ReservedErrorData {
                    code: "future".into(),
                    detail: "reserved".into(),
                }),
            ),
        ];

        for (code, data) in families {
            let err = CepError {
                code,
                message: "test".into(),
                data,
                retriability: Retriability::Permanent,
            };
            let resp: crate::frame::ToolResult = crate::jsonrpc::Response::Error {
                error: crate::jsonrpc::ErrorObject {
                    code: err.code.as_i32(),
                    message: err.message.clone(),
                    data: Some(err.clone()),
                },
                id: crate::jsonrpc::Id::parse("11111111-1111-4111-8111-111111111111")
                    .expect("valid InvocationId"),
            };
            let serialized = serde_json::to_string(&resp).unwrap();
            let parsed: crate::frame::ToolResult = serde_json::from_str(&serialized).unwrap();
            if let crate::jsonrpc::Response::Error { error, .. } = parsed {
                let data = error.data.unwrap();
                assert_eq!(data.code, err.code);
                assert_eq!(data.retriability, Retriability::Permanent);
            } else {
                panic!("expected error response");
            }
        }
    }

    #[test]
    fn cep_error_data_is_tagged_and_rejects_free_form_json() {
        let good = json!({
            "kind": "validation",
            "field": "path",
            "reason": "missing"
        });
        let parsed: CepErrorData = serde_json::from_value(good).unwrap();
        assert!(matches!(parsed, CepErrorData::Validation(_)));

        let bad = json!({"free_form": true, "message": "oops"});
        let result: Result<CepErrorData, _> = serde_json::from_value(bad);
        assert!(result.is_err());
    }

    #[test]
    fn capability_error_data_round_trips_without_legacy_effect_fields() {
        let wrapped = CepErrorData::Capability(Box::new(CapabilityErrorData {
            method: "observe.workspace".into(),
            violations: vec!["scope denied".into()],
            cell_ref: Some("alloc:none:workspace".into()),
            capability: Some("alloc:read:workspace".into()),
        }));
        let value = serde_json::to_value(&wrapped).unwrap();
        assert_eq!(value["kind"], "capability");
        assert!(value.get("declared_effect").is_none());
        assert!(value.get("authorized_effect").is_none());
        assert_eq!(
            serde_json::from_value::<CepErrorData>(value).unwrap(),
            wrapped
        );
    }

    #[test]
    fn retriability_present_after_round_trip() {
        let err = CepError {
            code: CepErrorCode::ExecutionFailed,
            message: "fail".into(),
            data: CepErrorData::Execution(ExecutionErrorData {
                operation: "test".into(),
                detail: None,
            }),
            retriability: Retriability::RequiresReplan,
        };
        let serialized = serde_json::to_string(&err).unwrap();
        let deserialized: CepError = serde_json::from_str(&serialized).unwrap();
        assert_eq!(deserialized.retriability, Retriability::RequiresReplan);
    }

    #[test]
    fn method_not_found_yields_32601() {
        let err = method_not_found("x");
        let (code, _, _) = to_jsonrpc_error(&err);
        assert_eq!(code, jsonrpc::METHOD_NOT_FOUND);
    }

    #[test]
    fn standard_jsonrpc_helpers_share_wire_mapping() {
        let parse = CepError::parse_error("bad json");
        assert_eq!(parse.wire_code(), jsonrpc::PARSE_ERROR);
        assert_eq!(to_jsonrpc_error(&parse).0, jsonrpc::PARSE_ERROR);

        let inherent_method = CepError::method_not_found("x");
        assert_eq!(inherent_method.wire_code(), jsonrpc::METHOD_NOT_FOUND);
        assert_eq!(
            to_jsonrpc_error(&inherent_method).0,
            jsonrpc::METHOD_NOT_FOUND
        );

        let free_method = method_not_found("x");
        assert_eq!(free_method.wire_code(), jsonrpc::METHOD_NOT_FOUND);
        assert_eq!(to_jsonrpc_error(&free_method).0, jsonrpc::METHOD_NOT_FOUND);

        let inherent_params = CepError::invalid_params("bad params");
        assert_eq!(inherent_params.wire_code(), jsonrpc::INVALID_PARAMS);
        assert_eq!(
            to_jsonrpc_error(&inherent_params).0,
            jsonrpc::INVALID_PARAMS
        );

        let free_params = invalid_params("bad params");
        assert_eq!(free_params.wire_code(), jsonrpc::INVALID_PARAMS);
        assert_eq!(to_jsonrpc_error(&free_params).0, jsonrpc::INVALID_PARAMS);
    }

    #[test]
    fn no_1000_series_codes_in_module() {
        // All CEP codes must be in -32000..-32099 range
        let all_codes = [
            CepErrorCode::SchemaMismatch,
            CepErrorCode::PathContainment,
            CepErrorCode::TypeError,
            CepErrorCode::CapabilityNotGranted,
            CepErrorCode::ProjectRootViolation,
            CepErrorCode::AmbientAuthorityDenied,
            CepErrorCode::ExecutionFailed,
            CepErrorCode::ToolTrap,
            CepErrorCode::ToolTimeout,
            CepErrorCode::ToolResourceExhausted,
            CepErrorCode::CallCancelled,
            CepErrorCode::ResourceConflict,
            CepErrorCode::GuardDenied,
            CepErrorCode::ReplanRequired,
            CepErrorCode::CapabilityViolation,
            CepErrorCode::Reserved,
        ];
        for code in all_codes {
            let i = code.as_i32();
            assert!((-32099..=-32001).contains(&i), "code {i} not in CEP range");
            assert!(i.abs() >= 32001, "no 1000-series codes: {i}");
        }
    }

    #[test]
    fn model_visible_codes_keep_stable_holes_after_legacy_variant_removal() {
        assert_eq!(CepErrorCode::SchemaMismatch as i32, 0);
        assert_eq!(CepErrorCode::ToolTrap as i32, 9);
        assert_eq!(CepErrorCode::ResourceConflict as i32, 15);
        assert_eq!(CepErrorCode::GuardDenied as i32, 20);
        assert_eq!(CepErrorCode::CapabilityViolation as i32, 22);
        assert_eq!(CepErrorCode::Reserved as i32, 23);
    }

    #[test]
    fn typed_data_not_raw_value() {
        // This test documents that CepErrorData is a tagged enum, not serde_json::Value.
        // If data were raw Value, this would compile differently.
        fn assert_typed(_: CepErrorData) {}
        assert_typed(CepErrorData::Validation(ValidationErrorData {
            field: "x".into(),
            reason: "y".into(),
        }));
    }

    #[test]
    fn tool_execution_codes_are_in_execution_family_range() {
        // #27 slice C: new Execution-family codes for Component tool failures.
        // Each must sit in -32020..-32029, map to the Execution family, and
        // round-trip through as_i32/from_i32.
        let new_codes = [
            (CepErrorCode::ToolTrap, -32023),
            (CepErrorCode::ToolTimeout, -32024),
            (CepErrorCode::ToolResourceExhausted, -32025),
            (CepErrorCode::CallCancelled, -32026),
        ];
        for (code, expected_i) in new_codes {
            let i = code.as_i32();
            assert_eq!(i, expected_i, "wire code for {code:?}");
            assert_eq!(
                code.family(),
                CepErrorFamily::Execution,
                "{code:?} must be in Execution family"
            );
            assert_eq!(
                CepErrorCode::from_i32(i),
                Some(code),
                "from_i32 round-trip for {code:?}"
            );
        }
    }

    #[test]
    fn tool_execution_error_data_is_tagged_and_leak_free() {
        // The data carries method + category + optional resource, never a
        // guest backtrace, host path, or secret.
        let data = ToolExecutionErrorData {
            method: "noop.echo".into(),
            category: "trap".into(),
            resource: None,
        };
        let wrapped = CepErrorData::ToolExecution(data.clone());
        let value = serde_json::to_value(&wrapped).unwrap();
        assert_eq!(value["kind"], "tool_execution");
        assert_eq!(value["method"], "noop.echo");
        assert_eq!(value["category"], "trap");
        assert!(value["resource"].is_null());

        // No backtrace/host_path/secret field exists.
        let serialized = serde_json::to_string(&wrapped).unwrap();
        assert!(!serialized.contains("backtrace"));
        assert!(!serialized.contains("host_path"));
        assert!(!serialized.contains("secret"));

        let deserialized: CepErrorData = serde_json::from_str(&serialized).unwrap();
        assert_eq!(deserialized, wrapped);

        // Resource-exhausted variant carries the resource name.
        let res_data = ToolExecutionErrorData {
            method: "noop.echo".into(),
            category: "resource_exhausted".into(),
            resource: Some("memory".into()),
        };
        let res_wrapped = CepErrorData::ToolExecution(res_data);
        let res_value = serde_json::to_value(&res_wrapped).unwrap();
        assert_eq!(res_value["resource"], "memory");
    }

    #[test]
    fn tool_execution_error_round_trips_through_jsonrpc_response() {
        let err = CepError {
            code: CepErrorCode::ToolTrap,
            message: "guest trap".into(),
            data: CepErrorData::ToolExecution(ToolExecutionErrorData {
                method: "noop.echo".into(),
                category: "trap".into(),
                resource: None,
            }),
            retriability: Retriability::Permanent,
        };
        let resp: crate::frame::ToolResult = crate::jsonrpc::Response::Error {
            error: crate::jsonrpc::ErrorObject {
                code: err.code.as_i32(),
                message: err.message.clone(),
                data: Some(err.clone()),
            },
            id: crate::jsonrpc::Id::parse("11111111-1111-4111-8111-111111111111")
                .expect("valid InvocationId"),
        };
        let serialized = serde_json::to_string(&resp).unwrap();
        let parsed: crate::frame::ToolResult = serde_json::from_str(&serialized).unwrap();
        if let crate::jsonrpc::Response::Error { error, .. } = parsed {
            let data = error.data.unwrap();
            assert_eq!(data.code, CepErrorCode::ToolTrap);
            assert_eq!(data.retriability, Retriability::Permanent);
        } else {
            panic!("expected error response");
        }
    }

    #[test]
    fn capability_error_data_carries_violation_evidence() {
        let data = CapabilityErrorData {
            method: "observe.workspace".into(),
            violations: vec!["result cell exceeds admitted capability set".into()],
            cell_ref: None,
            capability: None,
        };

        let wrapped = CepErrorData::Capability(Box::new(data.clone()));
        let value = serde_json::to_value(&wrapped).unwrap();
        assert_eq!(value["kind"], "capability");
        assert_eq!(value["method"], "observe.workspace");
        assert!(value["violations"].is_array());
        assert_eq!(
            value["violations"][0],
            "result cell exceeds admitted capability set"
        );

        let serialized = serde_json::to_string(&wrapped).unwrap();
        let deserialized: CepErrorData = serde_json::from_str(&serialized).unwrap();
        assert_eq!(deserialized, wrapped);
    }
}
