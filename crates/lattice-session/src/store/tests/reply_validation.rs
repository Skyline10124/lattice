// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use super::*;

/// 直接构造 CompleteReply 绕过流层，分别覆盖正文、reasoning、参数、
/// 共享总量和批次边界。外部场景不能注入未经累积器核验的完整消息。
#[test]
fn oversized_complete_replies_are_rejected_without_partial_records() {
    use lattice_conversation::{
        MAX_ASSISTANT_CONTENT_BYTES, MAX_MODEL_ATTEMPT_BYTES, MAX_OPAQUE_REPLAY_STATE_BYTES,
        MAX_REASONING_CONTENT_BYTES, MAX_TOOL_CALL_ARGUMENTS_BYTES,
    };
    let (_dir, layout, db) = store();
    let lock = db.try_lock_session(sid(1)).unwrap();
    accept_prepared(&db, &lock, accept(sid(1), tid(1))).unwrap();
    db.start_request(&lock, key(tid(1), 0, 0), origin(), now())
        .unwrap();
    for registration in [true, false] {
        for case in 0..if registration { 6 } else { 3 } {
            let mut input = reply(key(tid(1), 0, 0), usize::from(registration));
            match (registration, case) {
                (_, 0) => {
                    input.message.parts[0].semantic = MessageSemantic::Text {
                        text: "x".repeat(MAX_ASSISTANT_CONTENT_BYTES + 1),
                    }
                }
                (_, 1) => {
                    input.message.parts[0].semantic = MessageSemantic::Reasoning {
                        text: "r".repeat(MAX_REASONING_CONTENT_BYTES + 1),
                    }
                }
                (true, 2) => {
                    let MessageSemantic::ToolCall { call } = &mut input.message.parts[1].semantic
                    else {
                        unreachable!()
                    };
                    call.arguments = json!({"x":"a".repeat(MAX_TOOL_CALL_ARGUMENTS_BYTES)});
                }
                (true, 3) => {
                    input = reply(key(tid(1), 0, 0), 16);
                    for part in &mut input.message.parts {
                        if let MessageSemantic::ToolCall { call } = &mut part.semantic {
                            call.arguments =
                                json!({"x":"a".repeat(MAX_TOOL_CALL_ARGUMENTS_BYTES - 8)});
                        }
                    }
                }
                (true, 4) | (false, 2) => {
                    input.message.parts.extend(
                        (0..=MAX_MODEL_ATTEMPT_BYTES / MAX_OPAQUE_REPLAY_STATE_BYTES).map(|_| {
                            MessagePart {
                                semantic: MessageSemantic::Text {
                                    text: String::new(),
                                },
                                replay_state: Some(
                                    OpaqueProviderState::new(
                                        "s".repeat(MAX_OPAQUE_REPLAY_STATE_BYTES),
                                    )
                                    .unwrap(),
                                ),
                            }
                        }),
                    );
                }
                (true, 5) => input = reply(key(tid(1), 0, 0), 33),
                _ => unreachable!(),
            }
            let before = records(&db);
            let result = if registration {
                let ids: Vec<_> = (1..=input.message.tool_calls().count() as u64)
                    .map(iid)
                    .collect();
                db.register_calls(&lock, input, &ids, now()).map(|_| ())
            } else {
                db.close_step(&lock, CloseStep::Finish(input), now())
                    .map(|_| ())
            };
            let expected = if registration && case == 5 {
                "invalid tool call batch"
            } else {
                "response byte limit exceeded"
            };
            assert!(
                matches!(&result, Err(StoreError::InvalidInput { field: "reply", detail }) if detail == expected),
                "registration={registration} case={case}: {result:?}"
            );
            assert!(
                before == records(&db),
                "registration={registration} case={case}: partial records"
            );
        }
    }
    let before = records(&db);
    drop(lock);
    drop(db);
    let reopened = SessionStore::open(&layout).unwrap();
    assert!(
        before == records(&reopened),
        "reopening reveals no partial persistent writes"
    );
}

#[test]
fn reply_total_includes_names_and_ids_with_an_exact_boundary_control() {
    use lattice_conversation::MAX_TOOL_CALL_ARGUMENTS_BYTES;
    let (_dir, layout, db) = store();
    let lock = db.try_lock_session(sid(1)).unwrap();
    accept_prepared(&db, &lock, accept(sid(1), tid(1))).unwrap();
    db.start_request(&lock, key(tid(1), 0, 0), origin(), now())
        .unwrap();
    let input = |extra| {
        let mut input = reply(key(tid(1), 0, 0), 16);
        input
            .message
            .parts
            .retain(|part| matches!(part.semantic, MessageSemantic::ToolCall { .. }));
        for (index, part) in input.message.parts.iter_mut().enumerate() {
            let MessageSemantic::ToolCall { call } = &mut part.semantic else {
                unreachable!()
            };
            // 每个调用连名称和 ID 恰好 1 MiB；去掉其中任何一项都会低于总量上限。
            let metadata = call.name.len() + call.provider_call_id.as_ref().unwrap().len();
            let filler =
                MAX_TOOL_CALL_ARGUMENTS_BYTES - 8 - metadata + usize::from(index == 0) * extra;
            call.arguments = json!({"x":"a".repeat(filler)});
            part.replay_state = None;
        }
        input
    };
    let ids: Vec<_> = (1..=16).map(iid).collect();
    let before = records(&db);
    let result = db.register_calls(&lock, input(1), &ids, now());
    assert!(
        matches!(result, Err(StoreError::InvalidInput { field: "reply", detail }) if detail == "response byte limit exceeded")
    );
    assert!(
        before == records(&db),
        "one excess metadata byte must leave no partial writes"
    );
    let registered = db
        .register_calls(&lock, input(0), &ids, now())
        .expect("16 MiB including names and IDs is accepted");
    assert_eq!(registered.bindings.len(), 16);
    let Node::Step {
        reply: Some(saved),
        slots,
        ..
    } = node_at(&db, 2)
    else {
        panic!("registered reply")
    };
    assert!(saved == registered.reply);
    assert_eq!(slots.len(), 16);
    let after = records(&db);
    drop(lock);
    drop(db);
    let reopened = SessionStore::open(&layout).unwrap();
    assert!(
        after == records(&reopened),
        "the accepted boundary reply survives reopening"
    );
}

#[test]
fn deep_arguments_are_rejected_before_commit() {
    let (_dir, layout, db) = store();
    let lock = db.try_lock_session(sid(1)).unwrap();
    accept_prepared(&db, &lock, accept(sid(1), tid(1))).unwrap();
    db.start_request(&lock, key(tid(1), 0, 0), origin(), now())
        .unwrap();
    let before = load_read(&db.begin_read().unwrap().tx).unwrap();
    let mut input = reply(key(tid(1), 0, 0), 1);
    let json = format!("{}0{}", "{\"a\":".repeat(126), "}".repeat(126));
    let MessageSemantic::ToolCall { call } = &mut input.message.parts[1].semantic else {
        unreachable!()
    };
    call.arguments = serde_json::from_str(&json).unwrap();
    let result = db.register_calls(&lock, input, &[iid(1)], now());
    assert!(
        matches!(result, Err(StoreError::InvalidInput { .. })),
        "unreadable arguments were committed: {result:?}"
    );
    assert!(before == load_read(&db.begin_read().unwrap().tx).unwrap());
    let mut readable = reply(key(tid(1), 0, 0), 1);
    let json = format!("{}0{}", "{\"a\":".repeat(80), "}".repeat(80));
    let value: Value = serde_json::from_str(&json).unwrap();
    let MessageSemantic::ToolCall { call } = &mut readable.message.parts[1].semantic else {
        unreachable!()
    };
    call.arguments = value.clone();
    db.register_calls(&lock, readable, &[iid(1)], now())
        .unwrap();
    drop(lock);
    drop(db);
    let reopened = SessionStore::open(&layout).unwrap();
    let Node::Step {
        reply: Some(assistant),
        ..
    } = reopened
        .begin_read()
        .unwrap()
        .node(NodeRef {
            session_id: sid(1),
            node: NodeId(2),
        })
        .unwrap()
    else {
        unreachable!()
    };
    assert_eq!(assistant.tool_calls().next().unwrap().1.arguments, value);
}

#[test]
fn deep_results_are_rejected_before_commit() {
    let (_dir, layout, db) = store();
    let (lock, calls) = running(&db, sid(1), tid(1), 1, 1);
    db.start_invocation(&lock, &calls.bindings[0], now())
        .unwrap();
    let before = load_read(&db.begin_read().unwrap().tx).unwrap();
    for depth in [124, 80] {
        let json = format!("{}0{}", "[".repeat(depth), "]".repeat(depth));
        let value: Value = serde_json::from_str(&json).unwrap();
        let terminal = prepare_terminal(&calls.bindings[0], ToolReply::Succeeded(value.clone()));
        let result = db.finish_invocation(
            &lock,
            TerminalInput {
                invocation_id: iid(1),
                terminal,
            },
            now(),
        );
        if depth == 124 {
            assert!(
                matches!(result, Err(StoreError::InvalidInput { .. })),
                "unreadable result was committed: {result:?}"
            );
            assert!(before == load_read(&db.begin_read().unwrap().tx).unwrap());
        } else {
            result.unwrap();
            db.close_step(
                &lock,
                CloseStep::Continue {
                    key: step(tid(1), 0),
                },
                now(),
            )
            .unwrap();
            let Node::Step {
                reply: Some(_),
                slots,
                ..
            } = db
                .begin_read()
                .unwrap()
                .node(NodeRef {
                    session_id: sid(1),
                    node: NodeId(2),
                })
                .unwrap()
            else {
                unreachable!()
            };
            assert!(
                matches!(&resolve_slot(&slots[0]).payload, ResultPayload::Succeeded(SuccessContent::Json { value: actual, .. }) if *actual == value)
            );
        }
    }
    drop(lock);
    drop(db);
    SessionStore::open(&layout).unwrap();
}

#[test]
fn tool_reply_mapping_and_privacy() {
    let (_dir, _layout, db) = store();
    let (lock, calls) = running(&db, sid(1), tid(1), 2, 1);
    let binding = &calls.bindings[0];
    for (len, large) in [(65534, false), (65535, true)] {
        let value = json!("x".repeat(len));
        let term = prepare_terminal(binding, ToolReply::Succeeded(value));
        match term.result.payload {
            ResultPayload::Succeeded(SuccessContent::Json { byte_length, .. }) => {
                assert_eq!((byte_length, large), (65536, false))
            }
            ResultPayload::Succeeded(SuccessContent::OutputTooLarge { byte_length, .. }) => {
                assert_eq!((byte_length, large), (65537, true))
            }
            _ => panic!("预期为成功结果"),
        }
    }
    let a = prepare_terminal(binding, ToolReply::Succeeded(json!({"z":"树","a":1})));
    let b = prepare_terminal(binding, ToolReply::Succeeded(json!({"a":1,"z":"树"})));
    assert_eq!(a, b);
    for (reply, expected) in [
        (
            ToolReply::Failed(ToolError {
                code: "execution_failed".into(),
                message: "private-error".into(),
            }),
            ResultPayload::Failed(ToolError {
                code: "execution_failed".into(),
                message: "private-error".into(),
            }),
        ),
        (
            ToolReply::Cancelled,
            ResultPayload::Indeterminate(IndeterminateCause::Cancelled),
        ),
        (
            ToolReply::Indeterminate,
            ResultPayload::Indeterminate(IndeterminateCause::Reported),
        ),
    ] {
        let actual = prepare_terminal(binding, reply).result.payload;
        assert_eq!(actual, expected);
    }
    assert_eq!(
        bound_provider_diagnostic(&"树".repeat(2049))
            .chars()
            .count(),
        2048
    );
    finish(&db, &lock, &calls.bindings[0], ok_reply());
    finish(
        &db,
        &lock,
        &calls.bindings[1],
        failed_reply("execution_failed"),
    );
    let read = db.begin_read().unwrap();
    let turn = read.turn_detail(sid(1), tid(1)).unwrap();
    assert_eq!(
        turn.steps[0].slots[0].progress,
        SlotState::Terminal {
            outcome: ToolResultOutcome::Succeeded,
            reason: None,
            material: MaterialSummary::Available {
                digest: lattice_conversation::tool_content_digest(r#"{"secret-result":42}"#),
                byte_length: 20,
            },
        }
    );
    assert_eq!(turn.steps[0].slots[0].reason, None);
    assert_eq!(
        turn.steps[0].slots[1].progress,
        SlotState::Terminal {
            outcome: ToolResultOutcome::Failed,
            reason: Some(SlotReason::Failed {
                code: "execution_failed".into(),
            }),
            material: MaterialSummary::NoContent,
        }
    );
    assert_eq!(
        turn.steps[0].slots[1].reason,
        Some(SlotReason::Failed {
            code: "execution_failed".into(),
        })
    );
    let detail = serde_json::to_string(&turn).unwrap();
    for secret in [
        "private-input",
        "secret-result",
        "private-error",
        "input_schema",
        "profile_snapshot",
    ] {
        assert!(!detail.contains(secret));
    }
    assert!(!format!("{:?}", terminal(binding, ok_reply())).contains("secret-result"));
    assert!(
        !format!("{:?}", terminal(binding, failed_reply("execution_failed")))
            .contains("private-error")
    );
}

#[test]
fn input_and_diagnostic_boundaries() {
    let mut c = config();
    let base = codec::canonical(&c.catalog.entries).unwrap().len();
    c.catalog.entries[0]
        .description
        .push_str(&"x".repeat(MAX_CATALOG_SNAPSHOT_BYTES - base));
    c.catalog = ToolCatalogSnapshot::new(c.catalog.entries).unwrap();
    assert!(validate::config(&c).is_ok());
    let mut entries = c.catalog.entries.clone();
    entries[0].description.push('x');
    assert!(ToolCatalogSnapshot::new(entries).is_err());
    let (_d, _l, db) = store();
    let (lock, calls) = running(&db, sid(1), tid(1), 2, 1);
    let mut receipt = terminal(&calls.bindings[0], ok_reply());
    db.start_invocation(&lock, &calls.bindings[0], now())
        .unwrap();
    receipt.result.name = "other".into();
    assert!(
        db.finish_invocation(
            &lock,
            TerminalInput {
                invocation_id: iid(1),
                terminal: receipt
            },
            now()
        )
        .is_err()
    );
    let mut receipt = terminal(&calls.bindings[0], ok_reply());
    receipt.result.invocation_id = iid(2);
    assert!(
        db.finish_invocation(
            &lock,
            TerminalInput {
                invocation_id: iid(1),
                terminal: receipt
            },
            now()
        )
        .is_err()
    );
    db.finish_invocation(
        &lock,
        TerminalInput {
            invocation_id: iid(1),
            terminal: terminal(&calls.bindings[0], ok_reply()),
        },
        now(),
    )
    .unwrap();
    db.close_step(
        &lock,
        CloseStep::StopExecuting {
            key: step(tid(1), 0),
            stop: DispatchStop::Deadline,
        },
        now(),
    )
    .unwrap();
    let mut bad = stopped(
        key(tid(1), 1, 0),
        StopCause::ProviderError { retryable: false },
    );
    bad.diagnostic.provider_error = None;
    assert!(validate::diagnostic(&bad.cause, &bad.diagnostic).is_err());
    bad.diagnostic.provider_error = Some("界".repeat(2049));
    assert!(validate::diagnostic(&bad.cause, &bad.diagnostic).is_err());
    bad.diagnostic.provider_error = Some("界".repeat(2048));
    assert!(validate::diagnostic(&bad.cause, &bad.diagnostic).is_ok());
}

#[test]
fn rejected_error_round_trip_and_privacy() {
    let (_dir, layout, db) = store();
    let (lock, calls) = running(&db, sid(1), tid(1), 1, 1);
    let binding = &calls.bindings[0];
    let term = terminal(binding, failed_reply("capability_denied"));
    let wire = serde_json::to_value(&term).unwrap();
    assert_eq!(
        wire["result"]["payload"]["payload"],
        json!({"code": "capability_denied", "message": "private-error"})
    );
    let bytes = codec::encode(&term).unwrap();
    assert_eq!(
        codec::decode::<InvocationTerminal>("steps", "test", &bytes).unwrap(),
        term
    );
    let mut bad = wire.clone();
    bad["result"]["payload"]["payload"]["unknown"] = json!(true);
    let mut bytes = VERSION.to_be_bytes().to_vec();
    bytes.extend(lattice_conversation::canonical_json(&bad).as_bytes());
    assert!(codec::decode::<InvocationTerminal>("steps", "test", &bytes).is_err());
    let mut old = wire;
    old["result"]["payload"]["payload"]
        .as_object_mut()
        .unwrap()
        .remove("message");
    assert!(serde_json::from_value::<InvocationTerminal>(old).is_err());
    db.start_invocation(&lock, binding, now()).unwrap();
    db.finish_invocation(
        &lock,
        TerminalInput {
            invocation_id: binding.invocation_id,
            terminal: term.clone(),
        },
        now(),
    )
    .unwrap();

    drop(lock);
    drop(db);
    let db = SessionStore::open(&layout).unwrap();
    let Node::Step { slots, .. } = node_at(&db, 2) else {
        unreachable!()
    };
    assert_eq!(resolve_slot(&slots[0]), term.result);
    let turn = db
        .begin_read()
        .unwrap()
        .turn_detail(sid(1), tid(1))
        .unwrap();
    assert_eq!(
        turn.steps[0].slots[0].reason,
        Some(SlotReason::Failed {
            code: "capability_denied".into(),
        })
    );
    assert!(
        !serde_json::to_string(&turn)
            .unwrap()
            .contains("private-error")
    );
}
