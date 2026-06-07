// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Envelope declaration methods.

use crate::method::{BaseAuthorityRequirement, CatalogMethod, CepMethod};
use lancet_protocol::RequiredCapability;
use lancet_protocol::{
    EffectDecl, EffectKindSet, EffectScopeKind, ExecutionEnvelope, FailureSemantics, ImpactKind,
    OverlayBehavior,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct DeclareParams {
    pub envelope: ExecutionEnvelope,
    /// When true, atomically replaces the active envelope if one exists.
    #[serde(default)]
    pub supersede: bool,
    /// Optional reason for superseding the active envelope.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct DeclareResult {
    pub envelope_id: String,
    /// Hash of the canonical envelope JSON content.
    pub envelope_hash: String,
    pub accepted: bool,
    /// The envelope_id of the superseded envelope, if supersede was true.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub superseded_envelope_id: Option<String>,
}

/// `session.envelope`
pub struct Envelope;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum EnvelopeParams {
    Declare(DeclareParams),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "mode", content = "result", rename_all = "snake_case")]
pub enum EnvelopeResult {
    Declare(DeclareResult),
}

impl CepMethod for Envelope {
    const NAME: &'static str = "session.envelope";
    const DESCRIPTION: &'static str = "Declare an execution envelope, mutating the active session authorization state. The caller supplies the explicit envelope capabilities, scope, impact, constraints, and optional verification.";
    type Params = EnvelopeParams;
    type Result = EnvelopeResult;
    const IMPACT: ImpactKind = ImpactKind::SessionStateMutation;
    const CAPABILITY: RequiredCapability = RequiredCapability::MutateSessionState;
    const BASE_AUTHORITY: BaseAuthorityRequirement = BaseAuthorityRequirement::SESSION_STATE;
    const EFFECT: EffectDecl = EffectDecl {
        kinds: EffectKindSet::from_static(&[ImpactKind::SessionStateMutation]),
        scope: EffectScopeKind::SessionState,
        capabilities: &[RequiredCapability::MutateSessionState],
        overlay: OverlayBehavior::SessionState,
        failure: FailureSemantics::NoBackendEffectOnFailure,
    };
}

impl CatalogMethod for Envelope {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelope_name_is_dotted() {
        assert_eq!(Envelope::NAME, "session.envelope");
        assert!(Envelope::NAME.contains('.'));
        assert!(!Envelope::NAME.contains('/'));
    }
}
