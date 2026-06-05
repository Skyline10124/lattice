// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Envelope declaration methods.

use crate::method::{CatalogMethod, CepMethod};
use lancet_protocol::RequiredCapability;
use lancet_protocol::{
    EffectDecl, EffectKindSet, EffectScopeKind, ExecutionEnvelope, FailureSemantics, Impact,
    ImpactKind, OverlayBehavior,
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

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SubmitIntentParams {
    pub action: String,
    pub target: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<serde_json::Value>,
    #[serde(default)]
    pub supersede: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Guard Layer 2 status for the submitted intent.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum L2Status {
    /// Layer 2 AST analysis produced category coverage.
    Covered { categories: Vec<String> },
    /// Layer 2 guard is not implemented for the target method.
    NotImplemented { method: String },
    /// Layer 2 guard is not applicable (e.g., observation-only methods).
    NotApplicable,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SubmitIntentResult {
    pub envelope_id: String,
    /// Hash of the canonical envelope JSON content.
    pub envelope_hash: String,
    pub accepted: bool,
    /// The enriched Impact with scope, confidence, and unknowns from the analyzer.
    pub impact: Impact,
    /// IDs of invariant cards that matched the target.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub matched_cards: Vec<String>,
    /// Guard Layer 2 AST coverage for mutation methods.
    pub l2_coverage: L2Status,
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
    DeriveIntent(SubmitIntentParams),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "mode", content = "result", rename_all = "snake_case")]
pub enum EnvelopeResult {
    Declare(DeclareResult),
    DeriveIntent(SubmitIntentResult),
}

impl CepMethod for Envelope {
    const NAME: &'static str = "session.envelope";
    const DESCRIPTION: &'static str = "Declare or derive an execution envelope, mutating the active session authorization state. Use mode=declare for an explicit envelope, or mode=derive_intent to derive one from natural intent fields.";
    type Params = EnvelopeParams;
    type Result = EnvelopeResult;
    const IMPACT: ImpactKind = ImpactKind::SessionStateMutation;
    const CAPABILITY: RequiredCapability = RequiredCapability::MutateSessionState;
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
