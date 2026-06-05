// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Feature-gated `workspace.inspect` / `workspace.navigate` scaffold (OBS-05).

use crate::method::CepMethod;
use lancet_protocol::RequiredCapability;
use lancet_protocol::{
    EffectDecl, EffectKindSet, EffectScopeKind, FailureSemantics, ImpactKind, OverlayBehavior,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Typed scaffold stub result shared by inspect/navigate stubs.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ScaffoldStubResult {
    pub status: String,
    pub semantic_stage: String,
    pub routed: bool,
    pub message: String,
}

impl ScaffoldStubResult {
    pub fn not_yet_routed(method: &str) -> Self {
        Self {
            status: "not_yet_routed".to_string(),
            semantic_stage: "scaffold".to_string(),
            routed: false,
            message: format!(
                "{method} is registered as a CEP scaffold stub and is not routed in v1.3-alpha"
            ),
        }
    }
}

/// `workspace.inspect` scaffold (not in default `CATALOG`).
pub struct Inspect;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct InspectParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

impl CepMethod for Inspect {
    const NAME: &'static str = "workspace.inspect";
    const DESCRIPTION: &'static str =
        "Scaffold stub for future workspace inspection routing (not yet routed).";
    type Params = InspectParams;
    type Result = ScaffoldStubResult;
    const IMPACT: ImpactKind = ImpactKind::WorkspaceObservation;
    const CAPABILITY: RequiredCapability = RequiredCapability::Inspect;
    const EFFECT: EffectDecl = EffectDecl {
        kinds: EffectKindSet::from_static(&[ImpactKind::WorkspaceObservation]),
        scope: EffectScopeKind::PathRead,
        capabilities: &[RequiredCapability::Inspect],
        overlay: OverlayBehavior::ReadWorkspace,
        failure: FailureSemantics::NoBackendEffectOnFailure,
    };
}

/// `workspace.navigate` scaffold (not in default `CATALOG`).
pub struct Navigate;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct NavigateParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
}

impl CepMethod for Navigate {
    const NAME: &'static str = "workspace.navigate";
    const DESCRIPTION: &'static str =
        "Scaffold stub for future workspace navigation routing (not yet routed).";
    type Params = NavigateParams;
    type Result = ScaffoldStubResult;
    const IMPACT: ImpactKind = ImpactKind::WorkspaceObservation;
    const CAPABILITY: RequiredCapability = RequiredCapability::Inspect;
    const EFFECT: EffectDecl = EffectDecl {
        kinds: EffectKindSet::from_static(&[ImpactKind::WorkspaceObservation]),
        scope: EffectScopeKind::PathRead,
        capabilities: &[RequiredCapability::Inspect],
        overlay: OverlayBehavior::ReadWorkspace,
        failure: FailureSemantics::NoBackendEffectOnFailure,
    };
}

#[cfg(feature = "inspect-scaffold")]
mod sealed {
    use super::{Inspect, Navigate};
    use crate::method::sealed::Sealed;

    impl Sealed for Inspect {}
    impl Sealed for Navigate {}
}

#[cfg(feature = "inspect-scaffold")]
use crate::method::CatalogMethod;

#[cfg(feature = "inspect-scaffold")]
impl CatalogMethod for Inspect {}

#[cfg(feature = "inspect-scaffold")]
impl CatalogMethod for Navigate {}
