// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use std::collections::{BTreeMap, BTreeSet};

use super::runtime_fixtures::{
    RuntimeEffectFixture, create_runtime_effects, edit_runtime_effects, fetch_runtime_effects,
    observe_workspace_runtime_effects, todo_write_runtime_effects,
};
use lancet_authority::effect::ledger::EffectSource;
use lancet_catalog::CepMethod;
use lancet_catalog::catalog;
use lancet_catalog::methods::*;

fn runtime_effect_fixtures() -> BTreeMap<&'static str, RuntimeEffectFixture> {
    let mut fixtures = BTreeMap::new();
    fixtures.insert(
        ObserveWorkspace::NAME,
        observe_workspace_runtime_effects as RuntimeEffectFixture,
    );
    fixtures.insert(Create::NAME, create_runtime_effects as RuntimeEffectFixture);
    fixtures.insert(Edit::NAME, edit_runtime_effects as RuntimeEffectFixture);
    fixtures.insert(Fetch::NAME, fetch_runtime_effects as RuntimeEffectFixture);
    fixtures.insert(
        TodoWrite::NAME,
        todo_write_runtime_effects as RuntimeEffectFixture,
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
        for scenario in scenarios {
            let backend_effects: Vec<_> = scenario
                .effects
                .iter()
                .filter(|effect| effect.source == EffectSource::HandlerBackend)
                .collect();
            method_recorded_backend_effect |= !backend_effects.is_empty();

            for effect in &backend_effects {
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
    }
}

// ── Positive property tests ────────────────────────────────────────────
