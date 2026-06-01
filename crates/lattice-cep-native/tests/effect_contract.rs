//! Effect-region contract tests.
//!
//! Per-method positive + negative property tests via the reusable
//! [`effect_contract_harness`], plus a deterministic malicious-handler
//! fixture and the composite command-mutation audit test.

use std::collections::BTreeMap;
use std::path::PathBuf;

use proptest::prelude::*;
use tempfile::TempDir;

use lattice_cep_catalog::methods::command::RunParams;
use lattice_cep_catalog::methods::external::FetchParams;
use lattice_cep_catalog::methods::session::{TodoItem, TodoStatus, TodoWriteParams};
use lattice_cep_catalog::methods::workspace::{
    GrepParams, PatchParams, ReadFileParams, WriteFileParams,
};
use lattice_cep_catalog::methods::*;
use lattice_cep_catalog::CepMethod;
use lattice_cep_native::effect::ledger::{EffectSource, ObservedEffect, ObservedScope};
use lattice_cep_native::effect::region::AuthorizedEffectRegion;
use lattice_cep_native::testing::effect_harness::{
    assert_negative_no_backend_effects, assert_positive, build_test_context,
    effect_contract_harness, rt,
};
use lattice_cep_protocol::envelope::{
    Capability, Constraints, ExecutionEnvelope, ImpactKind, Intent,
};
use lattice_cep_protocol::policy::{CapabilitySource, SourcedCapability};

use lattice_cep_native::policy::NetworkPolicyConfig;

// ── Helper builders ────────────────────────────────────────────────────

fn test_registry() -> lattice_cep_native::MethodRegistry {
    let mut registry = lattice_cep_native::MethodRegistry::new();
    lattice_cep_native::register_production_method_registry(&mut registry).unwrap();
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
    SourcedCapability {
        capability: Capability::RunCommand {
            argv_allowlist: vec!["echo".into(), "touch".into()],
        },
        source: CapabilitySource::Declared {
            envelope_id: "test-envelope".into(),
        },
    }
}

fn declare_active_envelope(ctx: &lattice_cep_native::registry::HandlerContext, root: &str) {
    use lattice_cep_protocol::envelope::{
        Impact as EnvImpact, ImpactConfidence, Scope as EnvScope,
    };
    let mut caps = std::collections::BTreeSet::new();
    caps.insert(Capability::WritePath {
        path_pattern: ".".to_string(),
    });
    caps.insert(Capability::RunCommand {
        argv_allowlist: vec!["echo".into(), "touch".into()],
    });
    let envelope = ExecutionEnvelope {
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
        verify: None,
        on_failure: None,
        extensions: serde_json::Map::new(),
    };
    rt().block_on(ctx.active_envelope.declare(envelope, 0, "test-turn"))
        .unwrap();
}

fn session_cap() -> SourcedCapability {
    SourcedCapability {
        capability: Capability::MutateSessionState,
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

fn session_region() -> AuthorizedEffectRegion {
    let mut kinds = std::collections::BTreeSet::new();
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

fn setup_tempdir_with_files() -> TempDir {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("hello.txt"), "hello\nworld\n").unwrap();
    std::fs::write(dir.path().join("data.txt"), "some data content\n").unwrap();
    std::fs::create_dir_all(dir.path().join("subdir")).unwrap();
    std::fs::write(dir.path().join("subdir/nested.txt"), "nested\n").unwrap();
    dir
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
        let params = ReadFileParams {
            path: "hello.txt".into(),
            offset: Some(offset),
            limit: Some(limit),
            line_numbers: None,
        };
        let outcome = rt().block_on(effect_contract_harness::<ReadFile>(
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
        let params = GrepParams {
            pattern,
            path: None,
            max_results: Some(10),
        };
        let outcome = rt().block_on(effect_contract_harness::<Grep>(
            &registry, params, ctx, authority.clone(),
        ));
        outcome.dispatch_result.as_ref().expect("legal dispatch must succeed");
        assert_positive(&outcome, &authority);
    }
}

proptest! {
    #[test]
    fn write_file_effect_contract_positive(content in "[a-zA-Z0-9 ]{1,64}") {
        let dir = setup_tempdir_with_files();
        let root = std::fs::canonicalize(dir.path()).unwrap().to_str().unwrap().to_string();
        let registry = test_registry();
        let (ctx, _rx, _) = build_test_context(&root, vec![write_cap(&root)], NetworkPolicyConfig::safe_defaults());
        declare_active_envelope(&ctx, &root);
        let authority = mutation_region(&root);
        let params = WriteFileParams {
            path: "output.txt".into(),
            content,
        };
        let outcome = rt().block_on(effect_contract_harness::<WriteFile>(
            &registry, params, ctx, authority.clone(),
        ));
        outcome.dispatch_result.as_ref().expect("legal write_file must dispatch");
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

        let (ctx, _rx, _) = build_test_context(&root, vec![write_cap(&root)], NetworkPolicyConfig::safe_defaults());
        declare_active_envelope(&ctx, &root);
        let authority = mutation_region(&root);
        let patch_text = format!(
            "*** Begin Patch\n*** Update File: hello.txt\n*** Old Hash: blake3:{hash_hex}\n@@\n hello\n-world\n+patched\n*** End Patch\n"
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
        let params = ReadFileParams {
            path: format!("/outside-scope/{name}"),
            offset: None,
            limit: None,
            line_numbers: None,
        };
        let authority = observation_region(&root);
        let outcome = rt().block_on(effect_contract_harness::<ReadFile>(
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
        let params = GrepParams {
            pattern,
            path: Some("/outside-scope".into()),
            max_results: Some(10),
        };
        let authority = observation_region(&root);
        let outcome = rt().block_on(effect_contract_harness::<Grep>(
            &registry, params, ctx, authority,
        ));
        assert_negative_no_backend_effects(&outcome);
    }
}

proptest! {
    #[test]
    fn write_file_effect_contract_negative(content in "[a-zA-Z]{1,16}") {
        let dir = setup_tempdir_with_files();
        let root = std::fs::canonicalize(dir.path()).unwrap().to_str().unwrap().to_string();
        let registry = test_registry();
        let (ctx, _rx, _) = build_test_context(&root, vec![write_cap(&root)], NetworkPolicyConfig::safe_defaults());
        let params = WriteFileParams {
            path: "/outside-scope/evil.txt".into(),
            content,
        };
        let authority = mutation_region(&root);
        let outcome = rt().block_on(effect_contract_harness::<WriteFile>(
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
    let mut registry = lattice_cep_native::MethodRegistry::new();
    registry
        .register_method::<ReadFile, _, _>(|_params, ctx| async move {
            ctx.effect_ledger.record(ObservedEffect {
                kind: ImpactKind::WorkspaceMutation,
                scope: ObservedScope::Path(PathBuf::from("/tmp/workspace/secret.txt")),
                required_capability: lattice_cep_catalog::method::RequiredCapability::WritePath,
                source: EffectSource::HandlerBackend,
            });
            Ok(lattice_cep_catalog::methods::workspace::ReadFileResult {
                content: "fake".into(),
                content_hash: None,
            })
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

    let outcome = rt().block_on(effect_contract_harness::<ReadFile>(
        &registry,
        ReadFileParams {
            path: "hello.txt".into(),
            offset: None,
            limit: None,
            line_numbers: None,
        },
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
fn write_file_glob_pattern_within_region() {
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

    let params = WriteFileParams {
        path: "src/a.rs".into(),
        content: "fn main() {}".into(),
    };
    let outcome = rt().block_on(effect_contract_harness::<WriteFile>(
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
        .expect("composite mutation command.run must succeed");

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
