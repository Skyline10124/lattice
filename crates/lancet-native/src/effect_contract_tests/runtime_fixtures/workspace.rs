use super::super::fixtures::*;
use super::super::support::build_test_context;
use lancet_authority::NetworkPolicyConfig;
use lancet_catalog::methods::workspace::{
    CreateParams, FindFilesParams, GrepParams, ListDirectoryParams, ObserveWorkspaceParams,
    PatchParams, ReadFileParams,
};
use lancet_catalog::methods::*;
use lancet_protocol::envelope::VerifyCommand;

pub(in crate::effect_contract_tests) fn observe_workspace_runtime_effects(
) -> Vec<RuntimeEffectScenario> {
    let dir = setup_tempdir_with_files();
    let root = std::fs::canonicalize(dir.path())
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    let mut scenarios = Vec::new();

    let observe_caps = vec![read_cap(&root), inspect_cap()];
    let (ctx, _rx, _) = build_test_context(
        &root,
        observe_caps.clone(),
        NetworkPolicyConfig::safe_defaults(),
    );
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

    let (ctx, _rx, _) = build_test_context(
        &root,
        observe_caps.clone(),
        NetworkPolicyConfig::safe_defaults(),
    );
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

    let (ctx, _rx, _) = build_test_context(
        &root,
        observe_caps.clone(),
        NetworkPolicyConfig::safe_defaults(),
    );
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

    let (ctx, _rx, _) = build_test_context(
        &root,
        observe_caps.clone(),
        NetworkPolicyConfig::safe_defaults(),
    );
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

    let (ctx, _rx, _) =
        build_test_context(&root, observe_caps, NetworkPolicyConfig::safe_defaults());
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

    let (ctx, _rx, _) = build_test_context(
        &root,
        vec![write_cap(&root)],
        NetworkPolicyConfig::safe_defaults(),
    );
    declare_active_envelope(&ctx, &root);
    scenarios.push(scenario(
        "happy_create",
        dispatch_success_effects::<Create>(
            CreateParams {
                path: "created.txt".into(),
                content: "created\n".into(),
            },
            ctx,
            mutation_region(&root),
        ),
    ));

    let (ctx, _rx, _) = build_test_context(
        &root,
        vec![write_cap(&root)],
        NetworkPolicyConfig::safe_defaults(),
    );
    declare_active_envelope(&ctx, &root);
    scenarios.push(scenario(
        "failure_target_exists_no_backend_effect",
        dispatch_failure_effects::<Create>(
            CreateParams {
                path: "hello.txt".into(),
                content: "created\n".into(),
            },
            ctx,
            mutation_region(&root),
        ),
    ));

    let (ctx, _rx, _) = build_test_context(
        &root,
        vec![write_cap(&root)],
        NetworkPolicyConfig::safe_defaults(),
    );
    declare_envelope(
        &ctx,
        test_mutation_envelope_with(&root, "src/**", vec!["echo".into(), "touch".into()], None),
    );
    scenarios.push(scenario(
        "scope_boundary_out_of_scope_no_backend_effect",
        dispatch_failure_effects::<Create>(
            CreateParams {
                path: "outside.txt".into(),
                content: "outside\n".into(),
            },
            ctx,
            mutation_region(&root),
        ),
    ));

    let (ctx, _rx, _) = build_test_context(
        &root,
        vec![write_cap(&root), run_cap()],
        NetworkPolicyConfig::safe_defaults(),
    );
    declare_envelope(
        &ctx,
        test_mutation_envelope_with(
            &root,
            ".",
            vec!["echo".into(), "touch".into()],
            Some(VerifyCommand {
                command: "echo ok".into(),
                expected_exit_code: None,
            }),
        ),
    );
    scenarios.push(scenario(
        "verify_success_filters_verify_source",
        dispatch_success_effects::<Create>(
            CreateParams {
                path: "created_with_verify.txt".into(),
                content: "created\n".into(),
            },
            ctx,
            mutation_region(&root),
        ),
    ));

    let (ctx, _rx, _) = build_test_context(
        &root,
        vec![write_cap(&root), run_cap()],
        NetworkPolicyConfig::safe_defaults(),
    );
    declare_envelope(
        &ctx,
        test_mutation_envelope_with(
            &root,
            ".",
            vec!["echo".into(), "touch".into()],
            Some(VerifyCommand {
                command: "echo ok".into(),
                expected_exit_code: Some(1),
            }),
        ),
    );
    scenarios.push(scenario(
        "failure_verify_failed_retains_backend_mutation",
        dispatch_failure_effects::<Create>(
            CreateParams {
                path: "created_then_verify_failed.txt".into(),
                content: "created\n".into(),
            },
            ctx,
            mutation_region(&root),
        ),
    ));

    scenarios
}

pub(in crate::effect_contract_tests) fn patch_runtime_effects() -> Vec<RuntimeEffectScenario> {
    let dir = setup_tempdir_with_files();
    let root = std::fs::canonicalize(dir.path())
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    let mut scenarios = Vec::new();

    let (ctx, _rx, _) = build_test_context(
        &root,
        vec![write_cap(&root)],
        NetworkPolicyConfig::safe_defaults(),
    );
    declare_active_envelope(&ctx, &root);
    scenarios.push(scenario(
        "happy_patch",
        dispatch_success_effects::<Patch>(
            PatchParams {
                patch: update_patch_text("hello.txt", "hello\nworld\n", "world", "patched"),
            },
            ctx,
            mutation_region(&root),
        ),
    ));

    let (ctx, _rx, _) = build_test_context(
        &root,
        vec![write_cap(&root)],
        NetworkPolicyConfig::safe_defaults(),
    );
    declare_active_envelope(&ctx, &root);
    scenarios.push(scenario(
        "failure_hash_mismatch_no_backend_effect",
        dispatch_failure_effects::<Patch>(
            PatchParams {
                patch: "*** Begin Patch\n*** Update File: data.txt\n*** Old Hash: blake3:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n@@\n blake3:aaaa\tsome data content\n-blake3:aaaa\tsome data content\n+patched\n*** End Patch\n".into(),
            },
            ctx,
            mutation_region(&root),
        ),
    ));

    let (ctx, _rx, _) = build_test_context(
        &root,
        vec![write_cap(&root)],
        NetworkPolicyConfig::safe_defaults(),
    );
    declare_envelope(
        &ctx,
        test_mutation_envelope_with(&root, "src/**", vec!["echo".into(), "touch".into()], None),
    );
    scenarios.push(scenario(
        "scope_boundary_out_of_scope_no_backend_effect",
        dispatch_failure_effects::<Patch>(
            PatchParams {
                patch: update_patch_text(
                    "data.txt",
                    "some data content\n",
                    "some data content",
                    "patched",
                ),
            },
            ctx,
            mutation_region(&root),
        ),
    ));

    std::fs::write(dir.path().join("verify_success.txt"), "alpha\nbeta\n").unwrap();
    let (ctx, _rx, _) = build_test_context(
        &root,
        vec![write_cap(&root), run_cap()],
        NetworkPolicyConfig::safe_defaults(),
    );
    declare_envelope(
        &ctx,
        test_mutation_envelope_with(
            &root,
            ".",
            vec!["echo".into(), "touch".into()],
            Some(VerifyCommand {
                command: "echo ok".into(),
                expected_exit_code: None,
            }),
        ),
    );
    scenarios.push(scenario(
        "verify_success_filters_verify_source",
        dispatch_success_effects::<Patch>(
            PatchParams {
                patch: update_patch_text("verify_success.txt", "alpha\nbeta\n", "beta", "patched"),
            },
            ctx,
            mutation_region(&root),
        ),
    ));

    std::fs::write(dir.path().join("verify_failure.txt"), "alpha\nbeta\n").unwrap();
    let (ctx, _rx, _) = build_test_context(
        &root,
        vec![write_cap(&root), run_cap()],
        NetworkPolicyConfig::safe_defaults(),
    );
    declare_envelope(
        &ctx,
        test_mutation_envelope_with(
            &root,
            ".",
            vec!["echo".into(), "touch".into()],
            Some(VerifyCommand {
                command: "echo ok".into(),
                expected_exit_code: Some(1),
            }),
        ),
    );
    scenarios.push(scenario(
        "failure_verify_failed_retains_backend_mutation",
        dispatch_failure_effects::<Patch>(
            PatchParams {
                patch: update_patch_text("verify_failure.txt", "alpha\nbeta\n", "beta", "patched"),
            },
            ctx,
            mutation_region(&root),
        ),
    ));

    scenarios
}
