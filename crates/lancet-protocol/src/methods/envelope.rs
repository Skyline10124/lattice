//! Envelope declaration methods.

use crate::envelope::{EffectDecl, EffectKindSet, EffectScopeKind, ExecutionEnvelope, ImpactKind};
use crate::method::{CatalogMethod, CepMethod, RequiredCapability};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// `envelope.declare`
///
/// Declares an execution envelope for subsequent mutation/command methods.
/// Active-envelope storage is not implemented in Phase 19 — this method only
/// defines the wire shape and catalog metadata for Phase 22 guard integration.
pub struct Declare;

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

impl CepMethod for Declare {
    const NAME: &'static str = "envelope.declare";
    const DESCRIPTION: &'static str =
        "Declare an execution envelope granting capabilities for subsequent controlled operations.";
    type Params = DeclareParams;
    type Result = DeclareResult;
    // Session-state mutation: declaring an envelope updates guard/session context.
    // Full active-envelope storage lands in Phase 22.
    const IMPACT: ImpactKind = ImpactKind::SessionStateMutation;
    const CAPABILITY: RequiredCapability = RequiredCapability::MutateSessionState;
    const EFFECT: EffectDecl = EffectDecl {
        kinds: EffectKindSet::from_static(&[ImpactKind::SessionStateMutation]),
        scope: EffectScopeKind::SessionState,
        capabilities: &[RequiredCapability::MutateSessionState],
    };
}

impl CatalogMethod for Declare {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelope_declare_name_is_dotted() {
        assert_eq!(Declare::NAME, "envelope.declare");
        assert!(Declare::NAME.contains('.'));
        assert!(!Declare::NAME.contains('/'));
    }
}
