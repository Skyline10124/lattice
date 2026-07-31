// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

// `observe_workspace_runtime_effects` is gone (#30). Its five scenarios drove
// the four `observe.workspace` modes plus a missing-file failure through the
// native `MethodRegistry`; the method is an embedded Component Tool now, so the
// registry has no handler to dispatch and the fixture had nothing left to
// observe. The runtime-effect matrix keys on `lancet_catalog::catalog()`, which
// no longer lists the method either, so removing the fixture keeps that
// exactly-covers assertion honest rather than papering over a gap.

use super::super::fixtures::*;
use super::super::support::build_test_context;
use lancet_authority::NetworkPolicyConfig;
use lancet_catalog::methods::workspace::{CreateParams, EditOperation, EditParams};
use lancet_catalog::methods::*;

pub(in crate::effect_contract_tests) fn create_runtime_effects() -> Vec<RuntimeEffectScenario> {
    let dir = setup_tempdir_with_files();
    let root = std::fs::canonicalize(dir.path())
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    let mut scenarios = Vec::new();

    let (ctx, _rx, _) = build_test_context(&root, vec![], NetworkPolicyConfig::safe_defaults());
    declare_active_envelope(&ctx, &root);
    scenarios.push(scenario(
        "happy_create",
        dispatch_success_effects::<Create>(
            CreateParams {
                path: "created.txt".into(),
                content: "created\n".into(),
                create_parents: false,
            },
            ctx,
            mutation_region(&root),
        ),
    ));

    let (ctx, _rx, _) = build_test_context(&root, vec![], NetworkPolicyConfig::safe_defaults());
    declare_active_envelope(&ctx, &root);
    scenarios.push(scenario(
        "failure_target_exists_no_backend_effect",
        dispatch_failure_effects::<Create>(
            CreateParams {
                path: "hello.txt".into(),
                content: "created\n".into(),
                create_parents: false,
            },
            ctx,
            mutation_region(&root),
        ),
    ));

    scenarios
}

pub(in crate::effect_contract_tests) fn edit_runtime_effects() -> Vec<RuntimeEffectScenario> {
    let dir = setup_tempdir_with_files();
    let root = std::fs::canonicalize(dir.path())
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    let mut scenarios = Vec::new();

    let before = "hello\nworld\n";
    let (ctx, _rx, _) = build_test_context(&root, vec![], NetworkPolicyConfig::safe_defaults());
    declare_active_envelope(&ctx, &root);
    scenarios.push(scenario(
        "happy_edit",
        dispatch_success_effects::<Edit>(
            EditParams {
                path: "hello.txt".into(),
                expected_hash: format!("blake3:{}", blake3::hash(before.as_bytes()).to_hex()),
                edits: vec![EditOperation::ReplaceText {
                    old: "world".into(),
                    new: "edited".into(),
                    occurrence: None,
                }],
            },
            ctx,
            mutation_region(&root),
        ),
    ));

    let (ctx, _rx, _) = build_test_context(&root, vec![], NetworkPolicyConfig::safe_defaults());
    declare_active_envelope(&ctx, &root);
    scenarios.push(scenario(
        "failure_stale_hash_no_backend_effect",
        dispatch_failure_effects::<Edit>(
            EditParams {
                path: "data.txt".into(),
                expected_hash:
                    "blake3:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
                edits: vec![EditOperation::ReplaceText {
                    old: "some data content".into(),
                    new: "edited".into(),
                    occurrence: None,
                }],
            },
            ctx,
            mutation_region(&root),
        ),
    ));

    scenarios
}
