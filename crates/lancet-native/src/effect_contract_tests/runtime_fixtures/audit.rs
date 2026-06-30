// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use super::super::fixtures::*;
use super::super::support::{build_test_context, rt};
use lancet_audit::{AuditOutcome, CallAuditMetadata};
use lancet_authority::NetworkPolicyConfig;
use lancet_authority::effect::region::AuthorizedEffectRegion;
use lancet_catalog::CepMethod;
use lancet_catalog::methods::audit::{AuditObserveParams, AuditQueryParams, AuditReplayParams};
use lancet_catalog::methods::*;

pub(in crate::effect_contract_tests) fn audit_observe_runtime_effects() -> Vec<RuntimeEffectScenario>
{
    let dir = setup_tempdir_with_files();
    let root = std::fs::canonicalize(dir.path())
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    let mut scenarios = Vec::new();

    let (ctx, _rx, _) = build_test_context(&root, vec![], NetworkPolicyConfig::safe_defaults());
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

    let (ctx, _rx, _) = build_test_context(&root, vec![], NetworkPolicyConfig::safe_defaults());
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
        "happy_query_no_caps",
        dispatch_success_effects::<AuditObserve>(
            AuditObserveParams {
                stream: false,
                query: AuditQueryParams::default(),
            },
            ctx,
            AuthorizedEffectRegion::empty(),
        ),
    ));

    scenarios
}

fn seed_audit_replay_row(ctx: &crate::server::registry::HandlerContext) {
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

pub(in crate::effect_contract_tests) fn audit_replay_runtime_effects() -> Vec<RuntimeEffectScenario>
{
    let dir = setup_tempdir_with_files();
    let root = std::fs::canonicalize(dir.path())
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    let mut scenarios = Vec::new();

    let (ctx, _rx, _) = build_test_context(&root, vec![], NetworkPolicyConfig::safe_defaults());
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

    let (ctx, _rx, _) = build_test_context(&root, vec![], NetworkPolicyConfig::safe_defaults());
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
