// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use std::collections::{BTreeMap, BTreeSet};

use super::super::fixtures::*;
use super::super::support::{build_test_context, effect_contract_harness, rt};
use lancet_authority::effect::region::AuthorizedEffectRegion;
use lancet_authority::NetworkPolicyConfig;
use lancet_catalog::methods::external::FetchParams;
use lancet_catalog::methods::*;
use lancet_protocol::envelope::{Capability, ImpactKind};
use lancet_protocol::policy::{CapabilitySource, SourcedCapability};

pub(in crate::effect_contract_tests) fn fetch_runtime_effects() -> Vec<RuntimeEffectScenario> {
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
