//! Effect-region contract tests.
//!
//! Per-method positive + negative property tests via the reusable
//! [`effect_contract_harness`], plus a deterministic malicious-handler
//! fixture and the composite command-mutation audit test.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use proptest::prelude::*;
use serde::Serialize;
use tempfile::TempDir;

use lancet_audit::{AuditOutcome, CallAuditMetadata};
use lancet_authority::effect::ledger::{EffectSource, ObservedEffect, ObservedScope};
use lancet_authority::effect::region::AuthorizedEffectRegion;
use lancet_catalog::catalog;
use lancet_catalog::method::RequiredCapability;
use lancet_catalog::methods::audit::{AuditObserveParams, AuditQueryParams, AuditReplayParams};
use lancet_catalog::methods::command::RunParams;
use lancet_catalog::methods::envelope::{DeclareParams, EnvelopeParams};
use lancet_catalog::methods::external::FetchParams;
use lancet_catalog::methods::session::{TodoItem, TodoStatus, TodoWriteParams};
use lancet_catalog::methods::workspace::{
    CreateParams, FindFilesParams, GrepParams, ListDirectoryParams, ObserveWorkspaceParams,
    ObserveWorkspaceResult, PatchParams, ReadFileParams,
};
use lancet_catalog::methods::*;
use lancet_catalog::{CatalogMethod, CepMethod};
use lancet_native::testing::effect_harness::{
    assert_negative_no_backend_effects, assert_positive, build_test_context,
    effect_contract_harness, rt,
};
use lancet_protocol::envelope::{
    Capability, Constraints, ExecutionEnvelope, Impact as EnvImpact, ImpactConfidence, ImpactKind,
    Intent, Scope as EnvScope, VerifyCommand,
};
use lancet_protocol::policy::{CapabilitySource, SourcedCapability};

use lancet_authority::NetworkPolicyConfig;

// ── Helper builders ────────────────────────────────────────────────────

fn test_registry() -> lancet_native::MethodRegistry {
    let mut registry = lancet_native::MethodRegistry::new();
    lancet_native::register_production_method_registry(&mut registry).unwrap();
    registry
}

fn read_cap(root: &str) -> SourcedCapability {
    SourcedCapability {
        capability: Capability::ReadPath {
            path_pattern: root.to_string(),
        },
        source: CapabilitySource::Base,
    }
}

fn inspect_cap() -> SourcedCapability {
    SourcedCapability {
        capability: Capability::Inspect,
        source: CapabilitySource::Base,
    }
}

fn write_cap(_root: &str) -> SourcedCapability {
    SourcedCapability {
        capability: Capability::WritePath {
            path_pattern: ".".to_string(),
        },
        source: CapabilitySource::Declared {
            envelope_id: "test-envelope".into(),
        },
    }
}

fn run_cap() -> SourcedCapability {
    run_cap_with(vec!["echo".into(), "touch".into()])
}

fn run_cap_with(argv_allowlist: Vec<String>) -> SourcedCapability {
    SourcedCapability {
        capability: Capability::RunCommand { argv_allowlist },
        source: CapabilitySource::Declared {
            envelope_id: "test-envelope".into(),
        },
    }
}

fn test_mutation_envelope(root: &str) -> ExecutionEnvelope {
    test_mutation_envelope_with(root, ".", vec!["echo".into(), "touch".into()], None)
}

fn test_command_envelope(root: &str, argv_allowlist: Vec<String>) -> ExecutionEnvelope {
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

fn test_mutation_envelope_with(
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

fn declare_active_envelope(ctx: &lancet_native::registry::HandlerContext, root: &str) {
    let envelope = test_mutation_envelope(root);
    declare_envelope(ctx, envelope);
}

fn declare_envelope(ctx: &lancet_native::registry::HandlerContext, envelope: ExecutionEnvelope) {
    rt().block_on(ctx.active_envelope.declare(envelope, 0, "test-turn"))
        .unwrap();
}

fn session_cap() -> SourcedCapability {
    SourcedCapability {
        capability: Capability::MutateSessionState,
        source: CapabilitySource::Base,
    }
}

fn audit_cap() -> SourcedCapability {
    SourcedCapability {
        capability: Capability::ReadAudit,
        source: CapabilitySource::Base,
    }
}

fn observation_region(root: &str) -> AuthorizedEffectRegion {
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

fn mutation_region(root: &str) -> AuthorizedEffectRegion {
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

fn command_region() -> AuthorizedEffectRegion {
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

fn command_mutation_region(root: &str) -> AuthorizedEffectRegion {
    command_mutation_region_with_write_prefixes(root, vec![PathBuf::from(root)])
}

fn command_mutation_region_with_write_prefixes(
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

fn session_region() -> AuthorizedEffectRegion {
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

fn audit_region() -> AuthorizedEffectRegion {
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

fn setup_tempdir_with_files() -> TempDir {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("hello.txt"), "hello\nworld\n").unwrap();
    std::fs::write(dir.path().join("data.txt"), "some data content\n").unwrap();
    std::fs::create_dir_all(dir.path().join("subdir")).unwrap();
    std::fs::write(dir.path().join("subdir/nested.txt"), "nested\n").unwrap();
    dir
}

fn hashline(line_number: usize, line: &str) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"lattice.hashline.v2\0");
    hasher.update(&(line_number as u64).to_be_bytes());
    hasher.update(b"\0");
    hasher.update(line.as_bytes());
    let digest = hasher.finalize();
    let hex = digest.to_hex();
    format!("blake3:{}", &hex[..4])
}

fn update_patch_text(path: &str, before: &str, remove_line: &str, add_line: &str) -> String {
    let hash = blake3::hash(before.as_bytes());
    let hash_hex = hash.to_hex();
    let keep_line = before.lines().next().unwrap_or_default();
    let keep_hash = hashline(1, keep_line);
    let remove_line_number = before
        .lines()
        .position(|line| line == remove_line)
        .map(|index| index + 1)
        .expect("remove line must exist in before text");
    let remove_hash = hashline(remove_line_number, remove_line);
    format!(
        "*** Begin Patch\n*** Update File: {path}\n*** Old Hash: blake3:{hash_hex}\n@@\n {keep_hash}\t{keep_line}\n-{remove_hash}\t{remove_line}\n+{add_line}\n*** End Patch\n"
    )
}

#[derive(Debug)]
struct RuntimeEffectScenario {
    name: &'static str,
    effects: Vec<ObservedEffect>,
}

fn scenario(name: &'static str, effects: Vec<ObservedEffect>) -> RuntimeEffectScenario {
    RuntimeEffectScenario { name, effects }
}

#[derive(Clone, Copy, Debug)]
enum DispatchExpectation {
    Success,
    Failure,
}

fn dispatch_effects<M>(
    params: M::Params,
    ctx: lancet_native::registry::HandlerContext,
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

fn dispatch_success_effects<M>(
    params: M::Params,
    ctx: lancet_native::registry::HandlerContext,
    authority: AuthorizedEffectRegion,
) -> Vec<ObservedEffect>
where
    M: CatalogMethod,
    M::Params: Serialize,
{
    dispatch_effects::<M>(params, ctx, authority, DispatchExpectation::Success)
}

fn dispatch_failure_effects<M>(
    params: M::Params,
    ctx: lancet_native::registry::HandlerContext,
    authority: AuthorizedEffectRegion,
) -> Vec<ObservedEffect>
where
    M: CatalogMethod,
    M::Params: Serialize,
{
    dispatch_effects::<M>(params, ctx, authority, DispatchExpectation::Failure)
}

type RuntimeEffectFixture = fn() -> Vec<RuntimeEffectScenario>;

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

fn observe_workspace_runtime_effects() -> Vec<RuntimeEffectScenario> {
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

fn create_runtime_effects() -> Vec<RuntimeEffectScenario> {
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

fn patch_runtime_effects() -> Vec<RuntimeEffectScenario> {
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

fn command_run_runtime_effects() -> Vec<RuntimeEffectScenario> {
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

fn fetch_runtime_effects() -> Vec<RuntimeEffectScenario> {
    let dir = setup_tempdir_with_files();
    let root = std::fs::canonicalize(dir.path())
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    let registry = test_registry();

    let mut scenarios = Vec::new();
    let happy_effects = rt().block_on(async {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let addr_str = format!("127.0.0.1:{port}");

        tokio::spawn(async move {
            if let Ok((mut stream, _)) = listener.accept().await {
                use tokio::io::{AsyncReadExt, AsyncWriteExt};
                let mut buf = [0u8; 1024];
                let _ = stream.read(&mut buf).await;
                let response = b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nOK";
                let _ = stream.write_all(response).await;
            }
        });

        let fetch_cap = SourcedCapability {
            capability: Capability::FetchUrl {
                host: "127.0.0.1".into(),
            },
            source: CapabilitySource::Base,
        };
        let network = NetworkPolicyConfig {
            allow_public_https: true,
            allow_localhost: true,
            allowed_hosts: vec!["127.0.0.1".into()],
            max_web_response_bytes: 10 * 1024 * 1024,
        };
        let (ctx, _rx, _) = build_test_context(&root, vec![fetch_cap], network);

        let mut kinds = BTreeSet::new();
        kinds.insert(ImpactKind::ExternalObservation);
        let outcome = effect_contract_harness::<Fetch>(
            &registry,
            FetchParams {
                url: format!("http://{addr_str}/test"),
                headers: BTreeMap::new(),
            },
            ctx,
            AuthorizedEffectRegion {
                kinds,
                read_paths: Vec::new(),
                observation_roots: Vec::new(),
                write_paths: Vec::new(),
                write_path_prefixes: Vec::new(),
                hosts: vec!["127.0.0.1".into()],
                session_state_allowed: false,
                command_allowed: false,
            },
        )
        .await;
        outcome
            .dispatch_result
            .as_ref()
            .expect("fetch.web fixture dispatch must succeed");
        outcome.observed_effects
    });
    scenarios.push(scenario("happy_fetch_localhost", happy_effects));

    let fetch_cap = SourcedCapability {
        capability: Capability::FetchUrl {
            host: "allowed.example.com".into(),
        },
        source: CapabilitySource::Base,
    };
    let (ctx, _rx, _) =
        build_test_context(&root, vec![fetch_cap], NetworkPolicyConfig::safe_defaults());
    let mut kinds = BTreeSet::new();
    kinds.insert(ImpactKind::ExternalObservation);
    scenarios.push(scenario(
        "failure_host_denied_no_backend_effect",
        dispatch_failure_effects::<Fetch>(
            FetchParams {
                url: "https://denied.example.com/test".into(),
                headers: BTreeMap::new(),
            },
            ctx,
            AuthorizedEffectRegion {
                kinds,
                read_paths: Vec::new(),
                observation_roots: Vec::new(),
                write_paths: Vec::new(),
                write_path_prefixes: Vec::new(),
                hosts: vec!["allowed.example.com".into()],
                session_state_allowed: false,
                command_allowed: false,
            },
        ),
    ));

    scenarios
}

fn todo_write_runtime_effects() -> Vec<RuntimeEffectScenario> {
    let dir = setup_tempdir_with_files();
    let root = std::fs::canonicalize(dir.path())
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    let mut scenarios = Vec::new();

    let (ctx, _rx, _) = build_test_context(
        &root,
        vec![session_cap()],
        NetworkPolicyConfig::safe_defaults(),
    );
    scenarios.push(scenario(
        "happy_todo_write",
        dispatch_success_effects::<TodoWrite>(
            TodoWriteParams {
                todos: vec![TodoItem {
                    content: "item".into(),
                    status: TodoStatus::Pending,
                }],
            },
            ctx,
            session_region(),
        ),
    ));

    let (ctx, _rx, _) = build_test_context(
        &root,
        vec![session_cap()],
        NetworkPolicyConfig::safe_defaults(),
    );
    scenarios.push(scenario(
        "failure_empty_todo_no_backend_effect",
        dispatch_failure_effects::<TodoWrite>(
            TodoWriteParams {
                todos: vec![TodoItem {
                    content: "   ".into(),
                    status: TodoStatus::Pending,
                }],
            },
            ctx,
            session_region(),
        ),
    ));

    scenarios
}

fn envelope_runtime_effects() -> Vec<RuntimeEffectScenario> {
    let dir = setup_tempdir_with_files();
    let root = std::fs::canonicalize(dir.path())
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    let mut scenarios = Vec::new();

    let (ctx, _rx, _) = build_test_context(
        &root,
        vec![session_cap()],
        NetworkPolicyConfig::safe_defaults(),
    );
    scenarios.push(scenario(
        "happy_declare",
        dispatch_success_effects::<Envelope>(
            EnvelopeParams::Declare(DeclareParams {
                envelope: test_mutation_envelope(&root),
                supersede: false,
                reason: None,
            }),
            ctx,
            session_region(),
        ),
    ));

    let (ctx, _rx, _) = build_test_context(
        &root,
        vec![session_cap()],
        NetworkPolicyConfig::safe_defaults(),
    );
    declare_active_envelope(&ctx, &root);
    scenarios.push(scenario(
        "failure_already_active_no_backend_effect",
        dispatch_failure_effects::<Envelope>(
            EnvelopeParams::Declare(DeclareParams {
                envelope: test_mutation_envelope(&root),
                supersede: false,
                reason: None,
            }),
            ctx,
            session_region(),
        ),
    ));

    scenarios
}

fn audit_observe_runtime_effects() -> Vec<RuntimeEffectScenario> {
    let dir = setup_tempdir_with_files();
    let root = std::fs::canonicalize(dir.path())
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    let mut scenarios = Vec::new();

    let (ctx, _rx, _) = build_test_context(
        &root,
        vec![audit_cap()],
        NetworkPolicyConfig::safe_defaults(),
    );
    scenarios.push(scenario(
        "happy_query",
        dispatch_success_effects::<AuditObserve>(
            AuditObserveParams {
                stream: false,
                query: AuditQueryParams::default(),
            },
            ctx,
            audit_region(),
        ),
    ));

    let (ctx, _rx, _) = build_test_context(
        &root,
        vec![audit_cap()],
        NetworkPolicyConfig::safe_defaults(),
    );
    scenarios.push(scenario(
        "happy_tail",
        dispatch_success_effects::<AuditObserve>(
            AuditObserveParams {
                stream: true,
                query: AuditQueryParams::default(),
            },
            ctx,
            audit_region(),
        ),
    ));

    let (ctx, _rx, _) = build_test_context(&root, vec![], NetworkPolicyConfig::safe_defaults());
    scenarios.push(scenario(
        "failure_missing_read_audit_cap_no_backend_effect",
        dispatch_failure_effects::<AuditObserve>(
            AuditObserveParams {
                stream: false,
                query: AuditQueryParams::default(),
            },
            ctx,
            audit_region(),
        ),
    ));

    scenarios
}

fn seed_audit_replay_row(ctx: &lancet_native::registry::HandlerContext) {
    rt().block_on(async {
        let audit_context = ctx.audit_context.read().await.clone();
        let start = ctx
            .audit
            .start_call(
                &audit_context,
                99,
                TodoWrite::NAME,
                &CallAuditMetadata::native_policy_enforced(Some("started")),
            )
            .unwrap();
        ctx.audit
            .finalize_call(
                start,
                AuditOutcome::Success {
                    summary: "seed".into(),
                },
                Some("native_handler"),
                Some("{}"),
            )
            .unwrap();
    });
}

fn audit_replay_runtime_effects() -> Vec<RuntimeEffectScenario> {
    let dir = setup_tempdir_with_files();
    let root = std::fs::canonicalize(dir.path())
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    let mut scenarios = Vec::new();

    let (ctx, _rx, _) = build_test_context(
        &root,
        vec![audit_cap()],
        NetworkPolicyConfig::safe_defaults(),
    );
    seed_audit_replay_row(&ctx);
    scenarios.push(scenario(
        "happy_replay_seeded_turn",
        dispatch_success_effects::<AuditReplay>(
            AuditReplayParams {
                turn_id: "test-turn".into(),
            },
            ctx,
            audit_region(),
        ),
    ));

    let (ctx, _rx, _) = build_test_context(
        &root,
        vec![audit_cap()],
        NetworkPolicyConfig::safe_defaults(),
    );
    scenarios.push(scenario(
        "failure_missing_turn_no_backend_effect",
        dispatch_failure_effects::<AuditReplay>(
            AuditReplayParams {
                turn_id: "missing-turn".into(),
            },
            ctx,
            audit_region(),
        ),
    ));

    scenarios
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

proptest! {
    #[test]
    fn read_file_effect_contract_positive(offset in 0u64..100, limit in 1u64..50) {
        let dir = setup_tempdir_with_files();
        let root = std::fs::canonicalize(dir.path()).unwrap().to_str().unwrap().to_string();
        let registry = test_registry();
        let (ctx, _rx, _) = build_test_context(&root, vec![read_cap(&root)], NetworkPolicyConfig::safe_defaults());
        let authority = observation_region(&root);
        let params = ObserveWorkspaceParams::ReadFile(ReadFileParams {
            path: "hello.txt".into(),
            offset: Some(offset),
            limit: Some(limit),
            line_numbers: None,
        });
        let outcome = rt().block_on(effect_contract_harness::<ObserveWorkspace>(
            &registry, params, ctx, authority.clone(),
        ));
        outcome.dispatch_result.as_ref().expect("legal dispatch must succeed");
        assert_positive(&outcome, &authority);
    }
}

proptest! {
    #[test]
    fn grep_effect_contract_positive(pattern in "[a-z]{1,8}") {
        let dir = setup_tempdir_with_files();
        let root = std::fs::canonicalize(dir.path()).unwrap().to_str().unwrap().to_string();
        let registry = test_registry();
        let (ctx, _rx, _) = build_test_context(&root, vec![inspect_cap()], NetworkPolicyConfig::safe_defaults());
        let authority = observation_region(&root);
        let params = ObserveWorkspaceParams::Grep(GrepParams {
            pattern,
            path: None,
            max_results: Some(10),
        });
        let outcome = rt().block_on(effect_contract_harness::<ObserveWorkspace>(
            &registry, params, ctx, authority.clone(),
        ));
        outcome.dispatch_result.as_ref().expect("legal dispatch must succeed");
        assert_positive(&outcome, &authority);
    }
}

proptest! {
    #[test]
    fn create_effect_contract_positive(content in "[a-zA-Z0-9 ]{1,64}") {
        let dir = setup_tempdir_with_files();
        let root = std::fs::canonicalize(dir.path()).unwrap().to_str().unwrap().to_string();
        let registry = test_registry();
        let (ctx, _rx, _) = build_test_context(&root, vec![write_cap(&root)], NetworkPolicyConfig::safe_defaults());
        declare_active_envelope(&ctx, &root);
        let authority = mutation_region(&root);
        let params = CreateParams {
            path: "output.txt".into(),
            content,
        };
        let outcome = rt().block_on(effect_contract_harness::<Create>(
            &registry, params, ctx, authority.clone(),
        ));
        outcome.dispatch_result.as_ref().expect("legal create must dispatch");
        assert_positive(&outcome, &authority);
    }
}

proptest! {
    #[test]
    fn patch_effect_contract_positive(_n in 0u8..1) {
        let dir = setup_tempdir_with_files();
        let root = std::fs::canonicalize(dir.path()).unwrap().to_str().unwrap().to_string();
        let registry = test_registry();

        // Compute content hash for V2 patch format
        let content = std::fs::read(dir.path().join("hello.txt")).unwrap();
        let hash = blake3::hash(&content);
        let hash_hex = hash.to_hex();
        let hello_hash = hashline(1, "hello");
        let world_hash = hashline(2, "world");

        let (ctx, _rx, _) = build_test_context(&root, vec![write_cap(&root)], NetworkPolicyConfig::safe_defaults());
        declare_active_envelope(&ctx, &root);
        let authority = mutation_region(&root);
        let patch_text = format!(
            "*** Begin Patch\n*** Update File: hello.txt\n*** Old Hash: blake3:{hash_hex}\n@@\n {hello_hash}\thello\n-{world_hash}\tworld\n+patched\n*** End Patch\n"
        );
        let params = PatchParams { patch: patch_text };
        let outcome = rt().block_on(effect_contract_harness::<Patch>(
            &registry, params, ctx, authority.clone(),
        ));
        outcome.dispatch_result.as_ref().expect("legal patch must dispatch");
        assert_positive(&outcome, &authority);
    }
}

proptest! {
    #[test]
    fn run_effect_contract_positive(msg in "[a-z ]{1,32}") {
        let dir = setup_tempdir_with_files();
        let root = std::fs::canonicalize(dir.path()).unwrap().to_str().unwrap().to_string();
        let registry = test_registry();
        let (ctx, _rx, _) = build_test_context(&root, vec![run_cap()], NetworkPolicyConfig::safe_defaults());
        declare_active_envelope(&ctx, &root);
        let authority = command_region();
        let params = RunParams {
            argv: vec!["echo".into(), msg],
            cwd: Some(root.clone()),
            env: BTreeMap::new(),
            timeout_ms: Some(5000),
            declared_impact: None,
        };
        let outcome = rt().block_on(effect_contract_harness::<Run>(
            &registry, params, ctx, authority.clone(),
        ));
        outcome.dispatch_result.as_ref().expect("legal run must dispatch");
        assert_positive(&outcome, &authority);
    }
}

#[test]
fn fetch_effect_contract_positive() {
    let dir = setup_tempdir_with_files();
    let root = std::fs::canonicalize(dir.path())
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    let registry = test_registry();

    rt().block_on(async {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let addr_str = format!("127.0.0.1:{port}");

        tokio::spawn(async move {
            if let Ok((mut stream, _)) = listener.accept().await {
                use tokio::io::{AsyncReadExt, AsyncWriteExt};
                let mut buf = [0u8; 1024];
                let _ = stream.read(&mut buf).await;
                let response = b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nOK";
                let _ = stream.write_all(response).await;
            }
        });

        let fetch_cap = SourcedCapability {
            capability: Capability::FetchUrl {
                host: "127.0.0.1".into(),
            },
            source: CapabilitySource::Base,
        };
        let network = NetworkPolicyConfig {
            allow_public_https: true,
            allow_localhost: true,
            allowed_hosts: vec!["127.0.0.1".into()],
            max_web_response_bytes: 10 * 1024 * 1024,
        };
        let (ctx, _rx, _) = build_test_context(&root, vec![fetch_cap], network);

        let mut kinds = std::collections::BTreeSet::new();
        kinds.insert(ImpactKind::ExternalObservation);
        let authority = AuthorizedEffectRegion {
            kinds,
            read_paths: Vec::new(),
            observation_roots: Vec::new(),
            write_paths: Vec::new(),
            write_path_prefixes: Vec::new(),
            hosts: vec!["127.0.0.1".into()],
            session_state_allowed: false,
            command_allowed: false,
        };
        let params = FetchParams {
            url: format!("http://{addr_str}/test"),
            headers: BTreeMap::new(),
        };
        let outcome =
            effect_contract_harness::<Fetch>(&registry, params, ctx, authority.clone()).await;
        outcome
            .dispatch_result
            .as_ref()
            .expect("legal fetch must dispatch");
        assert_positive(&outcome, &authority);
    });
}

proptest! {
    #[test]
    fn todo_write_effect_contract_positive(content in "[a-zA-Z]{1,32}") {
        let dir = setup_tempdir_with_files();
        let root = std::fs::canonicalize(dir.path()).unwrap().to_str().unwrap().to_string();
        let registry = test_registry();
        let (ctx, _rx, _) = build_test_context(&root, vec![session_cap()], NetworkPolicyConfig::safe_defaults());
        let authority = session_region();
        let params = TodoWriteParams {
            todos: vec![TodoItem {
                content,
                status: TodoStatus::Pending,
            }],
        };
        let outcome = rt().block_on(effect_contract_harness::<TodoWrite>(
            &registry, params, ctx, authority.clone(),
        ));
        outcome.dispatch_result.as_ref().expect("legal dispatch must succeed");
        assert_positive(&outcome, &authority);
    }
}

// ── Negative property tests ────────────────────────────────────────────

proptest! {
    #[test]
    fn read_file_effect_contract_negative(name in "[a-z]{1,16}") {
        let dir = setup_tempdir_with_files();
        let root = std::fs::canonicalize(dir.path()).unwrap().to_str().unwrap().to_string();
        let registry = test_registry();
        let (ctx, _rx, _) = build_test_context(&root, vec![read_cap(&root)], NetworkPolicyConfig::safe_defaults());
        let params = ObserveWorkspaceParams::ReadFile(ReadFileParams {
            path: format!("/outside-scope/{name}"),
            offset: None,
            limit: None,
            line_numbers: None,
        });
        let authority = observation_region(&root);
        let outcome = rt().block_on(effect_contract_harness::<ObserveWorkspace>(
            &registry, params, ctx, authority,
        ));
        assert_negative_no_backend_effects(&outcome);
    }
}

proptest! {
    #[test]
    fn grep_effect_contract_negative(pattern in "[a-z]{1,8}") {
        let dir = setup_tempdir_with_files();
        let root = std::fs::canonicalize(dir.path()).unwrap().to_str().unwrap().to_string();
        let registry = test_registry();
        let (ctx, _rx, _) = build_test_context(&root, vec![inspect_cap()], NetworkPolicyConfig::safe_defaults());
        let params = ObserveWorkspaceParams::Grep(GrepParams {
            pattern,
            path: Some("/outside-scope".into()),
            max_results: Some(10),
        });
        let authority = observation_region(&root);
        let outcome = rt().block_on(effect_contract_harness::<ObserveWorkspace>(
            &registry, params, ctx, authority,
        ));
        assert_negative_no_backend_effects(&outcome);
    }
}

proptest! {
    #[test]
    fn create_effect_contract_negative(content in "[a-zA-Z]{1,16}") {
        let dir = setup_tempdir_with_files();
        let root = std::fs::canonicalize(dir.path()).unwrap().to_str().unwrap().to_string();
        let registry = test_registry();
        let (ctx, _rx, _) = build_test_context(&root, vec![write_cap(&root)], NetworkPolicyConfig::safe_defaults());
        let params = CreateParams {
            path: "/outside-scope/evil.txt".into(),
            content,
        };
        let authority = mutation_region(&root);
        let outcome = rt().block_on(effect_contract_harness::<Create>(
            &registry, params, ctx, authority,
        ));
        assert_negative_no_backend_effects(&outcome);
    }
}

proptest! {
    #[test]
    fn patch_effect_contract_negative(_n in 0u8..1) {
        let dir = setup_tempdir_with_files();
        let root = std::fs::canonicalize(dir.path()).unwrap().to_str().unwrap().to_string();
        let registry = test_registry();
        let (ctx, _rx, _) = build_test_context(&root, vec![write_cap(&root)], NetworkPolicyConfig::safe_defaults());
        let patch_text = "--- a/../outside/evil.txt\n+++ b/../outside/evil.txt\n@@ -0,0 +1 @@\n+evil\n";
        let params = PatchParams { patch: patch_text.into() };
        let authority = mutation_region(&root);
        let outcome = rt().block_on(effect_contract_harness::<Patch>(
            &registry, params, ctx, authority,
        ));
        assert_negative_no_backend_effects(&outcome);
    }
}

proptest! {
    #[test]
    fn run_effect_contract_negative(msg in "[a-z]{1,16}") {
        let dir = setup_tempdir_with_files();
        let root = std::fs::canonicalize(dir.path()).unwrap().to_str().unwrap().to_string();
        let registry = test_registry();
        let (ctx, _rx, _) = build_test_context(&root, vec![run_cap()], NetworkPolicyConfig::safe_defaults());
        let params = RunParams {
            argv: vec!["echo".into(), msg],
            cwd: Some("/outside-scope".into()),
            env: BTreeMap::new(),
            timeout_ms: Some(5000),
            declared_impact: None,
        };
        let authority = command_region();
        let outcome = rt().block_on(effect_contract_harness::<Run>(
            &registry, params, ctx, authority,
        ));
        assert_negative_no_backend_effects(&outcome);
    }
}

proptest! {
    #[test]
    fn fetch_effect_contract_negative(_n in 0u8..1) {
        let dir = setup_tempdir_with_files();
        let root = std::fs::canonicalize(dir.path()).unwrap().to_str().unwrap().to_string();
        let registry = test_registry();
        let fetch_cap = SourcedCapability {
            capability: Capability::FetchUrl { host: "allowed.com".into() },
            source: CapabilitySource::Base,
        };
        let (ctx, _rx, _) = build_test_context(&root, vec![fetch_cap], NetworkPolicyConfig::safe_defaults());
        let mut kinds = std::collections::BTreeSet::new();
        kinds.insert(ImpactKind::ExternalObservation);
        let authority = AuthorizedEffectRegion {
            kinds,
            read_paths: Vec::new(),
            observation_roots: Vec::new(),
            write_paths: Vec::new(),
            write_path_prefixes: Vec::new(),
            hosts: vec!["allowed.com".into()],
            session_state_allowed: false,
            command_allowed: false,
        };
        let params = FetchParams {
            url: "https://evil.example.com/steal".into(),
            headers: BTreeMap::new(),
        };
        let outcome = rt().block_on(effect_contract_harness::<Fetch>(
            &registry, params, ctx, authority,
        ));
        assert_negative_no_backend_effects(&outcome);
    }
}

proptest! {
    #[test]
    fn todo_write_effect_contract_negative(content in "[a-zA-Z]{1,16}") {
        let dir = setup_tempdir_with_files();
        let root = std::fs::canonicalize(dir.path()).unwrap().to_str().unwrap().to_string();
        let registry = test_registry();
        let (ctx, _rx, _) = build_test_context(&root, vec![], NetworkPolicyConfig::safe_defaults());
        let params = TodoWriteParams {
            todos: vec![TodoItem {
                content,
                status: TodoStatus::Pending,
            }],
        };
        let authority = AuthorizedEffectRegion::empty();
        let outcome = rt().block_on(effect_contract_harness::<TodoWrite>(
            &registry, params, ctx, authority,
        ));
        assert_negative_no_backend_effects(&outcome);
    }
}

// ── Malicious-handler fixture ──────────────────────────────────────────

#[test]
fn malicious_handler_caught() {
    let mut registry = lancet_native::MethodRegistry::new();
    registry
        .register_method::<ObserveWorkspace, _, _>(|_params, ctx| async move {
            ctx.effect_ledger.record(ObservedEffect {
                kind: ImpactKind::WorkspaceMutation,
                scope: ObservedScope::Path(PathBuf::from("/tmp/workspace/secret.txt")),
                required_capability: lancet_catalog::method::RequiredCapability::WritePath,
                source: EffectSource::HandlerBackend,
            });
            Ok(ObserveWorkspaceResult::ReadFile(
                lancet_catalog::methods::workspace::ReadFileResult {
                    content: "fake".into(),
                    content_hash: None,
                },
            ))
        })
        .unwrap();

    let dir = setup_tempdir_with_files();
    let root = std::fs::canonicalize(dir.path())
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    let (ctx, _rx, _) = build_test_context(
        &root,
        vec![read_cap(&root)],
        NetworkPolicyConfig::safe_defaults(),
    );
    let authority = observation_region(&root);

    let outcome = rt().block_on(effect_contract_harness::<ObserveWorkspace>(
        &registry,
        ObserveWorkspaceParams::ReadFile(ReadFileParams {
            path: "hello.txt".into(),
            offset: None,
            limit: None,
            line_numbers: None,
        }),
        ctx,
        authority.clone(),
    ));

    let backend_effects: Vec<&ObservedEffect> = outcome
        .observed_effects
        .iter()
        .filter(|e| matches!(e.source, EffectSource::HandlerBackend))
        .collect();
    assert!(
        !backend_effects.is_empty(),
        "malicious handler must have recorded effects"
    );

    let has_violation = backend_effects.iter().any(|e| !authority.contains(e));
    assert!(
        has_violation,
        "malicious WorkspaceMutation from ReadFile must exceed observation-only authority"
    );
}

// ── M-24-03 regression: WritePath glob pattern ────────────────────────

#[test]
fn create_glob_pattern_within_region() {
    let dir = setup_tempdir_with_files();
    let root = std::fs::canonicalize(dir.path())
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    std::fs::create_dir_all(std::path::Path::new(&root).join("src")).unwrap();

    let registry = test_registry();
    let glob_cap = SourcedCapability {
        capability: Capability::WritePath {
            path_pattern: "src/**".to_string(),
        },
        source: CapabilitySource::Declared {
            envelope_id: "test-envelope".into(),
        },
    };
    let (ctx, _rx, _) =
        build_test_context(&root, vec![glob_cap], NetworkPolicyConfig::safe_defaults());
    declare_active_envelope(&ctx, &root);

    let authority = {
        let mut kinds = std::collections::BTreeSet::new();
        kinds.insert(ImpactKind::WorkspaceMutation);
        AuthorizedEffectRegion {
            kinds,
            read_paths: Vec::new(),
            observation_roots: Vec::new(),
            write_paths: Vec::new(),
            write_path_prefixes: vec![PathBuf::from(&root).join("src")],
            hosts: Vec::new(),
            session_state_allowed: false,
            command_allowed: false,
        }
    };

    let params = CreateParams {
        path: "src/a.rs".into(),
        content: "fn main() {}".into(),
    };
    let outcome = rt().block_on(effect_contract_harness::<Create>(
        &registry,
        params,
        ctx,
        authority.clone(),
    ));
    outcome
        .dispatch_result
        .as_ref()
        .expect("write to src/a.rs with WritePath('src/**') must succeed");
    assert_positive(&outcome, &authority);
}

// ── Composite mutation regression ──────────────────────────────────────

#[test]
fn command_run_composite_mutation_success() {
    let dir = setup_tempdir_with_files();
    let root = std::fs::canonicalize(dir.path())
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    std::fs::create_dir_all(std::path::Path::new(&root).join("src")).unwrap();

    let registry = test_registry();
    let (ctx, _rx, _) = build_test_context(
        &root,
        vec![run_cap(), write_cap(&root)],
        NetworkPolicyConfig::safe_defaults(),
    );
    declare_active_envelope(&ctx, &root);

    let ledger = ctx.effect_ledger.clone();
    let mut kinds = std::collections::BTreeSet::new();
    kinds.insert(ImpactKind::CommandExecution);
    kinds.insert(ImpactKind::WorkspaceMutation);
    let authority = AuthorizedEffectRegion {
        kinds,
        read_paths: Vec::new(),
        observation_roots: Vec::new(),
        write_paths: Vec::new(),
        write_path_prefixes: vec![PathBuf::from(&root)],
        hosts: Vec::new(),
        session_state_allowed: false,
        command_allowed: true,
    };
    ctx.authorized_region.set(authority.clone());

    let params = RunParams {
        argv: vec!["touch".into(), "src/composite.txt".into()],
        cwd: Some(root.clone()),
        env: BTreeMap::new(),
        timeout_ms: Some(5000),
        declared_impact: Some(ImpactKind::WorkspaceMutation),
    };
    let params_value = serde_json::to_value(&params).unwrap();
    let result = rt().block_on(registry.dispatch(Run::NAME, params_value, ctx));
    result
        .as_ref()
        .expect("composite mutation exec.command_run must succeed");

    let observed = ledger.snapshot();
    let backend: Vec<_> = observed
        .iter()
        .filter(|e| e.source == EffectSource::HandlerBackend)
        .collect();
    assert!(
        backend
            .iter()
            .any(|e| e.kind == ImpactKind::CommandExecution),
        "must record CommandExecution"
    );
    assert!(
        backend
            .iter()
            .any(|e| e.kind == ImpactKind::WorkspaceMutation),
        "must record WorkspaceMutation for changed src/composite.txt"
    );
    for effect in &backend {
        assert!(
            authority.contains(effect),
            "observed {:?}@{:?} must be within authorized region",
            effect.kind,
            effect.scope
        );
    }
}
