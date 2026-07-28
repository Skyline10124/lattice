// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use std::path::PathBuf;

use serde::Serialize;
use tempfile::TempDir;

use super::support::{effect_contract_harness, rt};
use lancet_authority::effect::ledger::ObservedEffect;
use lancet_authority::effect::region::AuthorizedEffectRegion;
use lancet_catalog::CatalogMethod;
use lancet_protocol::effect_types::ImpactKind;

pub(super) fn test_registry() -> crate::MethodRegistry {
    let mut registry = crate::MethodRegistry::new();
    crate::register_production_method_registry(&mut registry).unwrap();
    registry
}

/// No-op: envelope declaration is no longer needed in direct authority mode.
pub(super) fn declare_active_envelope(_ctx: &crate::server::registry::HandlerContext, _root: &str) {
    // Envelope authority removed; this is now a no-op.
}

pub(super) fn observation_region(_root: &str) -> AuthorizedEffectRegion {
    let mut kinds = std::collections::BTreeSet::new();
    kinds.insert(ImpactKind::WorkspaceObservation);
    AuthorizedEffectRegion {
        kinds,
        read_paths: vec![PathBuf::from(".")],
        observation_roots: vec![PathBuf::from(".")],
        denied_write_paths: Vec::new(),
        denied_write_path_prefixes: Vec::new(),
        hosts: Vec::new(),
        session_state_allowed: false,
        command_allowed: false,
    }
}

pub(super) fn mutation_region(_root: &str) -> AuthorizedEffectRegion {
    let mut kinds = std::collections::BTreeSet::new();
    kinds.insert(ImpactKind::WorkspaceMutation);
    AuthorizedEffectRegion {
        kinds,
        read_paths: Vec::new(),
        observation_roots: Vec::new(),
        denied_write_paths: Vec::new(),
        // Blacklist semantics: empty deny list → all workspace paths allowed
        denied_write_path_prefixes: Vec::new(),
        hosts: Vec::new(),
        session_state_allowed: false,
        command_allowed: false,
    }
}

pub(super) fn setup_tempdir_with_files() -> TempDir {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("hello.txt"), "hello\nworld\n").unwrap();
    std::fs::write(dir.path().join("data.txt"), "some data content\n").unwrap();
    std::fs::create_dir_all(dir.path().join("subdir")).unwrap();
    std::fs::write(dir.path().join("subdir/nested.txt"), "nested\n").unwrap();
    dir
}

#[derive(Debug)]
pub(super) struct RuntimeEffectScenario {
    pub(super) name: &'static str,
    pub(super) effects: Vec<ObservedEffect>,
}

pub(super) fn scenario(name: &'static str, effects: Vec<ObservedEffect>) -> RuntimeEffectScenario {
    RuntimeEffectScenario { name, effects }
}

#[derive(Clone, Copy, Debug)]
enum DispatchExpectation {
    Success,
    Failure,
}

fn dispatch_effects<M>(
    params: M::Params,
    ctx: crate::server::registry::HandlerContext,
    authority: AuthorizedEffectRegion,
    expectation: DispatchExpectation,
) -> Vec<ObservedEffect>
where
    M: CatalogMethod,
    M::Params: Serialize,
{
    let registry = test_registry();
    let outcome = rt().block_on(effect_contract_harness::<M>(
        &registry, params, ctx, authority,
    ));
    match expectation {
        DispatchExpectation::Success => {
            outcome
                .dispatch_result
                .as_ref()
                .unwrap_or_else(|err| panic!("{} fixture dispatch failed: {err:?}", M::NAME));
        }
        DispatchExpectation::Failure => {
            assert!(
                outcome.dispatch_result.is_err(),
                "{} fixture was expected to fail",
                M::NAME
            );
        }
    }
    outcome.observed_effects
}

pub(super) fn dispatch_success_effects<M>(
    params: M::Params,
    ctx: crate::server::registry::HandlerContext,
    authority: AuthorizedEffectRegion,
) -> Vec<ObservedEffect>
where
    M: CatalogMethod,
    M::Params: Serialize,
{
    dispatch_effects::<M>(params, ctx, authority, DispatchExpectation::Success)
}

pub(super) fn dispatch_failure_effects<M>(
    params: M::Params,
    ctx: crate::server::registry::HandlerContext,
    authority: AuthorizedEffectRegion,
) -> Vec<ObservedEffect>
where
    M: CatalogMethod,
    M::Params: Serialize,
{
    dispatch_effects::<M>(params, ctx, authority, DispatchExpectation::Failure)
}
