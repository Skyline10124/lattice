use std::collections::BTreeMap;
use std::path::PathBuf;

use super::super::fixtures::*;
use super::super::support::build_test_context;
use lancet_authority::NetworkPolicyConfig;
use lancet_catalog::methods::command::RunParams;
use lancet_catalog::methods::*;
use lancet_protocol::envelope::{Capability, ImpactKind, VerifyCommand};
use lancet_protocol::policy::{CapabilitySource, SourcedCapability};

pub(in crate::effect_contract_tests) fn command_run_runtime_effects() -> Vec<RuntimeEffectScenario>
{
    use std::os::unix::fs::PermissionsExt;

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
        vec![run_cap_with(vec!["echo".into()])],
        NetworkPolicyConfig::safe_defaults(),
    );
    declare_envelope(&ctx, test_command_envelope(&root, vec!["echo".into()]));
    scenarios.push(scenario(
        "diagnostic_success_command_execution_only",
        dispatch_success_effects::<Run>(
            RunParams {
                argv: vec!["echo".into(), "diagnostic".into()],
                cwd: Some(root.clone()),
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
        vec![run_cap(), write_cap(&root)],
        NetworkPolicyConfig::safe_defaults(),
    );
    declare_active_envelope(&ctx, &root);
    scenarios.push(scenario(
        "mutation_success_command_and_workspace_mutation",
        dispatch_success_effects::<Run>(
            RunParams {
                argv: vec!["touch".into(), "src/composite.txt".into()],
                cwd: Some(root.clone()),
                env: BTreeMap::new(),
                timeout_ms: Some(5000),
                declared_impact: Some(ImpactKind::WorkspaceMutation),
            },
            ctx,
            command_mutation_region(&root),
        ),
    ));

    let timeout_script = dir.path().join("poc_timeout.sh");
    std::fs::write(
        &timeout_script,
        "#!/bin/sh\ntouch src/timeout.txt\nsleep 2\n",
    )
    .unwrap();
    let mut perms = std::fs::metadata(&timeout_script).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&timeout_script, perms).unwrap();
    let script_allowlist = vec!["./poc_timeout.sh".into()];
    let (ctx, _rx, _) = build_test_context(
        &root,
        vec![
            run_cap_with(script_allowlist.clone()),
            SourcedCapability {
                capability: Capability::WritePath {
                    path_pattern: "src/**".into(),
                },
                source: CapabilitySource::Declared {
                    envelope_id: "test-envelope".into(),
                },
            },
        ],
        NetworkPolicyConfig::safe_defaults(),
    );
    declare_envelope(
        &ctx,
        test_mutation_envelope_with(&root, "src/**", script_allowlist, None),
    );
    scenarios.push(scenario(
        "failure_timeout_records_workspace_mutation",
        dispatch_failure_effects::<Run>(
            RunParams {
                argv: vec!["./poc_timeout.sh".into()],
                cwd: Some(root.clone()),
                env: BTreeMap::new(),
                timeout_ms: Some(100),
                declared_impact: Some(ImpactKind::WorkspaceMutation),
            },
            ctx,
            command_mutation_region_with_write_prefixes(
                &root,
                vec![PathBuf::from(&root).join("src")],
            ),
        ),
    ));

    let (ctx, _rx, _) = build_test_context(
        &root,
        vec![run_cap(), write_cap(&root)],
        NetworkPolicyConfig::safe_defaults(),
    );
    declare_envelope(
        &ctx,
        test_mutation_envelope_with(&root, "src/**", vec!["echo".into(), "touch".into()], None),
    );
    scenarios.push(scenario(
        "scope_boundary_out_of_scope_records_then_replans",
        dispatch_failure_effects::<Run>(
            RunParams {
                argv: vec!["touch".into(), "outside.txt".into()],
                cwd: Some(root.clone()),
                env: BTreeMap::new(),
                timeout_ms: Some(5000),
                declared_impact: Some(ImpactKind::WorkspaceMutation),
            },
            ctx,
            command_mutation_region_with_write_prefixes(
                &root,
                vec![PathBuf::from(&root).join("src")],
            ),
        ),
    ));

    let (ctx, _rx, _) = build_test_context(
        &root,
        vec![run_cap(), write_cap(&root)],
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
        dispatch_success_effects::<Run>(
            RunParams {
                argv: vec!["touch".into(), "src/with_verify.txt".into()],
                cwd: Some(root.clone()),
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
