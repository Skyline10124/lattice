// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0
use super::*;
use serde_json::json;
#[test]
fn json_updates_keep_submission_receipt_and_error_shapes() {
    let id = lattice_api::local::new_turn_id();
    let sid = lattice_api::local::new_session_id();
    let error: LocalError = serde_json::from_value(
        json!({"kind":"bad_request","message":"exact rejection","recovery":null,"store":null}),
    )
    .unwrap();
    let updates = vec![
        LocalUpdate::Submit {
            update: SubmissionUpdate::Sending {
                turn_id: id,
                session_id: sid,
                new: true,
            },
        },
        LocalUpdate::Submit {
            update: SubmissionUpdate::Receipt(SubmissionReceipt {
                turn_id: id,
                result: SubmissionResult::Rejected {
                    error: error.clone(),
                },
                recovered: false,
            }),
        },
        LocalUpdate::Cancel {
            turn_id: id,
            status: CancelStatus::Taken,
            owned: Some(true),
            error: None,
        },
        LocalUpdate::Head(ipc_tests::head(id, false)),
        LocalUpdate::Entry {
            entry: ipc_tests::entry(id, "completed"),
        },
        LocalUpdate::Text(TextUpdate {
            node: NodeId(2),
            attempt: 0,
            offset: 0,
            segments: vec![TextSegment::Text {
                text: "live".into(),
            }],
        }),
        LocalUpdate::Connection {
            status: ConnectionStatus::Eof,
        },
        LocalUpdate::Error { error },
    ];
    let mut out = Vec::new();
    let mut err = Vec::new();
    let mut state = RunRenderState::default();
    for update in &updates {
        render_update(update, &mut state, &mut out, &mut err, true, true).unwrap();
    }
    let records: Vec<serde_json::Value> = String::from_utf8(out)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(
        records,
        updates
            .iter()
            .map(|u| serde_json::to_value(u).unwrap())
            .collect::<Vec<_>>(),
        "每条更新原样交付，sending 后以 receipt 结束"
    );
    assert!(err.is_empty());
    assert_eq!(
        records
            .iter()
            .map(|r| r["type"].as_str().unwrap())
            .collect::<Vec<_>>(),
        [
            "submit",
            "submit",
            "cancel",
            "head",
            "entry",
            "text",
            "connection",
            "error"
        ]
    );
}

#[test]
fn human_rejected_receipt_keeps_legacy_store_paths() {
    let id = lattice_api::local::new_turn_id();
    let paths = [
        "/home/test/state/lattice/sessions.git",
        "/home/test/state/lattice/projects/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa/invocations.git",
    ];
    let error:LocalError=serde_json::from_value(json!({"kind":"store_unavailable","message":"session store unavailable","store":{"type":"legacy_stores","paths":paths}})).unwrap();
    let receipt = SubmissionReceipt {
        turn_id: id,
        result: SubmissionResult::Rejected { error },
        recovered: false,
    };
    let mut state = RunRenderState::default();
    let mut out = Vec::new();
    let mut err = Vec::new();
    render_update(
        &LocalUpdate::Submit {
            update: SubmissionUpdate::Receipt(receipt.clone()),
        },
        &mut state,
        &mut out,
        &mut err,
        false,
        false,
    )
    .unwrap();
    let text = String::from_utf8(err).unwrap();
    for path in paths {
        assert!(
            text.contains(path),
            "人类拒绝回执必须保留旧库路径 {path}：{text}"
        );
    }
    assert!(text.contains("session store unavailable"));
    assert!(text.contains(&id.to_string()));
    assert!(out.is_empty());
    assert_eq!(
        receipt_exit(&receipt.result),
        std::process::ExitCode::from(1)
    );
}
#[test]
fn human_entries_replace_streamed_text_and_print_one_final_summary() {
    let id = lattice_api::local::new_turn_id();
    let mut out = Vec::new();
    let mut err = Vec::new();
    let mut state = RunRenderState::default();
    let updates = [
        LocalUpdate::Head(ipc_tests::head(id, true)),
        LocalUpdate::Text(TextUpdate {
            node: NodeId(2),
            attempt: 0,
            offset: 0,
            segments: vec![
                TextSegment::Text { text: "ans".into() },
                TextSegment::Reasoning {
                    text: "think".into(),
                },
            ],
        }),
        LocalUpdate::Entry {
            entry: ipc_tests::entry(id, "completed"),
        },
        LocalUpdate::Entry {
            entry: ipc_tests::entry(id, "completed"),
        },
    ];
    for update in &updates {
        render_update(update, &mut state, &mut out, &mut err, true, false).unwrap();
    }
    finish_human(&state, &mut out, &mut err, true).unwrap();
    assert_eq!(
        String::from_utf8(out).unwrap(),
        "answer\n",
        "最终条目只补流式正文的剩余部分"
    );
    let err = String::from_utf8(err).unwrap();
    assert_eq!(err.lines().filter(|l| *l == "completed").count(), 1);
    assert!(err.contains("reasoning: think"));
    assert_eq!(
        summary_exit(state.summary.as_ref()),
        std::process::ExitCode::SUCCESS
    );
}
#[test]
fn durable_attempt_usage_is_reported_once_with_partial_totals() {
    let id = lattice_api::local::new_turn_id();
    let mut node = ipc_tests::entry(id, "completed");
    if let ConversationEntry::Step { step, .. } = node.as_mut() {
        let attempt = |n, usage| {
            serde_json::from_value(json!({"attempt":n,"origin":"open_ai_compatible","usage":usage,"progress":{"progress":"ended","end":{"end":"completed"},"diagnostic":null},"end":{"end":"stopped","cause":{"cause":"process_exit"},"diagnostic":null},"retry_delay_ms":null})).unwrap()
        };
        step.attempts = vec![
            attempt(
                0,
                json!({"prompt_tokens":20,"completion_tokens":10,"total_tokens":30}),
            ),
            attempt(1, serde_json::Value::Null),
        ];
    }
    let mut state = RunRenderState::default();
    let mut out = Vec::new();
    let mut err = Vec::new();
    for _ in 0..2 {
        render_update(
            &LocalUpdate::Entry {
                entry: node.clone(),
            },
            &mut state,
            &mut out,
            &mut err,
            true,
            false,
        )
        .unwrap();
    }
    finish_human(&state, &mut out, &mut err, true).unwrap();
    let text = String::from_utf8(err).unwrap();
    assert!(
        text.contains("\"unknown_attempts\":1"),
        "持久 usage 不能被重复 entry 翻倍或丢弃：{text}"
    );
    assert!(text.contains("\"total_tokens\":30"));
}

#[test]
fn human_tool_results_wait_for_terminal_slots_or_execution_end() {
    let id = lattice_api::local::new_turn_id();
    let mut node = ipc_tests::entry(id, "completed");
    if let ConversationEntry::Step {
        content: Node::Step {
            progress, slots, ..
        },
        summary,
        ..
    } = node.as_mut()
    {
        *progress = serde_json::from_value(json!({"progress":"executing"})).unwrap();
        *summary = None;
        slots.push(serde_json::from_value(json!({"binding":{"invocation_id":"10000000-0000-4000-8000-000000000001","identity":{"session_id":ipc_tests::sid(),"turn_id":id,"model_step_ordinal":0,"part_ordinal":0,"method":"observe.workspace","descriptor_identity":"test","arguments_digest":"00".repeat(32)}},"progress":{"progress":"registered"}})).unwrap());
    }
    let mut state = RunRenderState::default();
    let mut out = Vec::new();
    let mut err = Vec::new();
    render_update(
        &LocalUpdate::Head(ipc_tests::head(id, true)),
        &mut state,
        &mut out,
        &mut err,
        false,
        false,
    )
    .unwrap();
    render_update(
        &LocalUpdate::Entry {
            entry: node.clone(),
        },
        &mut state,
        &mut out,
        &mut err,
        false,
        false,
    )
    .unwrap();
    assert!(
        !String::from_utf8_lossy(&err).contains("tool "),
        "运行中 Registered 不能提前打印 NotExecuted 并挡住最后结果"
    );
    if let ConversationEntry::Step {
        content: Node::Step { slots, .. },
        ..
    } = node.as_mut()
    {
        slots[0].progress = SlotProgress::Started;
    }
    render_update(
        &LocalUpdate::Head(ipc_tests::head(id, false)),
        &mut state,
        &mut out,
        &mut err,
        false,
        false,
    )
    .unwrap();
    for _ in 0..2 {
        render_update(
            &LocalUpdate::Entry {
                entry: node.clone(),
            },
            &mut state,
            &mut out,
            &mut err,
            false,
            false,
        )
        .unwrap();
    }
    let text = String::from_utf8(err).unwrap();
    assert_eq!(text.lines().filter(|l| l.starts_with("tool ")).count(), 1);
    assert!(text.contains("indeterminate"));
    assert!(!text.contains("not_executed"));
    for (progress, expected) in [
        (SlotProgress::Registered, "not_executed"),
        (SlotProgress::Started, "indeterminate"),
    ] {
        let mut old = node.clone();
        if let ConversationEntry::Step {
            content: Node::Step { slots, .. },
            ..
        } = old.as_mut()
        {
            slots[0].progress = progress;
        }
        // 新观察者尚未显示旧轮的槽位；新一轮的 Owner 不能挡住旧轮的停止结果。
        let mut state = RunRenderState::default();
        let mut out = Vec::new();
        let mut err = Vec::new();
        let mut new_head = ipc_tests::head(lattice_api::local::new_turn_id(), true);
        new_head.position = NodeId(4);
        render_update(
            &LocalUpdate::Head(new_head),
            &mut state,
            &mut out,
            &mut err,
            false,
            false,
        )
        .unwrap();
        for _ in 0..2 {
            render_update(
                &LocalUpdate::Entry { entry: old.clone() },
                &mut state,
                &mut out,
                &mut err,
                false,
                false,
            )
            .unwrap();
        }
        let text = String::from_utf8(err).unwrap();
        assert!(
            text.contains(expected),
            "新轮的 Owner 不能隐藏旧轮 {expected}：{text}"
        );
        assert_eq!(text.lines().filter(|l| l.starts_with("tool ")).count(), 1);
    }
}
