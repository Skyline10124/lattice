use super::super::fixtures::*;
use super::super::support::build_test_context;
use lancet_authority::NetworkPolicyConfig;
use lancet_catalog::methods::envelope::{DeclareParams, EnvelopeParams};
use lancet_catalog::methods::session::{TodoItem, TodoStatus, TodoWriteParams};
use lancet_catalog::methods::*;

pub(in crate::effect_contract_tests) fn todo_write_runtime_effects() -> Vec<RuntimeEffectScenario> {
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

pub(in crate::effect_contract_tests) fn envelope_runtime_effects() -> Vec<RuntimeEffectScenario> {
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
