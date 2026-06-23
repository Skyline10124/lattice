// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use std::collections::BTreeSet;
use std::path::PathBuf;

use serde::Serialize;
use tempfile::TempDir;

use super::support::{effect_contract_harness, rt};
use lancet_authority::effect::ledger::ObservedEffect;
use lancet_authority::effect::region::AuthorizedEffectRegion;
use lancet_catalog::CatalogMethod;
use lancet_protocol::effect_types::ImpactKind;

use lancet_protocol::policy::{Capability, Constraints, ExecutionEnvelope, Impact as EnvImpact, ImpactConfidence, Intent, Scope as EnvScope, VerifyCommand};
use lancet_protocol::policy::{CapabilitySource, SourcedCapability};

pub(super) fn test_registry() -> crate::MethodRegistry {
    let mut registry = crate::MethodRegistry::new();
    crate::register_production_method_registry(&mut registry).unwrap();
    registry
}

pub(super) fn read_cap(root: &str) -> SourcedCapability {
    SourcedCapability {
        capability: Capability::ReadPath {
            path_pattern: root.to_string(),
        },
        source: CapabilitySource::Base,
    }
}

pub(super) fn inspect_cap() -> SourcedCapability {
    SourcedCapability {
        capability: Capability::Inspect,
        source: CapabilitySource::Base,
    }
}

pub(super) fn write_cap(_root: &str) -> SourcedCapability {
    SourcedCapability {
        capability: Capability::WritePath {
            path_pattern: ".".to_string(),
        },
        source: CapabilitySource::Declared {
            envelope_id: "test-envelope".into(),
        },
    }
}

pub(super) fn run_cap() -> SourcedCapability {
    run_cap_with(vec!["echo".into(), "touch".into()])
}

pub(super) fn run_cap_with(argv_allowlist: Vec<String>) -> SourcedCapability {
    SourcedCapability {
        capability: Capability::RunCommand { argv_allowlist },
        source: CapabilitySource::Declared {
            envelope_id: "test-envelope".into(),
        },
    }
}

pub(super) fn test_mutation_envelope(root: &str) -> ExecutionEnvelope {
    test_mutation_envelope_with(root, ".", vec!["echo".into(), "touch".into()], None)
}

pub(super) fn test_command_envelope(root: &str, argv_allowlist: Vec<String>) -> ExecutionEnvelope {
    let mut caps = BTreeSet::new();
    caps.insert(Capability::RunCommand { argv_allowlist });
    ExecutionEnvelope {
        intent: Intent {
            description: "test command envelope".into(),
        },
        capabilities: caps,
        scope: EnvScope {
            workspace_root: Some(root.into()),
            extensions: serde_json::Map::new(),
        },
        impact: vec![EnvImpact {
            kind: ImpactKind::CommandExecution,
            target: root.into(),
            scope: None,
            confidence: ImpactConfidence::Conservative,
            unknowns: vec![],
        }],
        constraints: Constraints::default(),
        verify: None,
        on_failure: None,
        extensions: serde_json::Map::new(),
    }
}

pub(super) fn test_mutation_envelope_with(
    root: &str,
    write_path_pattern: &str,
    argv_allowlist: Vec<String>,
    verify: Option<VerifyCommand>,
) -> ExecutionEnvelope {
    let mut caps = BTreeSet::new();
    caps.insert(Capability::WritePath {
        path_pattern: write_path_pattern.to_string(),
    });
    caps.insert(Capability::RunCommand { argv_allowlist });
    ExecutionEnvelope {
        intent: Intent {
            description: "test mutation envelope".into(),
        },
        capabilities: caps,
        scope: EnvScope {
            workspace_root: Some(root.into()),
            extensions: serde_json::Map::new(),
        },
        impact: vec![EnvImpact {
            kind: ImpactKind::WorkspaceMutation,
            target: root.into(),
            scope: None,
            confidence: ImpactConfidence::Conservative,
            unknowns: vec![],
        }],
        constraints: Constraints::default(),
        verify,
        on_failure: None,
        extensions: serde_json::Map::new(),
    }
}

pub(super) fn declare_active_envelope(ctx: &crate::server::registry::HandlerContext, root: &str) {
    let envelope = test_mutation_envelope(root);
    declare_envelope(ctx, envelope);
}

pub(super) fn declare_envelope(
    ctx: &crate::server::registry::HandlerContext,
    envelope: ExecutionEnvelope,
) {
    rt().block_on(ctx.active_envelope.declare(envelope, 0, "test-turn"))
        .unwrap();
}

pub(super) fn session_cap() -> SourcedCapability {
    SourcedCapability {
        capability: Capability::MutateSessionState,
        source: CapabilitySource::Base,
    }
}

pub(super) fn audit_cap() -> SourcedCapability {
    SourcedCapability {
        capability: Capability::ReadAudit,
        source: CapabilitySource::Base,
    }
}

pub(super) fn observation_region(root: &str) -> AuthorizedEffectRegion {
    let mut kinds = std::collections::BTreeSet::new();
    kinds.insert(ImpactKind::WorkspaceObservation);
    AuthorizedEffectRegion {
        kinds,
        read_paths: vec![PathBuf::from(root)],
        observation_roots: vec![PathBuf::from(root)],
        write_paths: Vec::new(),
        write_path_prefixes: Vec::new(),
        hosts: Vec::new(),
        session_state_allowed: false,
        command_allowed: false,
    }
}

pub(super) fn mutation_region(root: &str) -> AuthorizedEffectRegion {
    let mut kinds = std::collections::BTreeSet::new();
    kinds.insert(ImpactKind::WorkspaceMutation);
    AuthorizedEffectRegion {
        kinds,
        read_paths: Vec::new(),
        observation_roots: Vec::new(),
        write_paths: Vec::new(),
        write_path_prefixes: vec![PathBuf::from(root)],
        hosts: Vec::new(),
        session_state_allowed: false,
        command_allowed: false,
    }
}

pub(super) fn command_region() -> AuthorizedEffectRegion {
    let mut kinds = std::collections::BTreeSet::new();
    kinds.insert(ImpactKind::CommandExecution);
    AuthorizedEffectRegion {
        kinds,
        read_paths: Vec::new(),
        observation_roots: Vec::new(),
        write_paths: Vec::new(),
        write_path_prefixes: Vec::new(),
        hosts: Vec::new(),
        session_state_allowed: false,
        command_allowed: true,
    }
}

pub(super) fn command_mutation_region(root: &str) -> AuthorizedEffectRegion {
    command_mutation_region_with_write_prefixes(root, vec![PathBuf::from(root)])
}

pub(super) fn command_mutation_region_with_write_prefixes(
    _root: &str,
    write_path_prefixes: Vec<PathBuf>,
) -> AuthorizedEffectRegion {
    let mut kinds = BTreeSet::new();
    kinds.insert(ImpactKind::CommandExecution);
    kinds.insert(ImpactKind::WorkspaceMutation);
    AuthorizedEffectRegion {
        kinds,
        read_paths: Vec::new(),
        observation_roots: Vec::new(),
        write_paths: Vec::new(),
        write_path_prefixes,
        hosts: Vec::new(),
        session_state_allowed: false,
        command_allowed: true,
    }
}

pub(super) fn session_region() -> AuthorizedEffectRegion {
    let mut kinds = BTreeSet::new();
    kinds.insert(ImpactKind::SessionStateMutation);
    AuthorizedEffectRegion {
        kinds,
        read_paths: Vec::new(),
        observation_roots: Vec::new(),
        write_paths: Vec::new(),
        write_path_prefixes: Vec::new(),
        hosts: Vec::new(),
        session_state_allowed: true,
        command_allowed: false,
    }
}

pub(super) fn audit_region() -> AuthorizedEffectRegion {
    let mut kinds = BTreeSet::new();
    kinds.insert(ImpactKind::AuditObservation);
    AuthorizedEffectRegion {
        kinds,
        read_paths: Vec::new(),
        observation_roots: Vec::new(),
        write_paths: Vec::new(),
        write_path_prefixes: Vec::new(),
        hosts: Vec::new(),
        session_state_allowed: true,
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
