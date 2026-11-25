// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use super::*;

fn conversation_tree() -> (
    tempfile::TempDir,
    StateLayout,
    SessionStore,
    SessionExecutionLock,
) {
    let (dir, layout, db) = store();
    let (lock, calls) = running(&db, sid(1), tid(1), 1, 1);
    finish(&db, &lock, &calls.bindings[0], ok_reply());
    db.close_step(
        &lock,
        CloseStep::Continue {
            key: step(tid(1), 0),
        },
        now(),
    )
    .unwrap();
    db.start_request(&lock, key(tid(1), 1, 0), origin(), now())
        .unwrap();
    db.close_step(&lock, CloseStep::Finish(reply(key(tid(1), 1, 0), 0)), now())
        .unwrap();
    let mut beta_settings = SessionSettings::for_test(&config());
    beta_settings.profile.agent_name = agent("beta");
    db.update_settings(&lock, &project(1), beta_settings, now())
        .unwrap();
    for t in [tid(2), tid(3)] {
        let mut next = accept(sid(1), t);
        next.config.agent_name = agent("beta");
        accept_prepared(&db, &lock, next).unwrap();
        if t == tid(2) {
            db.start_request(&lock, key(t, 0, 0), origin(), now())
                .unwrap();
            db.close_step(&lock, CloseStep::Finish(reply(key(t, 0, 0), 0)), now())
                .unwrap();
        }
    }
    (dir, layout, db, lock)
}

#[test]
fn last_accepted_agent_survives_rewind() {
    let (_dir, layout, db, lock) = conversation_tree();
    for target in [NodeId(2), NodeId(1), NodeId(0)] {
        db.rewind(&lock, target, &project(1), now()).unwrap();
        let read = db.begin_read().unwrap();
        assert_eq!(read.current_agent(sid(1)).unwrap(), Some(agent("beta")));
        assert_eq!(
            read.view(sid(1), 10).unwrap().current_agent,
            Some(agent("beta"))
        );
        assert_eq!(read.log(sid(1)).unwrap().current_agent, Some(agent("beta")));
    }
    drop(lock);
    drop(db);
    let reopened = SessionStore::open(&layout).unwrap();
    assert_eq!(
        reopened
            .begin_read()
            .unwrap()
            .current_agent(sid(1))
            .unwrap(),
        Some(agent("beta"))
    );
}

#[test]
fn conversation_view_recent_turns_and_has_more_boundaries() {
    let (_d, _l, db, lock) = conversation_tree();
    let read = db.begin_read().unwrap();
    let view = read.view(sid(1), 2).unwrap();
    assert_eq!(entry_nodes(&view.entries), vec![4, 5, 6, 7]);
    assert!(view.has_more, "2 轮窗口之外仍有第 1 轮，has_more 应为 true");
    assert_eq!(view.session.current, NodeId(7));
    assert_eq!(view.current_agent.as_ref(), Some(&agent("beta")));
    assert!(
        matches!(&view.entries[3], ConversationEntry::Step { step, agent_name: Some(name), summary: Some(summary), .. }
        if step.progress == StepProgress::Reserved && *name == agent("beta") && summary.progress == StepProgress::Reserved)
    );
    let full = read.view(sid(1), u32::MAX).unwrap();
    assert_eq!(entry_nodes(&full.entries), vec![1, 2, 3, 4, 5, 6, 7]);
    assert!(!full.has_more, "已覆盖到根节点，has_more 应为 false");
    let usage = read.usage(sid(1)).unwrap();
    assert_eq!(
        usage
            .turns
            .iter()
            .map(|row| row.start_node)
            .collect::<Vec<_>>(),
        vec![NodeId(2), NodeId(5), NodeId(7)],
        "TurnUsageRow.start_node 应为各轮 Step 0 节点"
    );
    drop(read);
    assert!(matches!(
        db.rewind(&lock, NodeId(99), &project(1), now()),
        Err(StoreError::NotFound { kind: "node", .. })
    ));
    assert_eq!(
        db.begin_read().unwrap().session(sid(1)).unwrap().current,
        NodeId(7)
    );
    db.rewind(&lock, NodeId(2), &project(1), now()).unwrap();
    let branch = accept(sid(1), tid(4));
    accept_prepared(&db, &lock, branch).unwrap();
    let read = db.begin_read().unwrap();
    let view = read.view(sid(1), 10).unwrap();
    assert_eq!(entry_nodes(&view.entries), vec![1, 2, 8, 9]);
    assert!(!view.has_more);
    assert_eq!(
        read.entry(NodeRef {
            session_id: sid(1),
            node: NodeId(9)
        })
        .unwrap()
        .turn_id(),
        Some(tid(4))
    );
    drop(read);
    db.rewind(&lock, NodeId(1), &project(1), now()).unwrap();
    let view = db.begin_read().unwrap().view(sid(1), 1).unwrap();
    assert_eq!(entry_nodes(&view.entries), vec![1]);
    assert!(!view.has_more);
    assert_eq!(view.current_agent, Some(agent("beta")));
    db.rewind(&lock, NodeId(0), &project(1), now()).unwrap();
    let view = db.begin_read().unwrap().view(sid(1), 1).unwrap();
    assert_eq!(view.session.current, NodeId(0));
    assert!(view.entries.is_empty());
    assert!(!view.has_more);
    assert_eq!(view.current_agent, Some(agent("beta")));
}

#[test]
fn conversation_history_pages_whole_turns_on_fixed_parent_chain() {
    let (_d, _l, db, lock) = conversation_tree();
    let read = db.begin_read().unwrap();
    let (recent, recent_more) = read.history(sid(1), NodeId(6), 1).unwrap();
    assert_eq!(entry_nodes(&recent), vec![4, 5]);
    assert!(recent_more, "节点 4 之前仍有第 1 轮");
    let (older, older_more) = read.history(sid(1), NodeId(4), 1).unwrap();
    assert_eq!(entry_nodes(&older), vec![1, 2, 3]);
    assert!(!older_more, "已回溯到根节点，无更多历史");
    let (both, both_more) = read.history(sid(1), NodeId(6), 2).unwrap();
    assert!(!both_more);
    assert_eq!(both, [older.clone(), recent].concat());
    assert_eq!(
        read.history(sid(1), NodeId(6), u32::MAX).unwrap(),
        (both.clone(), false)
    );
    let (empty, empty_more) = read.history(sid(1), NodeId(1), 1).unwrap();
    assert!(empty.is_empty() && !empty_more);
    drop(read);
    db.rewind(&lock, NodeId(2), &project(1), now()).unwrap();
    let branch = accept(sid(1), tid(4));
    accept_prepared(&db, &lock, branch).unwrap();
    let read = db.begin_read().unwrap();
    assert_eq!(read.history(sid(1), NodeId(6), 2).unwrap(), (both, false));
    let (branch_older, branch_more) = read.history(sid(1), NodeId(8), 1).unwrap();
    assert_eq!(entry_nodes(&branch_older), vec![1, 2]);
    assert!(!branch_more);
    assert_eq!(
        read.entry(NodeRef {
            session_id: sid(1),
            node: NodeId(3)
        })
        .unwrap(),
        older[2]
    );
}

#[test]
fn conversation_reads_reject_invalid_boundaries_and_missing_targets() {
    let (_d, _l, db) = store();
    let lock = db.try_lock_session(sid(1)).unwrap();
    accept_prepared(&db, &lock, accept(sid(1), tid(1))).unwrap();
    let other = db.try_lock_session(sid(2)).unwrap();
    accept_prepared(&db, &other, accept(sid(2), tid(2))).unwrap();
    let next = accept(sid(2), tid(3));
    accept_prepared(&db, &other, next).unwrap();
    let read = db.begin_read().unwrap();
    for before in [NodeId(0), NodeId(2), NodeId(3), NodeId(99)] {
        assert!(
            matches!(
                read.history(sid(1), before, 1),
                Err(StoreError::InvalidInput {
                    field: "before",
                    ..
                })
            ),
            "before {before:?}"
        );
    }
    assert!(matches!(
        read.history(sid(1), NodeId(1), 0),
        Err(StoreError::InvalidInput { field: "turns", .. })
    ));
    assert!(matches!(
        read.view(sid(1), 0),
        Err(StoreError::InvalidInput { field: "turns", .. })
    ));
    assert!(matches!(
        read.view(sid(99), 1),
        Err(StoreError::NotFound {
            kind: "session",
            ..
        })
    ));
    assert!(matches!(
        read.history(sid(99), NodeId(1), 1),
        Err(StoreError::NotFound {
            kind: "session",
            ..
        })
    ));
    assert!(matches!(
        read.entry(NodeRef {
            session_id: sid(99),
            node: NodeId(1)
        }),
        Err(StoreError::NotFound {
            kind: "session",
            ..
        })
    ));
    assert!(matches!(
        read.entry(NodeRef {
            session_id: sid(1),
            node: NodeId(99)
        }),
        Err(StoreError::NotFound { kind: "node", .. })
    ));
    assert!(matches!(
        read.entry(NodeRef {
            session_id: sid(1),
            node: NodeId(0)
        }),
        Err(StoreError::InvalidInput { field: "node", .. })
    ));
}

#[test]
fn conversation_step_summary_moves_to_reserved_successor() {
    let (_d, _l, db) = store();
    let (lock, calls) = running(&db, sid(1), tid(1), 1, 1);
    let node = NodeRef {
        session_id: sid(1),
        node: NodeId(2),
    };
    let entry = db.begin_read().unwrap().entry(node).unwrap();
    let expected = detail(&db);
    let step_node = node_at(&db, 2);
    assert_eq!(
        serde_json::to_value(&entry).unwrap(),
        json!({
            "kind": "step",
            "node": 2,
            "step": expected.steps[0],
            "agent_name": "alpha",
            "reply": step_node.step_content().unwrap(),
            "retries": [],
            "summary": expected.summary,
        })
    );
    finish(&db, &lock, &calls.bindings[0], ok_reply());
    let entry = db.begin_read().unwrap().entry(node).unwrap();
    assert!(matches!(
        entry,
        ConversationEntry::Step {
            summary: Some(_),
            ..
        }
    ));
    db.close_step(
        &lock,
        CloseStep::Continue {
            key: step(tid(1), 0),
        },
        now(),
    )
    .unwrap();
    let read = db.begin_read().unwrap();
    let continued = read.entry(node).unwrap();
    assert!(matches!(
        &continued,
        ConversationEntry::Step { summary: None, .. }
    ));
    assert!(
        serde_json::to_value(&continued)
            .unwrap()
            .get("summary")
            .is_none()
    );
    let next = read
        .entry(NodeRef {
            node: NodeId(3),
            ..node
        })
        .unwrap();
    assert!(
        matches!(&next, ConversationEntry::Step { step, summary: Some(summary), .. }
        if step.progress == StepProgress::Reserved && summary.progress == StepProgress::Reserved)
    );
    assert_eq!(
        read.view(sid(1), 1).unwrap().entries,
        vec![
            read.entry(NodeRef {
                node: NodeId(1),
                ..node
            })
            .unwrap(),
            continued,
            next
        ]
    );
    drop(read);
    db.start_request(&lock, key(tid(1), 1, 0), origin(), now())
        .unwrap();
    db.close_step(&lock, CloseStep::Finish(reply(key(tid(1), 1, 0), 0)), now())
        .unwrap();
    let read = db.begin_read().unwrap();
    let last = read
        .entry(NodeRef {
            node: NodeId(3),
            ..node
        })
        .unwrap();
    assert!(matches!(
        last,
        ConversationEntry::Step {
            summary: Some(TurnSummary {
                outcome: PublicTurnOutcome::Completed,
                ..
            }),
            ..
        }
    ));
    let input_entry = read
        .entry(NodeRef {
            node: NodeId(1),
            ..node
        })
        .unwrap();
    let Node::Input {
        accepted_at, input, ..
    } = node_at(&db, 1)
    else {
        panic!("节点 1 应为 Node::Input");
    };
    assert_eq!(
        serde_json::to_value(&input_entry).unwrap(),
        json!({
            "kind": "input",
            "node": 1,
            "accepted_at": accepted_at,
            "input": input,
        })
    );
    let last_json = serde_json::to_value(&last).unwrap();
    assert!(
        last_json.get("agent_name").is_none(),
        "step_ordinal > 0 不应序列化 agent_name 字段"
    );
    assert!(
        last_json.get("content").is_none(),
        "扁平化 ConversationEntry 不应包含物理 content 字段"
    );
    for entry in read.view(sid(1), 1).unwrap().entries {
        assert_eq!(
            serde_json::from_value::<ConversationEntry>(serde_json::to_value(&entry).unwrap())
                .unwrap(),
            entry
        );
    }
}

#[test]
fn conversation_retries_and_transaction_reads_survive_reopen() {
    let (_d, layout, db) = store();
    let lock = db.try_lock_session(sid(1)).unwrap();
    accept_prepared(&db, &lock, accept(sid(1), tid(1))).unwrap();
    db.start_request(&lock, key(tid(1), 0, 0), origin(), now())
        .unwrap();
    db.stop_attempt(
        &lock,
        stopped(
            key(tid(1), 0, 0),
            StopCause::ProviderError { retryable: true },
        ),
        now(),
    )
    .unwrap();
    let old_read = db.begin_read().unwrap();
    let view = old_read.view(sid(1), 1).unwrap();
    let usage = old_read.usage(sid(1)).unwrap();
    assert_eq!(usage.position, NodeId(2));
    assert_eq!(usage.session_total, usage.path_total);
    assert_eq!(usage.path_total.attempts, 1);
    assert_eq!(view.entries.len(), 2);
    assert!(
        matches!(&view.entries[1], ConversationEntry::Step { retries, .. }
        if *retries == node_at(&db, 2).retry_notices().unwrap() && retries.len() == 1 && retries[0].notice.level == NoticeLevel::Warn)
    );
    db.start_request(&lock, key(tid(1), 0, 1), origin(), now())
        .unwrap();
    db.close_step(&lock, CloseStep::Finish(reply(key(tid(1), 0, 1), 0)), now())
        .unwrap();
    let next = accept(sid(1), tid(2));
    accept_prepared(&db, &lock, next).unwrap();
    assert_eq!(old_read.view(sid(1), 1).unwrap(), view);
    assert_eq!(old_read.usage(sid(1)).unwrap(), usage);
    let current = db.begin_read().unwrap().usage(sid(1)).unwrap();
    assert_eq!(current.position, NodeId(4));
    assert_eq!(current.path_total.attempts, 2);
    assert_eq!(current.session_total, current.path_total);
    assert_eq!(current.turns.len(), 2);
    assert_eq!(
        old_read
            .entry(NodeRef {
                session_id: sid(1),
                node: NodeId(2)
            })
            .unwrap(),
        view.entries[1]
    );
    drop(old_read);
    let read = db.begin_read().unwrap();
    let (entries, has_more) = read.history(sid(1), NodeId(3), 1).unwrap();
    assert!(!has_more);
    assert!(
        matches!(&entries[1], ConversationEntry::Step { retries, summary: Some(summary), .. }
        if retries.len() == 1 && summary.outcome == PublicTurnOutcome::Completed)
    );
    let before = records(&db);
    drop(read);
    drop(lock);
    drop(db);
    let db = SessionStore::open(&layout).unwrap();
    let read = db.begin_read().unwrap();
    assert_eq!(
        read.history(sid(1), NodeId(3), 1).unwrap(),
        (entries.clone(), false)
    );
    assert_eq!(
        read.entry(NodeRef {
            session_id: sid(1),
            node: NodeId(2)
        })
        .unwrap(),
        entries[1]
    );
    assert!(before == records(&db));
}

#[test]
fn conversation_step_entry_reads_only_its_turn() {
    for damaged in [NodeId(4), NodeId(5)] {
        let (_d, _l, db, _lock) = conversation_tree();
        let node = NodeRef {
            session_id: sid(1),
            node: NodeId(3),
        };
        let expected = db.begin_read().unwrap().entry(node).unwrap();
        let tx = db.db.begin_write().unwrap();
        {
            let mut nodes = tx.open_table(NODES).unwrap();
            nodes
                .insert(node_key(sid(1), damaged).as_slice(), b"invalid".as_slice())
                .unwrap();
        }
        tx.commit().unwrap();
        let read = db.begin_read().unwrap();
        assert_eq!(read.entry(node).unwrap(), expected);
        assert!(
            read.entry(NodeRef {
                node: NodeId(6),
                ..node
            })
            .is_ok()
        );
    }
}

#[test]
fn conversation_windows_skip_steps_outside_the_requested_turns() {
    let (_d, _l, db) = store();
    let (lock, calls0) = running(&db, sid(1), tid(1), 1, 1);
    finish(&db, &lock, &calls0.bindings[0], ok_reply());
    db.close_step(
        &lock,
        CloseStep::Continue {
            key: step(tid(1), 0),
        },
        now(),
    )
    .unwrap();
    db.start_request(&lock, key(tid(1), 1, 0), origin(), now())
        .unwrap();
    let calls1 = db
        .register_calls(&lock, reply(key(tid(1), 1, 0), 1), &[iid(2)], now())
        .unwrap();
    finish(&db, &lock, &calls1.bindings[0], ok_reply());
    db.close_step(
        &lock,
        CloseStep::Continue {
            key: step(tid(1), 1),
        },
        now(),
    )
    .unwrap();
    db.start_request(&lock, key(tid(1), 2, 0), origin(), now())
        .unwrap();
    db.close_step(&lock, CloseStep::Finish(reply(key(tid(1), 2, 0), 0)), now())
        .unwrap();
    accept_prepared(&db, &lock, accept(sid(1), tid(2))).unwrap();
    db.start_request(&lock, key(tid(2), 0, 0), origin(), now())
        .unwrap();
    db.close_step(&lock, CloseStep::Finish(reply(key(tid(2), 0, 0), 0)), now())
        .unwrap();
    accept_prepared(&db, &lock, accept(sid(1), tid(3))).unwrap();
    // 树：Input(1) -> Step0(2) -> Step1(3) -> Step2(4) -> Input(5) -> Step0(6) -> Input(7) -> Step0(8)
    let read = db.begin_read().unwrap();
    let view = read.view(sid(1), 1).unwrap();
    assert!(view.has_more);
    let history = read.history(sid(1), NodeId(7), 1).unwrap();
    assert!(history.1);
    drop(read);
    // 损坏窗口之外的 Turn 1 中间步 Step 1 (NodeId(3))；窗口读取不再向根回溯，只按 head != 0 判定 has_more。
    let tx = db.db.begin_write().unwrap();
    {
        let mut nodes = tx.open_table(NODES).unwrap();
        nodes
            .insert(
                node_key(sid(1), NodeId(3)).as_slice(),
                b"invalid".as_slice(),
            )
            .unwrap();
    }
    tx.commit().unwrap();
    let read = db.begin_read().unwrap();
    assert_eq!(read.view(sid(1), 1).unwrap(), view);
    assert_eq!(read.history(sid(1), NodeId(7), 1).unwrap(), history);
    assert_eq!(
        read.entry(NodeRef {
            session_id: sid(1),
            node: NodeId(8)
        })
        .unwrap()
        .turn_id(),
        Some(tid(3))
    );
    assert!(read.view(sid(1), 3).is_err());
}

#[test]
fn conversation_windows_do_not_decode_nodes_outside_window() {
    for (damaged, field) in [
        (NodeId(3), "reply"),
        (NodeId(3), "attempts"),
        (NodeId(3), "slots"),
        (NodeId(3), "unexpected"),
        (NodeId(2), "reply"),
        (NodeId(2), "turn_start"),
        (NodeId(1), "input"),
        (NodeId(1), "unexpected"),
    ] {
        let (_d, _l, db, _lock) = conversation_tree();
        let tx = db.db.begin_write().unwrap();
        {
            let mut nodes = tx.open_table(NODES).unwrap();
            let key = node_key(sid(1), damaged);
            let mut wire: serde_json::Value = {
                let value = nodes.get(key.as_slice()).unwrap().unwrap();
                serde_json::from_slice(&value.value()[4..]).unwrap()
            };
            wire[field] = json!(false);
            let mut bytes = VERSION.to_be_bytes().to_vec();
            bytes.extend(serde_json::to_vec(&wire).unwrap());
            nodes.insert(key.as_slice(), bytes.as_slice()).unwrap();
        }
        tx.commit().unwrap();
        let read = db.begin_read().unwrap();
        let view = read.view(sid(1), 1).unwrap();
        assert_eq!(entry_nodes(&view.entries), vec![6, 7]);
        assert!(view.has_more);
        let (history, history_more) = read.history(sid(1), NodeId(6), 1).unwrap();
        assert_eq!(entry_nodes(&history), vec![4, 5]);
        assert!(history_more);
        assert_eq!(
            read.entry(NodeRef {
                session_id: sid(1),
                node: NodeId(7),
            })
            .unwrap()
            .turn_id(),
            Some(tid(3))
        );
        assert!(matches!(
            read.view(sid(1), 3),
            Err(StoreError::Corruption { .. })
        ));
    }
}

#[test]
fn conversation_reads_reject_damaged_headers_within_traversed_range() {
    let cases: &[(NodeId, &[&str])] = &[
        (NodeId(1), &["kind", "parent"]),
        (NodeId(2), &["kind", "parent", "turn_id", "step_ordinal"]),
        (NodeId(3), &["kind", "parent", "turn_id", "step_ordinal"]),
    ];
    for &(damaged, fields) in cases {
        for &field in fields {
            for remove in [false, true] {
                let (_d, _l, db, _lock) = conversation_tree();
                let tx = db.db.begin_write().unwrap();
                {
                    let mut nodes = tx.open_table(NODES).unwrap();
                    let key = node_key(sid(1), damaged);
                    let mut wire: serde_json::Value = {
                        let value = nodes.get(key.as_slice()).unwrap().unwrap();
                        serde_json::from_slice(&value.value()[4..]).unwrap()
                    };
                    if remove {
                        wire.as_object_mut().unwrap().remove(field);
                    } else {
                        wire[field] = json!(false);
                    }
                    let mut bytes = VERSION.to_be_bytes().to_vec();
                    bytes.extend(serde_json::to_vec(&wire).unwrap());
                    nodes.insert(key.as_slice(), bytes.as_slice()).unwrap();
                }
                tx.commit().unwrap();
                let read = db.begin_read().unwrap();
                for result in [
                    read.usage(sid(1)).map(|_| ()),
                    read.view(sid(1), 3).map(|_| ()),
                    read.history(sid(1), NodeId(6), 2).map(|_| ()),
                ] {
                    assert!(
                        matches!(result, Err(StoreError::Corruption { ref table, ref key, .. })
                        if table == "nodes" && key == &format!("{}/{}", sid(1), damaged.0)),
                        "damaged={damaged:?} field={field} remove={remove} result={result:?}"
                    );
                }
            }
        }
    }
}

#[test]
fn conversation_counts_reject_invalid_value_versions() {
    for version in [None, Some(VERSION + 1)] {
        let (_d, layout, db, _lock) = conversation_tree();
        let tx = db.db.begin_write().unwrap();
        {
            let mut nodes = tx.open_table(NODES).unwrap();
            let key = node_key(sid(1), NodeId(3));
            let mut bytes = nodes.get(key.as_slice()).unwrap().unwrap().value().to_vec();
            if let Some(version) = version {
                bytes[..4].copy_from_slice(&version.to_be_bytes());
            } else {
                bytes.truncate(3);
            }
            nodes.insert(key.as_slice(), bytes.as_slice()).unwrap();
        }
        tx.commit().unwrap();
        let read = db.begin_read().unwrap();
        for result in [
            read.view(sid(1), 3).map(|_| ()),
            read.usage(sid(1)).map(|_| ()),
        ] {
            if let Some(version) = version {
                assert!(
                    matches!(result, Err(StoreError::UnsupportedVersion { path, table, key, found, supported: VERSION })
                if path == layout.lattice_root().join("lattice.redb")
                    && table == "nodes" && key == format!("{}/3", sid(1)) && found == version)
                );
            } else {
                assert!(matches!(result, Err(StoreError::Corruption { .. })));
            }
        }
    }
}

#[test]
fn conversation_reads_report_missing_nodes_and_invalid_encoding() {
    for damaged in [NodeId(1), NodeId(2)] {
        for broken in [false, true] {
            let (_d, _l, db) = store();
            let lock = db.try_lock_session(sid(1)).unwrap();
            accept_prepared(&db, &lock, accept(sid(1), tid(1))).unwrap();
            let next = accept(sid(1), tid(2));
            accept_prepared(&db, &lock, next).unwrap();
            let tx = db.db.begin_write().unwrap();
            {
                let mut nodes = tx.open_table(NODES).unwrap();
                if broken {
                    nodes
                        .insert(node_key(sid(1), damaged).as_slice(), b"invalid".as_slice())
                        .unwrap();
                } else {
                    nodes.remove(node_key(sid(1), damaged).as_slice()).unwrap();
                }
            }
            tx.commit().unwrap();
            let read = db.begin_read().unwrap();
            // view(1) 只读节点 3、4，且在探查节点 3 之前停止于节点 2（Step）；
            // 若节点 2 损坏则 view(1) 也会报错，节点 1 损坏则只在读入窗口（view(2) / history / usage）报错。
            for result in [
                read.usage(sid(1)).map(|_| ()),
                read.view(sid(1), 2).map(|_| ()),
                read.history(sid(1), NodeId(3), 1).map(|_| ()),
            ] {
                assert!(matches!(
                    result,
                    Err(StoreError::Corruption { .. }) | Err(StoreError::UnsupportedVersion { .. })
                ));
            }
            if damaged == NodeId(2) {
                assert!(matches!(
                    read.view(sid(1), 1),
                    Err(StoreError::Corruption { .. }) | Err(StoreError::UnsupportedVersion { .. })
                ));
            }
            let entry = read.entry(NodeRef {
                session_id: sid(1),
                node: NodeId(2),
            });
            if !broken && damaged == NodeId(2) {
                assert!(matches!(
                    entry,
                    Err(StoreError::NotFound { kind: "node", .. })
                ));
            } else if damaged == NodeId(2) {
                assert!(matches!(
                    entry,
                    Err(StoreError::Corruption { .. }) | Err(StoreError::UnsupportedVersion { .. })
                ));
            }
        }
    }
}
