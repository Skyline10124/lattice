// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use std::collections::BTreeMap;

use proptest::prelude::*;

use super::fixtures::*;
use super::support::{
    assert_negative_no_backend_effects, assert_positive, build_test_context,
    effect_contract_harness, rt,
};
use lancet_authority::NetworkPolicyConfig;
use lancet_authority::effect::region::AuthorizedEffectRegion;
use lancet_catalog::methods::command::RunParams;
use lancet_catalog::methods::external::FetchParams;
use lancet_catalog::methods::session::{TodoItem, TodoStatus, TodoWriteParams};
use lancet_catalog::methods::workspace::{
    CreateParams, EditOperation, EditParams, GrepParams, ObserveWorkspaceParams, ReadFileParams,
};
use lancet_catalog::methods::*;
use lancet_protocol::envelope::{Capability, ImpactKind};
use lancet_protocol::policy::{CapabilitySource, SourcedCapability};

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
            create_parents: false,
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
    fn edit_effect_contract_positive(_n in 0u8..1) {
        let dir = setup_tempdir_with_files();
        let root = std::fs::canonicalize(dir.path()).unwrap().to_str().unwrap().to_string();
        let registry = test_registry();

        let content = std::fs::read(dir.path().join("hello.txt")).unwrap();
        let hash = blake3::hash(&content);
        let hash_hex = hash.to_hex();

        let (ctx, _rx, _) = build_test_context(&root, vec![write_cap(&root)], NetworkPolicyConfig::safe_defaults());
        declare_active_envelope(&ctx, &root);
        let authority = mutation_region(&root);
        let params = EditParams {
            path: "hello.txt".into(),
            expected_hash: format!("blake3:{hash_hex}"),
            edits: vec![EditOperation::ReplaceText {
                old: "world".into(),
                new: "patched".into(),
                occurrence: None,
            }],
        };
        let outcome = rt().block_on(effect_contract_harness::<Edit>(
            &registry, params, ctx, authority.clone(),
        ));
        outcome.dispatch_result.as_ref().expect("legal edit must dispatch");
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
            create_parents: false,
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
    fn edit_effect_contract_negative(_n in 0u8..1) {
        let dir = setup_tempdir_with_files();
        let root = std::fs::canonicalize(dir.path()).unwrap().to_str().unwrap().to_string();
        let registry = test_registry();
        let (ctx, _rx, _) = build_test_context(&root, vec![write_cap(&root)], NetworkPolicyConfig::safe_defaults());
        let params = EditParams {
            path: "../outside/evil.txt".into(),
            expected_hash: "blake3:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
            edits: vec![EditOperation::ReplaceText {
                old: "x".into(),
                new: "evil".into(),
                occurrence: None,
            }],
        };
        let authority = mutation_region(&root);
        let outcome = rt().block_on(effect_contract_harness::<Edit>(
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
