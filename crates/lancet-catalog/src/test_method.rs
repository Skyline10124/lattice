// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! A synthetic [`CepMethod`] for tests that need *any* typed method.
//!
//! Before #29, `session.todo_write` doubled as this fixture: it was the one
//! catalog method with a trivial params/result contract and no workspace,
//! network, or session dependency, so dispatch, transport, and audit tests
//! reached for it whenever they needed a typed method to register. Migrating it
//! to a Component Tool removed the type, and those tests do not want a *catalog*
//! method at all — they want a shape to exercise the generic machinery.
//!
//! [`TestEcho`] is that shape, and it is deliberately not a
//! [`CatalogMethod`](crate::method::CatalogMethod): it is unsealed, absent from
//! [`catalog`](crate::catalog), and named under a `test.` prefix no production
//! surface serves. A test using it cannot accidentally assert something about
//! the shipped catalog.

use crate::method::{CepMethod, RequiredCapability};
use lancet_protocol::{
    EffectDecl, EffectKindSet, EffectScopeKind, FailureSemantics, ImpactKind, OverlayBehavior,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// `test.echo` — a typed method that echoes its items back.
pub struct TestEcho;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TestEchoStatus {
    Pending,
    InProgress,
    Completed,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TestEchoItem {
    pub content: String,
    pub status: TestEchoStatus,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TestEchoParams {
    pub items: Vec<TestEchoItem>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TestEchoResult {
    pub items: Vec<TestEchoItem>,
}

// `MethodRegistry::register_method` takes a `CatalogMethod`, so the fixture must
// be one to be registrable at all. Sealing lives in this crate, so the impl is
// legal here and nowhere else — a downstream crate still cannot invent a catalog
// method. Membership in `CatalogMethod` is *not* membership in `catalog()`: the
// static list is separate, and the test above pins that `test.echo` is absent.
impl crate::method::sealed::Sealed for TestEcho {}
impl crate::method::CatalogMethod for TestEcho {}

impl CepMethod for TestEcho {
    const NAME: &'static str = "test.echo";
    const DESCRIPTION: &'static str = "Test-only typed method: echoes its items.";
    type Params = TestEchoParams;
    type Result = TestEchoResult;
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The fixture must never be mistaken for a shipped Tool.
    #[test]
    fn test_echo_is_not_in_the_catalog() {
        assert!(
            crate::catalog()
                .iter()
                .all(|descriptor| descriptor.name != TestEcho::NAME)
        );
    }
}
