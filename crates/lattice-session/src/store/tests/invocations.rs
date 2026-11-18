// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn invocation_registration_and_transitions_are_atomic() {
    let (_d, _l, db) = store();
    let lock = db.try_lock_session(sid(1)).unwrap();
    accept_prepared(&db, &lock, accept(sid(1), tid(1))).unwrap();
    assert!(
        db.start_request(&lock, key(tid(1), 0, 1), origin(), now())
            .is_err()
    );
    db.start_request(&lock, key(tid(1), 0, 0), origin(), now())
        .unwrap();
    assert!(
        db.start_request(&lock, key(tid(1), 0, 0), origin(), now())
            .is_err()
    );
    for case in 0..11 {
        let before = records(&db);
        let mut input = reply(key(tid(1), 0, 0), 2);
        let mut candidates = vec![iid(1), iid(2)];
        match case {
            0 => candidates.pop().map(|_| ()).unwrap(),
            1 => candidates[1] = iid(1),
            2 => {
                input.origin.protocol = AttemptProtocol::Messages;
                input.message.provider_origin = Some(AttemptOrigin {
                    protocol: Protocol::Messages,
                    ..origin()
                });
            }
            3 => input.message.role = MessageRole::User,
            4 => {
                let MessageSemantic::ToolCall { call } = &mut input.message.parts[1].semantic
                else {
                    unreachable!()
                };
                call.name = "unknown".into();
            }
            5 => input = reply(key(tid(1), 0, 0), 0),
            6 => {
                let MessageSemantic::ToolCall { call } = &mut input.message.parts[1].semantic
                else {
                    unreachable!()
                };
                call.arguments = json!([]);
            }
            7 => {
                let MessageSemantic::ToolCall { call } = &mut input.message.parts[3].semantic
                else {
                    unreachable!()
                };
                call.provider_call_id = Some("call-0".into());
            }
            9 => input.origin.provider_id = provider("another-provider"),
            10 => {
                input.message.provider_origin = Some(AttemptOrigin {
                    protocol: Protocol::Messages,
                    ..origin()
                })
            }
            _ => {
                input = reply(key(tid(1), 0, 0), 33);
                candidates = (1..=33).map(iid).collect();
            }
        }
        assert!(
            db.register_calls(&lock, input, &candidates, now()).is_err(),
            "{case}"
        );
        assert!(before == records(&db));
    }
    let (_other, _) = running(&db, sid(2), tid(2), 1, 1);
    assert!(matches!(
        db.register_calls(&lock, reply(key(tid(1), 0, 0), 2), &[iid(1), iid(2)], now()),
        Err(StoreError::InvocationCollision { .. })
    ));
    let original = reply(key(tid(1), 0, 0), 2);
    let calls = db
        .register_calls(&lock, original.clone(), &[iid(2), iid(3)], now())
        .unwrap();
    assert_eq!(calls.reply.parts.len(), original.message.parts.len());
    assert_eq!(
        calls.reply.parts[1].replay_state,
        original.message.parts[1].replay_state
    );
    assert_eq!(calls.bindings[1].identity.part_ordinal, 3);
    assert!(
        db.start_request(&lock, key(tid(1), 0, 1), origin(), now())
            .is_err()
    );
    assert!(
        db.close_step(&lock, CloseStep::Finish(reply(key(tid(1), 0, 0), 0)), now())
            .is_err()
    );
    let mut fake = calls.bindings[0].clone();
    fake.identity.arguments_digest = [0; 32];
    assert!(db.start_invocation(&lock, &fake, now()).is_err());
    assert!(
        db.start_invocation(&lock, &calls.bindings[1], now())
            .is_err()
    );
    assert!(
        db.finish_invocation(
            &lock,
            TerminalInput {
                invocation_id: iid(2),
                terminal: terminal(&calls.bindings[0], ok_reply())
            },
            now()
        )
        .is_err()
    );
    assert!(
        db.close_step(
            &lock,
            CloseStep::Continue {
                key: step(tid(1), 0)
            },
            now()
        )
        .is_err()
    );
    finish(
        &db,
        &lock,
        &calls.bindings[0],
        failed_reply("execution_failed"),
    );
    assert!(
        db.start_invocation(&lock, &calls.bindings[0], now())
            .is_err()
    );
    assert!(
        db.finish_invocation(
            &lock,
            TerminalInput {
                invocation_id: iid(2),
                terminal: terminal(&calls.bindings[0], ok_reply())
            },
            now()
        )
        .is_err()
    );
    finish(&db, &lock, &calls.bindings[1], failed_reply("guard_denied"));
    assert!(
        db.finish_invocation(
            &lock,
            TerminalInput {
                invocation_id: iid(3),
                terminal: terminal(&calls.bindings[1], ok_reply())
            },
            now()
        )
        .is_err()
    );
    let closed = db
        .close_step(
            &lock,
            CloseStep::StopExecuting {
                key: calls.key,
                stop: DispatchStop::Cancelled,
            },
            now(),
        )
        .unwrap();
    assert_eq!(closed.step.end, StepEnd::Continue);
    assert_eq!(
        closed.next_node,
        Some(NodeRef {
            session_id: sid(1),
            node: NodeId(3)
        })
    );
    assert_eq!(closed.summary, detail(&db).summary);
    assert_eq!(
        closed.summary.progress,
        StepProgress::Closed {
            end: StepEnd::Stop {
                cause: StopCause::Cancelled
            },
        }
    );
    assert_eq!(
        db.begin_read().unwrap().session(sid(1)).unwrap().current,
        NodeId(3)
    );
    assert!(
        db.start_invocation(&lock, &calls.bindings[0], now())
            .is_err()
    );
    assert_eq!(detail(&db).summary.outcome, PublicTurnOutcome::Cancelled);
}

#[test]
fn completed_batch_stop_is_atomic_and_survives_reopen() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    for stop in [
        DispatchStop::Cancelled,
        DispatchStop::Deadline,
        DispatchStop::ProcessExit,
    ] {
        for (ordinal, fault) in [
            (0, None),
            (MAX_MODEL_STEPS - 1, None),
            (0, Some(FaultPoint::Poisoned)),
            (0, Some(FaultPoint::UnknownBefore)),
            (0, Some(FaultPoint::UnknownAfter)),
        ] {
            let (_dir, layout, db) = store();
            let db = Arc::new(db);
            let lock = db.try_lock_session(sid(1)).unwrap();
            accept_prepared(&db, &lock, accept(sid(1), tid(1))).unwrap();
            for index in 0..=ordinal {
                db.start_request(&lock, key(tid(1), index, 0), origin(), now())
                    .unwrap();
                let base = u64::from(index) * 2 + 1;
                let calls = db
                    .register_calls(
                        &lock,
                        reply(key(tid(1), index, 0), 2),
                        &[iid(base), iid(base + 1)],
                        now(),
                    )
                    .unwrap();
                finish(&db, &lock, &calls.bindings[0], ok_reply());
                finish(
                    &db,
                    &lock,
                    &calls.bindings[1],
                    failed_reply("execution_failed"),
                );
                if index < ordinal {
                    db.close_step(&lock, CloseStep::Continue { key: calls.key }, now())
                        .unwrap();
                }
            }
            let before = records(&db);
            let before_commit = before.clone();
            let reading = db.clone();
            let commits = Arc::new(AtomicUsize::new(0));
            let probe = commits.clone();
            let closed_at = now() + chrono::Duration::seconds(1);
            let close = || {
                db.close_step(
                    &lock,
                    CloseStep::StopExecuting {
                        key: step(tid(1), ordinal),
                        stop,
                    },
                    closed_at,
                )
            };
            let result = with_write_probe(
                move |stage| {
                    if stage == WriteStage::BeforeCommit {
                        probe.fetch_add(1, Ordering::SeqCst);
                        assert!(
                            before_commit == records(&reading),
                            "close became visible before commit"
                        );
                    }
                },
                || match fault {
                    Some(point) => with_fault(point, close),
                    None => close(),
                },
            );
            if fault.is_some() {
                assert!(
                    matches!(result, Err(StoreError::CommitFailed { .. })),
                    "{result:?}"
                );
            } else {
                let closed = result.unwrap();
                assert_eq!(closed.step.key.ordinal, ordinal);
                assert_eq!(
                    closed.step.end,
                    if stop == DispatchStop::ProcessExit {
                        StepEnd::Stop {
                            cause: StopCause::ProcessExit,
                        }
                    } else {
                        StepEnd::Continue
                    }
                );
                assert_eq!(
                    closed.next_node,
                    (stop != DispatchStop::ProcessExit).then_some(NodeRef {
                        session_id: sid(1),
                        node: NodeId(u64::from(ordinal) + 3),
                    })
                );
                assert_eq!(closed.summary, detail(&db).summary);
            }
            assert_eq!(commits.load(Ordering::SeqCst), 1);
            let after = records(&db);
            if fault.is_none() || fault == Some(FaultPoint::UnknownAfter) {
                let turn = detail(&db);
                let next = stop != DispatchStop::ProcessExit;
                assert_eq!(turn.steps.len(), ordinal as usize + 1 + usize::from(next));
                let completed = &turn.steps[ordinal as usize];
                assert!(completed.progress.is_closed());
                assert!(
                    completed
                        .slots
                        .iter()
                        .all(|s| matches!(s.progress, SlotState::Terminal { .. }))
                );
                let last = turn.steps.last().unwrap();
                assert_eq!(last.end, StepEnd::Stop { cause: stop.into() });
                assert_eq!(
                    last.progress,
                    StepProgress::Closed {
                        end: last.end.clone()
                    }
                );
                if next {
                    assert_eq!(completed.end, StepEnd::Continue);
                    assert_eq!(last.key.ordinal, ordinal + 1);
                    assert!(last.attempts.is_empty());
                    assert!(last.slots.is_empty());
                }
                assert_eq!(after.sessions[&sid(1)].current, last.node);
                assert_eq!(turn.summary.progress, last.progress);
                assert_eq!(
                    turn.summary.outcome,
                    if stop == DispatchStop::Cancelled {
                        PublicTurnOutcome::Cancelled
                    } else {
                        PublicTurnOutcome::Failed
                    }
                );
                assert!(turn.summary.notice.is_some());
                assert_eq!(turn.ended_at, closed_at);
                assert_eq!(after.nodes.len(), before.nodes.len() + usize::from(next));
            } else {
                assert!(before == after, "failed close left partial facts");
            }
            validate::records(&after).unwrap();
            let durable = detail(&db);
            drop(lock);
            drop(db);
            let reopened = SessionStore::open(&layout).unwrap();
            assert!(after == records(&reopened));
            assert_eq!(detail(&reopened), durable);
        }
    }
}

#[test]
fn non_hex_tool_identity_catalog_digest_and_binding_mismatch() {
    use sha2::{Digest, Sha256};

    let entries = vec![
        ToolCatalogEntry {
            name: "z_write".into(),
            tool_identity: "".into(),
            description: "empty identity".into(),
            input_schema: json!({"type": "object"}),
        },
        ToolCatalogEntry {
            name: "observe__workspace".into(),
            tool_identity: "mcp:filesystem/read_file@v2#非十六进制".into(),
            description: "opaque identity".into(),
            input_schema: json!({"type": "object"}),
        },
    ];
    let snapshot = ToolCatalogSnapshot::new(entries).expect("非十六进制 tool_identity 应当合法");
    assert_eq!(snapshot.entries[0].name, "observe__workspace");
    assert_eq!(snapshot.entries[1].name, "z_write");

    // 确认目录摘要只对 entries 规范 JSON 编码计算，不含 runtime_build_id。
    let canonical_entries =
        lattice_conversation::canonical_json(&serde_json::to_value(&snapshot.entries).unwrap());
    let mut hasher = Sha256::new();
    hasher.update(b"lattice-tool-catalog-snapshot-v1\0");
    hasher.update((canonical_entries.len() as u64).to_be_bytes());
    hasher.update(canonical_entries.as_bytes());
    let expected_digest: [u8; 32] = hasher.finalize().into();
    assert_eq!(
        snapshot.digest, expected_digest,
        "目录摘要应仅由条目列表决定"
    );

    let mut wire_snapshot = serde_json::to_value(&snapshot).unwrap();
    assert!(
        wire_snapshot.get("runtime_build_id").is_none(),
        "序列化目录不应含 runtime_build_id"
    );
    wire_snapshot["runtime_build_id"] = json!("build-1");
    assert!(
        serde_json::from_value::<ToolCatalogSnapshot>(wire_snapshot).is_err(),
        "反序列化目录必须拒绝旧字段 runtime_build_id"
    );

    let (_dir, layout, db) = store();
    let lock = db.try_lock_session(sid(1)).unwrap();
    let mut turn_accept = accept(sid(1), tid(1));
    turn_accept.config.catalog = snapshot.clone();
    let settings = SessionSettings::for_test(&turn_accept.config);
    turn_accept.settings_version = settings.version().unwrap();
    db.create_session(&lock, turn_accept.project_path.clone(), settings, now())
        .expect("创建测试会话");
    db.accept_turn(&lock, turn_accept)
        .expect("接受含非十六进制 tool_identity 的 Turn");

    let k = key(tid(1), 0, 0);
    db.start_request(&lock, k, origin(), now()).unwrap();
    let registered = db
        .register_calls(&lock, reply(k, 1), &[iid(1)], now())
        .expect("登记非十六进制 tool_identity 的工具调用");
    assert_eq!(
        registered.bindings[0].identity.tool_identity,
        "mcp:filesystem/read_file@v2#非十六进制"
    );

    let started = db
        .start_invocation(&lock, &registered.bindings[0], now())
        .unwrap();
    assert_eq!(
        started.binding().identity.tool_identity,
        "mcp:filesystem/read_file@v2#非十六进制"
    );
    let receipt = terminal(&registered.bindings[0], ok_reply());
    db.finish_invocation(
        &lock,
        TerminalInput {
            invocation_id: iid(1),
            terminal: receipt,
        },
        now(),
    )
    .unwrap();

    {
        let read = db.begin_read().unwrap();
        let stored_cfg = read.turn_config(sid(1), tid(1)).unwrap();
        assert_eq!(stored_cfg.catalog, snapshot);
        let Node::Step { slots, .. } = read
            .node(NodeRef {
                session_id: sid(1),
                node: NodeId(2),
            })
            .unwrap()
        else {
            panic!("预期 Step 节点");
        };
        assert_eq!(
            slots[0].binding.identity.tool_identity,
            "mcp:filesystem/read_file@v2#非十六进制"
        );
        assert_eq!(
            read.turn_detail(sid(1), tid(1)).unwrap().steps[0]
                .slots
                .len(),
            1
        );
    }

    drop(lock);
    drop(db);
    let reopened = SessionStore::open(&layout).expect("重开含非十六进制 tool_identity 的会话库");

    // 篡改持久槽位的绑定参数摘要后，只读投影与严格重开均必须报错。
    reopened
        .edit_test_node(
            NodeRef {
                session_id: sid(1),
                node: NodeId(2),
            },
            |wire| {
                wire["slots"][0]["binding"]["identity"]["arguments_digest"] =
                    json!("00".repeat(32));
            },
        )
        .unwrap();
    {
        let read = reopened.begin_read().unwrap();
        assert!(
            matches!(
                read.node(NodeRef {
                    session_id: sid(1),
                    node: NodeId(2),
                }),
                Err(StoreError::Corruption { .. })
            ),
            "读取节点时绑定参数摘要失配必须报错"
        );
        assert!(
            matches!(
                read.turn_detail(sid(1), tid(1)),
                Err(StoreError::Corruption { .. })
            ),
            "读取轮次详情时绑定参数摘要失配必须报错"
        );
    }
    drop(reopened);
    assert!(
        matches!(
            SessionStore::open(&layout),
            Err(StoreError::Corruption { .. })
        ),
        "重开会话库时绑定参数摘要失配必须报错"
    );
}
