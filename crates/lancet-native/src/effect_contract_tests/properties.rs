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
use lancet_catalog::methods::external::FetchParams;
use lancet_catalog::methods::*;
use lancet_protocol::effect_types::ImpactKind;

// The `read_file`/`grep` positive and negative properties are gone (#30):
// `observe.workspace` is an embedded Component Tool with no native handler, so
// there is nothing left for this harness to dispatch. Its effect contract is
// enforced host-side in `lancet_runtime::host_workspace` and covered by the
// Component's own tests, not by the native `MethodRegistry`. What survives here
// is the contract for the one method a native handler still serves (`fetch.web`).

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

        let network = NetworkPolicyConfig {
            allow_public_https: true,
            allow_localhost: true,
            allowed_hosts: vec!["127.0.0.1".into()],
            max_web_response_bytes: 10 * 1024 * 1024,
        };
        let (ctx, _rx, _) = build_test_context(&root, vec![], network);

        let mut kinds = std::collections::BTreeSet::new();
        kinds.insert(ImpactKind::ExternalObservation);
        let authority = AuthorizedEffectRegion {
            kinds,
            read_paths: Vec::new(),
            observation_roots: Vec::new(),
            denied_write_paths: Vec::new(),
            denied_write_path_prefixes: Vec::new(),
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

// ── Negative property tests ────────────────────────────────────────────


proptest! {
    #[test]
    fn fetch_effect_contract_negative(_n in 0u8..1) {
        let dir = setup_tempdir_with_files();
        let root = std::fs::canonicalize(dir.path()).unwrap().to_str().unwrap().to_string();
        let registry = test_registry();
        // Deny at network-policy preflight so the test never performs DNS/HTTP.
        // safe_defaults() allows any public https host, which would hang here on
        // a real connect timeout to evil.example.com.
        let network = NetworkPolicyConfig {
            allow_public_https: true,
            allow_localhost: false,
            allowed_hosts: vec!["allowed.com".into()],
            max_web_response_bytes: 10 * 1024 * 1024,
        };
        let (ctx, _rx, _) = build_test_context(&root, vec![], network);
        let mut kinds = std::collections::BTreeSet::new();
        kinds.insert(ImpactKind::ExternalObservation);
        let authority = AuthorizedEffectRegion {
            kinds,
            read_paths: Vec::new(),
            observation_roots: Vec::new(),
            denied_write_paths: Vec::new(),
            denied_write_path_prefixes: Vec::new(),
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
        assert!(
            outcome.dispatch_result.is_err(),
            "disallowed host must be denied at preflight"
        );
        assert_negative_no_backend_effects(&outcome);
    }
}

// ── Malicious-handler fixture ──────────────────────────────────────────
