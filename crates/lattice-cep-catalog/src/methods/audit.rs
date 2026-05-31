//! Read-only audit provenance methods (`audit.query`, `audit.tail`, `audit.replay`).

use crate::method::{CatalogMethod, CepMethod};
use lattice_cep_protocol::provenance::{InTotoStatement, ProvGraphFragment, TurnExecutionTrace};
use lattice_cep_protocol::RequiredCapability;
use lattice_cep_protocol::{EffectDecl, EffectKindSet, EffectScopeKind, ImpactKind};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// `audit.query` — filtered PROV graph projection over audit rows.
pub struct AuditQuery;

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
    pub envelope_id: Option<String>,
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

impl CepMethod for AuditQuery {
    const NAME: &'static str = "audit.query";
    const DESCRIPTION: &'static str =
        "Query the CEP audit store and return a PROV-DM graph fragment for matching rows.";
    type Params = AuditQueryParams;
    type Result = AuditQueryResult;
    const IMPACT: ImpactKind = ImpactKind::AuditObservation;
    const CAPABILITY: RequiredCapability = RequiredCapability::ReadAudit;
    const EFFECT: EffectDecl = EffectDecl {
        kinds: EffectKindSet::from_static(&[ImpactKind::AuditObservation]),
        scope: EffectScopeKind::SessionState,
        capabilities: &[RequiredCapability::ReadAudit],
    };
}

impl CatalogMethod for AuditQuery {}

/// `audit.tail` — stream new audit rows as notifications.
pub struct AuditTail;

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

impl CepMethod for AuditTail {
    const NAME: &'static str = "audit.tail";
    const DESCRIPTION: &'static str =
        "Tail the CEP audit store from a cursor, emitting row notifications until caught up.";
    type Params = AuditTailParams;
    type Result = AuditTailResult;
    const IMPACT: ImpactKind = ImpactKind::AuditObservation;
    const CAPABILITY: RequiredCapability = RequiredCapability::ReadAudit;
    const EFFECT: EffectDecl = EffectDecl {
        kinds: EffectKindSet::from_static(&[ImpactKind::AuditObservation]),
        scope: EffectScopeKind::SessionState,
        capabilities: &[RequiredCapability::ReadAudit],
    };
}

impl CatalogMethod for AuditTail {}

/// `audit.replay` — structural turn trace from audit rows alone.
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
    const NAME: &'static str = "audit.replay";
    const DESCRIPTION: &'static str =
        "Reconstruct a structural turn execution trace and optional attestation from audit rows.";
    type Params = AuditReplayParams;
    type Result = AuditReplayResult;
    const IMPACT: ImpactKind = ImpactKind::AuditObservation;
    const CAPABILITY: RequiredCapability = RequiredCapability::ReadAudit;
    const EFFECT: EffectDecl = EffectDecl {
        kinds: EffectKindSet::from_static(&[ImpactKind::AuditObservation]),
        scope: EffectScopeKind::SessionState,
        capabilities: &[RequiredCapability::ReadAudit],
    };
}

impl CatalogMethod for AuditReplay {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::catalog;

    #[test]
    fn audit_method_names_are_dotted() {
        for name in [AuditQuery::NAME, AuditTail::NAME, AuditReplay::NAME] {
            assert!(name.contains('.'));
            assert!(!name.contains('/'));
        }
    }

    #[test]
    fn audit_methods_use_audit_observation_and_read_audit() {
        assert_eq!(AuditQuery::IMPACT, ImpactKind::AuditObservation);
        assert_eq!(AuditQuery::CAPABILITY, RequiredCapability::ReadAudit);
        assert_eq!(AuditTail::IMPACT, ImpactKind::AuditObservation);
        assert_eq!(AuditReplay::CAPABILITY, RequiredCapability::ReadAudit);
    }

    #[test]
    fn audit_methods_are_in_catalog() {
        let names: Vec<_> = catalog().iter().map(|d| d.name).collect();
        assert!(names.contains(&"audit.query"));
        assert!(names.contains(&"audit.tail"));
        assert!(names.contains(&"audit.replay"));
    }
}
