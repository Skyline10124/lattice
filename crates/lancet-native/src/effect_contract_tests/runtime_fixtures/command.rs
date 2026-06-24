// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use std::collections::BTreeMap;

use super::super::fixtures::*;
use super::super::support::build_test_context;
use lancet_authority::NetworkPolicyConfig;
use lancet_catalog::methods::command::RunParams;
use lancet_catalog::methods::*;
use lancet_protocol::effect_types::ImpactKind;


pub(in crate::effect_contract_tests) fn command_run_runtime_effects() -> Vec<RuntimeEffectScenario>
{
    let dir = setup_tempdir_with_files();
    let root = std::fs::canonicalize(dir.path())
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    std::fs::create_dir_all(std::path::Path::new(&root).join("src")).unwrap();
    let mut scenarios = Vec::new();

    let (ctx, _rx, _) = build_test_context(
        &root,
        vec![],
        NetworkPolicyConfig::safe_defaults(),
    );
    scenarios.push(scenario(
        "diagnostic_success_command_execution_only",
        dispatch_success_effects::<Run>(
            RunParams {
                argv: vec!["echo".into(), "diagnostic".into()],
                cwd: Some(".".into()),
                env: BTreeMap::new(),
                timeout_ms: Some(5000),
                declared_impact: None,
            },
            ctx,
            command_region(),
        ),
    ));

    let (ctx, _rx, _) = build_test_context(
        &root,
        vec![],
        NetworkPolicyConfig::safe_defaults(),
    );
    declare_active_envelope(&ctx, &root);
    scenarios.push(scenario(
        "mutation_success_command_and_workspace_mutation",
        dispatch_success_effects::<Run>(
            RunParams {
                argv: vec!["touch".into(), "src/composite.txt".into()],
                cwd: Some(".".into()),
                env: BTreeMap::new(),
                timeout_ms: Some(5000),
                declared_impact: Some(ImpactKind::WorkspaceMutation),
            },
            ctx,
            command_mutation_region(&root),
        ),
    ));

    scenarios
}
