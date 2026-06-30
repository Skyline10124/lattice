// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use super::super::fixtures::*;
use super::super::support::build_test_context;
use lancet_authority::NetworkPolicyConfig;
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

    let (ctx, _rx, _) = build_test_context(&root, vec![], NetworkPolicyConfig::safe_defaults());
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

    let (ctx, _rx, _) = build_test_context(&root, vec![], NetworkPolicyConfig::safe_defaults());
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
