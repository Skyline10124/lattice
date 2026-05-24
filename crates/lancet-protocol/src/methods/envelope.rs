//! Envelope declaration methods.

use crate::envelope::{ExecutionEnvelope, ImpactKind};
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
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct DeclareResult {
    pub envelope_id: String,
    pub accepted: bool,
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
