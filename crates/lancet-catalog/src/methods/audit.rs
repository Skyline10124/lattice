// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Read-only audit provenance methods (`observe.audit`, `observe.audit_replay`).

use crate::method::{BaseAuthorityRequirement, CatalogMethod, CepMethod};
use lancet_protocol::RequiredCapability;
use lancet_protocol::provenance::{InTotoStatement, ProvGraphFragment, TurnExecutionTrace};
use lancet_protocol::{
    EffectDecl, EffectKindSet, EffectScopeKind, FailureSemantics, ImpactKind, OverlayBehavior,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AuditQueryParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub impact_kind: Option<ImpactKind>,
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
    /// Override session scope for tests; production handlers default from turn context.
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

/// `observe.audit` — query audit rows, optionally streaming tail rows.
pub struct AuditObserve;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AuditObserveParams {
    #[serde(default)]
    pub stream: bool,
    #[serde(flatten)]
    pub query: AuditQueryParams,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "mode", content = "result", rename_all = "snake_case")]
pub enum AuditObserveResult {
    Query(AuditQueryResult),
    Tail(AuditTailResult),
}

impl CepMethod for AuditObserve {
    const NAME: &'static str = "observe.audit";
    const DESCRIPTION: &'static str = "Observe CEP audit rows. With stream=false, returns a PROV graph fragment for matching rows; with stream=true, emits tail row notifications from since_audit_id and returns the last cursor.";
    type Params = AuditObserveParams;
    type Result = AuditObserveResult;
    const IMPACT: ImpactKind = ImpactKind::AuditObservation;
    const CAPABILITY: RequiredCapability = RequiredCapability::ReadAudit;
    const BASE_AUTHORITY: BaseAuthorityRequirement = BaseAuthorityRequirement::READ_AUDIT;
    const EFFECT: EffectDecl = EffectDecl {
        kinds: EffectKindSet::from_static(&[ImpactKind::AuditObservation]),
        scope: EffectScopeKind::SessionState,
        capabilities: &[RequiredCapability::ReadAudit],
        overlay: OverlayBehavior::AuditRead,
        failure: FailureSemantics::NoBackendEffectOnFailure,
    };
}

impl CatalogMethod for AuditObserve {}

/// `observe.audit_replay` — structural turn trace from audit rows alone.
pub struct AuditReplay;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AuditReplayParams {
    pub turn_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AuditReplayResult {
    pub trace: TurnExecutionTrace,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub statement: Option<InTotoStatement>,
}

impl CepMethod for AuditReplay {
    const NAME: &'static str = "observe.audit_replay";
    const DESCRIPTION: &'static str =
        "Reconstruct a structural turn execution trace and optional attestation from audit rows.";
    type Params = AuditReplayParams;
    type Result = AuditReplayResult;
    const IMPACT: ImpactKind = ImpactKind::AuditObservation;
    const CAPABILITY: RequiredCapability = RequiredCapability::ReadAudit;
    const BASE_AUTHORITY: BaseAuthorityRequirement = BaseAuthorityRequirement::READ_AUDIT;
    const EFFECT: EffectDecl = EffectDecl {
        kinds: EffectKindSet::from_static(&[ImpactKind::AuditObservation]),
        scope: EffectScopeKind::SessionState,
        capabilities: &[RequiredCapability::ReadAudit],
        overlay: OverlayBehavior::AuditRead,
        failure: FailureSemantics::NoBackendEffectOnFailure,
    };
}

impl CatalogMethod for AuditReplay {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::catalog;

    #[test]
    fn audit_method_names_are_dotted() {
        for name in [AuditObserve::NAME, AuditReplay::NAME] {
            assert!(name.contains('.'));
            assert!(!name.contains('/'));
        }
    }

    #[test]
    fn audit_methods_use_audit_observation_and_read_audit() {
        assert_eq!(AuditObserve::IMPACT, ImpactKind::AuditObservation);
        assert_eq!(AuditObserve::CAPABILITY, RequiredCapability::ReadAudit);
        assert_eq!(AuditReplay::CAPABILITY, RequiredCapability::ReadAudit);
    }

    #[test]
    fn audit_methods_are_in_catalog() {
        let names: Vec<_> = catalog().iter().map(|d| d.name).collect();
        assert!(names.contains(&"observe.audit"));
        assert!(names.contains(&"observe.audit_replay"));
    }
}
