// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Typed CEP error model.
//!
//! Error codes live in JSON-RPC reserved range `-32000..-32099`, segmented by
//! family. Every error carries typed `data` and a mandatory `Retriability` hint.

use crate::effect_types::ImpactKind;
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
    Effect,
    Reserved,
}

/// Closed enum of CEP error codes within `-32000..-32099`.
///
/// `CommandTimeout` (`-32021`) is reserved for execution deadline elapse
/// scenarios, including cases where cleanup succeeds or the child has already
/// exited. `ChildKillFailed` (`-32022`) is reserved for timeout cleanup
/// failures, such as `Child::kill().await` returning an error or the OS
/// reporting an unreaped child after the deadline.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CepErrorCode {
    // Validation (-32001..-32009)
    SchemaMismatch,
    PathContainment,
    TypeError,
    // Permission (-32010..-32019)
    CapabilityNotGranted,
    ProjectRootViolation,
    AmbientAuthorityDenied,
    // Execution (-32020..-32029)
    ExecutionFailed,
    CommandTimeout,
    ChildKillFailed,
    // State (-32030..-32039)
    StaleContext,
    ResourceConflict,
    EnvelopeAlreadyActive,
    DuplicateRegistration,
    // Guard (-32040..-32049)
    EnvelopeMissing,
    OutOfScopeMutation,
    GuardDenied,
    ReplanRequired,
    // Effect (-32050..-32059)
    EffectViolation,
    // Reserved (-32060..-32099)
    Reserved,
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
            Self::CommandTimeout => -32021,
            Self::ChildKillFailed => -32022,
            Self::StaleContext => -32030,
            Self::ResourceConflict => -32031,
            Self::EnvelopeAlreadyActive => -32032,
            Self::DuplicateRegistration => -32033,
            Self::EnvelopeMissing => -32040,
            Self::OutOfScopeMutation => -32041,
            Self::GuardDenied => -32042,
            Self::ReplanRequired => -32043,
            Self::EffectViolation => -32050,
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
            | Self::CommandTimeout
            | Self::ChildKillFailed => CepErrorFamily::Execution,
            Self::StaleContext
            | Self::ResourceConflict
            | Self::EnvelopeAlreadyActive
            | Self::DuplicateRegistration => CepErrorFamily::State,
            Self::EnvelopeMissing
            | Self::OutOfScopeMutation
            | Self::GuardDenied
            | Self::ReplanRequired => CepErrorFamily::Guard,
            Self::EffectViolation => CepErrorFamily::Effect,
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
            -32021 => Self::CommandTimeout,
            -32022 => Self::ChildKillFailed,
            -32030 => Self::StaleContext,
            -32031 => Self::ResourceConflict,
            -32032 => Self::EnvelopeAlreadyActive,
            -32033 => Self::DuplicateRegistration,
            -32040 => Self::EnvelopeMissing,
            -32041 => Self::OutOfScopeMutation,
            -32042 => Self::GuardDenied,
            -32043 => Self::ReplanRequired,
            -32050 => Self::EffectViolation,
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
    pub active_envelope_id: Option<String>,
    pub granted_capabilities: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ExecutionErrorData {
    pub command: String,
    pub exit_code: Option<i32>,
    pub stderr: Option<String>,
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct EffectErrorData {
    pub method: String,
    pub declared_effect: Vec<ImpactKind>,
    pub authorized_effect: Vec<ImpactKind>,
    pub observed_effects: Vec<ImpactKind>,
    pub violations: Vec<String>,
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
    State(StateErrorData),
    Guard(GuardErrorData),
    Effect(Box<EffectErrorData>),
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

    pub fn internal_error(message: impl Into<String>) -> Self {
        Self {
            code: CepErrorCode::ExecutionFailed,
            message: message.into(),
            data: CepErrorData::Execution(ExecutionErrorData {
                command: String::new(),
                exit_code: None,
                stderr: None,
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
            CepErrorFamily::Effect => (-32059, -32050),
            CepErrorFamily::Reserved => (-32099, -32060),
        }
    }

    #[test]
    fn every_family_has_code_in_range() {
        let codes = [
            CepErrorCode::SchemaMismatch,
            CepErrorCode::CapabilityNotGranted,
            CepErrorCode::ExecutionFailed,
            CepErrorCode::StaleContext,
            CepErrorCode::EnvelopeMissing,
            CepErrorCode::EffectViolation,
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
                    active_envelope_id: None,
                    granted_capabilities: vec![],
                }),
            ),
            (
                CepErrorCode::ExecutionFailed,
                CepErrorData::Execution(ExecutionErrorData {
                    command: "ls".into(),
                    exit_code: Some(1),
                    stderr: None,
                }),
            ),
            (
                CepErrorCode::StaleContext,
                CepErrorData::State(StateErrorData {
                    resource: "session".into(),
                    reason: "stale".into(),
                }),
            ),
            (
                CepErrorCode::EnvelopeMissing,
                CepErrorData::Guard(GuardErrorData {
                    method: "mutate.create".into(),
                    reason: "no envelope".into(),
                }),
            ),
            (
                CepErrorCode::EffectViolation,
                CepErrorData::Effect(Box::new(EffectErrorData {
                    method: "observe.workspace".into(),
                    declared_effect: vec![ImpactKind::WorkspaceObservation],
                    authorized_effect: vec![ImpactKind::WorkspaceObservation],
                    observed_effects: vec![
                        ImpactKind::WorkspaceObservation,
                        ImpactKind::WorkspaceMutation,
                    ],
                    violations: vec![
                        "observed workspace_mutation not in declared effect ceiling".into(),
                    ],
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
                id: crate::jsonrpc::Id::Number(1),
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
    fn retriability_present_after_round_trip() {
        let err = CepError {
            code: CepErrorCode::ExecutionFailed,
            message: "fail".into(),
            data: CepErrorData::Execution(ExecutionErrorData {
                command: "test".into(),
                exit_code: None,
                stderr: None,
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
            CepErrorCode::CommandTimeout,
            CepErrorCode::ChildKillFailed,
            CepErrorCode::StaleContext,
            CepErrorCode::ResourceConflict,
            CepErrorCode::EnvelopeAlreadyActive,
            CepErrorCode::DuplicateRegistration,
            CepErrorCode::EnvelopeMissing,
            CepErrorCode::OutOfScopeMutation,
            CepErrorCode::GuardDenied,
            CepErrorCode::ReplanRequired,
            CepErrorCode::EffectViolation,
            CepErrorCode::Reserved,
        ];
        for code in all_codes {
            let i = code.as_i32();
            assert!((-32099..=-32001).contains(&i), "code {i} not in CEP range");
            assert!(i.abs() >= 32001, "no 1000-series codes: {i}");
        }
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
    fn effect_error_data_carries_rich_violation_evidence() {
        use crate::effect_types::ImpactKind;

        let data = EffectErrorData {
            method: "observe.workspace".into(),
            declared_effect: vec![ImpactKind::WorkspaceObservation],
            authorized_effect: vec![ImpactKind::WorkspaceObservation],
            observed_effects: vec![
                ImpactKind::WorkspaceObservation,
                ImpactKind::WorkspaceMutation,
            ],
            violations: vec!["observed workspace_mutation not in declared effect ceiling".into()],
        };

        let wrapped = CepErrorData::Effect(Box::new(data.clone()));
        let value = serde_json::to_value(&wrapped).unwrap();
        assert_eq!(value["kind"], "effect");
        assert_eq!(value["method"], "observe.workspace");
        assert!(value["declared_effect"].is_array());
        assert!(value["authorized_effect"].is_array());
        assert!(value["observed_effects"].is_array());
        assert!(value["violations"].is_array());
        assert_eq!(
            value["violations"][0],
            "observed workspace_mutation not in declared effect ceiling"
        );

        let serialized = serde_json::to_string(&wrapped).unwrap();
        let deserialized: CepErrorData = serde_json::from_str(&serialized).unwrap();
        assert_eq!(deserialized, wrapped);
    }
}
