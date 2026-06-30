// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use super::super::fixtures::*;
use super::super::support::build_test_context;
use lancet_authority::NetworkPolicyConfig;
use lancet_catalog::methods::workspace::{
    BatchOperation, BatchParams, CreateParams, EditOperation, EditParams, FindFilesParams,
    GrepParams, ListDirectoryParams, ObserveWorkspaceParams, ReadFileParams,
};
use lancet_catalog::methods::*;

pub(in crate::effect_contract_tests) fn observe_workspace_runtime_effects()
-> Vec<RuntimeEffectScenario> {
    let dir = setup_tempdir_with_files();
    let root = std::fs::canonicalize(dir.path())
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    let mut scenarios = Vec::new();

    let (ctx, _rx, _) = build_test_context(&root, vec![], NetworkPolicyConfig::safe_defaults());
    scenarios.push(scenario(
        "happy_read_file_read_path",
        dispatch_success_effects::<ObserveWorkspace>(
            ObserveWorkspaceParams::ReadFile(ReadFileParams {
                path: "hello.txt".into(),
                offset: None,
                limit: None,
                line_numbers: None,
            }),
            ctx,
            observation_region(&root),
        ),
    ));

    let (ctx, _rx, _) =
        build_test_context(&root, vec![].clone(), NetworkPolicyConfig::safe_defaults());
    scenarios.push(scenario(
        "happy_grep_inspect",
        dispatch_success_effects::<ObserveWorkspace>(
            ObserveWorkspaceParams::Grep(GrepParams {
                pattern: "hello".into(),
                path: None,
                max_results: Some(10),
            }),
            ctx,
            observation_region(&root),
        ),
    ));

    let (ctx, _rx, _) =
        build_test_context(&root, vec![].clone(), NetworkPolicyConfig::safe_defaults());
    scenarios.push(scenario(
        "happy_list_directory_inspect",
        dispatch_success_effects::<ObserveWorkspace>(
            ObserveWorkspaceParams::ListDirectory(ListDirectoryParams {
                path: ".".into(),
                limit: Some(10),
            }),
            ctx,
            observation_region(&root),
        ),
    ));

    let (ctx, _rx, _) =
        build_test_context(&root, vec![].clone(), NetworkPolicyConfig::safe_defaults());
    scenarios.push(scenario(
        "happy_find_files_inspect",
        dispatch_success_effects::<ObserveWorkspace>(
            ObserveWorkspaceParams::FindFiles(FindFilesParams {
                pattern: "*.txt".into(),
                path: Some(".".into()),
                limit: Some(10),
            }),
            ctx,
            observation_region(&root),
        ),
    ));

    let (ctx, _rx, _) = build_test_context(&root, vec![], NetworkPolicyConfig::safe_defaults());
    scenarios.push(scenario(
        "failure_missing_file_no_backend_effect",
        dispatch_failure_effects::<ObserveWorkspace>(
            ObserveWorkspaceParams::ReadFile(ReadFileParams {
                path: "missing.txt".into(),
                offset: None,
                limit: None,
                line_numbers: None,
            }),
            ctx,
            observation_region(&root),
        ),
    ));

    scenarios
}

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

pub(in crate::effect_contract_tests) fn batch_runtime_effects() -> Vec<RuntimeEffectScenario> {
    let dir = setup_tempdir_with_files();
    let root = std::fs::canonicalize(dir.path())
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    let mut scenarios = Vec::new();

    let before = "some data content\n";
    let (ctx, _rx, _) = build_test_context(&root, vec![], NetworkPolicyConfig::safe_defaults());
    declare_active_envelope(&ctx, &root);
    scenarios.push(scenario(
        "happy_batch_create_and_edit",
        dispatch_success_effects::<Batch>(
            BatchParams {
                operations: vec![
                    BatchOperation::EditFile {
                        path: "data.txt".into(),
                        expected_hash: format!(
                            "blake3:{}",
                            blake3::hash(before.as_bytes()).to_hex()
                        ),
                        edits: vec![EditOperation::ReplaceText {
                            old: "some data content".into(),
                            new: "edited".into(),
                            occurrence: None,
                        }],
                    },
                    BatchOperation::CreateFile {
                        path: "created.txt".into(),
                        content: "created\n".into(),
                        create_parents: false,
                    },
                ],
            },
            ctx,
            mutation_region(&root),
        ),
    ));

    scenarios
}
