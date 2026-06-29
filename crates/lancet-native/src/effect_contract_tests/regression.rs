// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use std::path::PathBuf;

use super::fixtures::*;
use super::support::{assert_positive, build_test_context, effect_contract_harness, rt};
use lancet_authority::NetworkPolicyConfig;
use lancet_authority::effect::ledger::{EffectSource, ObservedEffect, ObservedScope};
use lancet_authority::effect::region::AuthorizedEffectRegion;
use lancet_catalog::methods::workspace::{
    CreateParams, ObserveWorkspaceParams, ObserveWorkspaceResult, ReadFileParams,
};
use lancet_catalog::methods::*;
use lancet_protocol::effect_types::ImpactKind;

#[test]
fn malicious_handler_caught() {
    let mut registry = crate::MethodRegistry::new();
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
        vec![],
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

// ── regression: WritePath glob pattern ────────────────────────────────

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
    let (ctx, _rx, _) =
        build_test_context(&root, vec![], NetworkPolicyConfig::safe_defaults());
    declare_active_envelope(&ctx, &root);

    let authority = {
        let mut kinds = std::collections::BTreeSet::new();
        kinds.insert(ImpactKind::WorkspaceMutation);
        AuthorizedEffectRegion {
            kinds,
            read_paths: Vec::new(),
            observation_roots: Vec::new(),
            denied_write_paths: Vec::new(),
            // Blacklist semantics: no denied patterns → all paths allowed
            denied_write_path_prefixes: Vec::new(),
            hosts: Vec::new(),
            session_state_allowed: false,
            command_allowed: false,
        }
    };

    let params = CreateParams {
        path: "src/a.rs".into(),
        content: "fn main() {}".into(),
        create_parents: false,
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
        .expect("write to src/a.rs with no denied patterns must succeed");
    assert_positive(&outcome, &authority);
}
