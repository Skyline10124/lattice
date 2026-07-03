// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Operator audit read DTOs (query/tail/replay). Not agent catalog methods.

use lancet_protocol::provenance::{InTotoStatement, ProvGraphFragment, TurnExecutionTrace};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AuditQueryParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub impact_kind: Option<lancet_protocol::effect_types::ImpactKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capability: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verdict: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since_audit_id: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub until_audit_id: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AuditQueryResult {
    pub fragment: ProvGraphFragment,
    pub row_count: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AuditTailParams {
    #[serde(default)]
    pub since_audit_id: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AuditTailResult {
    pub last_audit_id: i64,
    pub done: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AuditReplayParams {
    pub turn_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AuditReplayResult {
    pub trace: TurnExecutionTrace,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub statement: Option<InTotoStatement>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stored_statement_match: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_audit_range_or_hash: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audit_replay_params_require_turn_id_field() {
        let params = AuditReplayParams {
            turn_id: "turn-a".into(),
        };
        assert_eq!(params.turn_id, "turn-a");
    }
}