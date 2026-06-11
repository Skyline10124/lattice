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
    const DESCRIPTION: &'static str = "Declare an execution envelope, replacing the active session authorization state with the supplied capabilities, scope, impact, constraints, and optional verification. Mutating methods (mutate.create/edit/batch and workspace-mutating exec.command_run) require an active envelope whose capabilities already cover the target before the call: declare first, and re-declare when you start a new change — the active envelope is replaced per declaration, not accumulated, so authority from a previous intent does not carry over. WritePath pattern syntax (project-root-relative): end a pattern with /** to authorize a directory and everything under it (e.g. 'crates/foo/**'); a pattern WITHOUT /** matches only that one exact file path (e.g. 'crates/foo/lib.rs'), so 'crates/foo' alone will NOT cover files inside it. '..', absolute paths, and glob metacharacters other than a trailing /** are rejected. RunCommand grants argv by token prefix (e.g. 'cargo test' covers 'cargo test --release'); for an inline shell every expanded sub-command must be covered. FetchUrl grants one host.";
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

    #[test]
    fn envelope_description_guides_writepath_and_declaration_order() {
        // The two highest-friction agent mistakes: not declaring before
        // mutating, and writing a directory pattern without `/**`.
        assert!(Envelope::DESCRIPTION.contains("declare first"));
        assert!(Envelope::DESCRIPTION.contains("replaced per declaration"));
        assert!(Envelope::DESCRIPTION.contains("/**"));
        assert!(Envelope::DESCRIPTION.contains("one exact file path"));
    }
}
