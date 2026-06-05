// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use std::collections::{BTreeMap, BTreeSet};

use super::runtime_fixtures::{
    audit_observe_runtime_effects, audit_replay_runtime_effects, command_run_runtime_effects,
    create_runtime_effects, envelope_runtime_effects, fetch_runtime_effects,
    observe_workspace_runtime_effects, patch_runtime_effects, todo_write_runtime_effects,
    RuntimeEffectFixture,
};
use lancet_authority::effect::ledger::EffectSource;
use lancet_catalog::catalog;
use lancet_catalog::method::RequiredCapability;
use lancet_catalog::methods::*;
use lancet_catalog::CepMethod;
use lancet_protocol::envelope::ImpactKind;

fn runtime_effect_fixtures() -> BTreeMap<&'static str, RuntimeEffectFixture> {
    let mut fixtures = BTreeMap::new();
    fixtures.insert(
        ObserveWorkspace::NAME,
        observe_workspace_runtime_effects as RuntimeEffectFixture,
    );
    fixtures.insert(Create::NAME, create_runtime_effects as RuntimeEffectFixture);
    fixtures.insert(Patch::NAME, patch_runtime_effects as RuntimeEffectFixture);
    fixtures.insert(
        Run::NAME,
        command_run_runtime_effects as RuntimeEffectFixture,
    );
    fixtures.insert(Fetch::NAME, fetch_runtime_effects as RuntimeEffectFixture);
    fixtures.insert(
        TodoWrite::NAME,
        todo_write_runtime_effects as RuntimeEffectFixture,
    );
    fixtures.insert(
        Envelope::NAME,
        envelope_runtime_effects as RuntimeEffectFixture,
    );
    fixtures.insert(
        AuditObserve::NAME,
        audit_observe_runtime_effects as RuntimeEffectFixture,
    );
    fixtures.insert(
        AuditReplay::NAME,
        audit_replay_runtime_effects as RuntimeEffectFixture,
    );
    fixtures
}

#[test]
fn runtime_backend_effects_are_declared_for_every_catalog_method() {
    let fixtures = runtime_effect_fixtures();
    let catalog_names: BTreeSet<_> = catalog().iter().map(|descriptor| descriptor.name).collect();
    let fixture_names: BTreeSet<_> = fixtures.keys().copied().collect();
    let missing: Vec<_> = catalog_names.difference(&fixture_names).copied().collect();
    let extra: Vec<_> = fixture_names.difference(&catalog_names).copied().collect();
    assert!(
        missing.is_empty() && extra.is_empty(),
        "runtime effect contract fixtures must cover the catalog exactly; missing={missing:?}, extra={extra:?}"
    );

    for descriptor in catalog() {
        let scenarios = fixtures
            .get(descriptor.name)
            .expect("fixture coverage checked")();
        assert!(
            !scenarios.is_empty(),
            "{} fixture did not define any runtime scenarios",
            descriptor.name
        );

        let mut method_recorded_backend_effect = false;
        let mut run_saw_command_execution = false;
        let mut run_saw_workspace_mutation = false;
        let mut run_saw_write_path = false;
        for scenario in scenarios {
            let backend_effects: Vec<_> = scenario
                .effects
                .iter()
                .filter(|effect| effect.source == EffectSource::HandlerBackend)
                .collect();
            method_recorded_backend_effect |= !backend_effects.is_empty();

            if scenario
                .name
                .contains("verify_success_filters_verify_source")
            {
                assert!(
                    scenario
                        .effects
                        .iter()
                        .any(|effect| effect.source == EffectSource::Verify),
                    "{} scenario {} must record a Verify-source effect so the source filter is exercised",
                    descriptor.name,
                    scenario.name
                );
            }

            for effect in &backend_effects {
                if descriptor.name == Run::NAME {
                    run_saw_command_execution |= effect.kind == ImpactKind::CommandExecution;
                    run_saw_workspace_mutation |= effect.kind == ImpactKind::WorkspaceMutation;
                    run_saw_write_path |=
                        effect.required_capability == RequiredCapability::WritePath;
                }
                assert!(
                    descriptor.effect.kinds.contains(&effect.kind),
                    "{} scenario {} runtime produced undeclared effect kind {:?}; declared={:?}",
                    descriptor.name,
                    scenario.name,
                    effect.kind,
                    descriptor.effect.kinds.0
                );
                assert!(
                    descriptor
                        .effect
                        .capabilities
                        .contains(&effect.required_capability),
                    "{} scenario {} runtime required undeclared capability {:?}; declared={:?}",
                    descriptor.name,
                    scenario.name,
                    effect.required_capability,
                    descriptor.effect.capabilities
                );
            }
        }

        assert!(
            method_recorded_backend_effect,
            "{} fixture scenarios did not record any HandlerBackend effects",
            descriptor.name
        );

        if descriptor.name == Run::NAME {
            assert!(
                run_saw_command_execution,
                "exec.command_run fixtures must record CommandExecution"
            );
            assert!(
                run_saw_workspace_mutation,
                "exec.command_run fixtures must record WorkspaceMutation"
            );
            assert!(
                run_saw_write_path,
                "exec.command_run mutation fixtures must exercise WritePath capability"
            );
        }
    }
}

// ── Positive property tests ────────────────────────────────────────────
