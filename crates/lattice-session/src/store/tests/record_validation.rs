// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use super::*;

fn corruptible_fixture(db: &SessionStore) -> SessionExecutionLock {
    let (lock, calls) = running(db, sid(1), tid(1), 1, 1);
    finish(db, &lock, &calls.bindings[0], ok_reply());
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
    db.stop_attempt(
        &lock,
        stopped(
            key(tid(1), 1, 0),
            StopCause::ProviderError { retryable: true },
        ),
        now(),
    )
    .unwrap();
    db.start_request(&lock, key(tid(1), 1, 1), origin(), now())
        .unwrap();
    let calls = db
        .register_calls(
            &lock,
            reply(key(tid(1), 1, 1), 3),
            &[iid(2), iid(3), iid(4)],
            now(),
        )
        .unwrap();
    finish(db, &lock, &calls.bindings[0], ok_reply());
    db.start_invocation(&lock, &calls.bindings[1], now())
        .unwrap();
    lock
}

#[test]
fn read_validation_rejects_broken_relations_and_shapes() {
    let (_d, layout, db) = store();
    let lock = corruptible_fixture(&db);
    let base = records(&db);
    drop(lock);
    drop(db);
    for case in [
        "session_id",
        "current",
        "root",
        "root_missing",
        "root_extra",
        "user_parent",
        "orphan_session",
        "node_gap",
        "parent_forward",
        "parent_missing",
        "step_turn",
        "ordinal",
        "turn_index_missing",
        "turn_index_extra",
        "turn_index_session",
        "turn_index_node",
        "turn_index_root",
        "config_missing",
        "config_digest",
        "catalog_digest",
        "intermediate_unclosed",
        "last_continue",
        "executing_reply",
        "reply_binding",
        "reply_provider",
        "reply_provider_id",
        "reply_origin_missing",
        "attempt_provider",
        "attempt_protocol",
        "reply_attempt_sent",
        "reply_attempt_stopped",
        "reply_slot_count",
        "slot_turn",
        "slot_arguments_digest",
        "slot_tool_identity",
        "terminal_not_executed",
        "terminal_no_result",
        "duplicate_invocation",
        "result_digest",
        "index_missing",
        "index_extra",
        "index_address",
    ] {
        let mut r = base.clone();
        match case {
            "session_id" => r.sessions.get_mut(&sid(1)).unwrap().session_id = sid(2),
            "current" => r.sessions.get_mut(&sid(1)).unwrap().current = NodeId(99),
            "root" => {
                r.nodes
                    .insert((sid(1), NodeId(0)), r.nodes[&(sid(1), NodeId(1))].clone());
            }
            "root_missing" => {
                r.nodes.remove(&(sid(1), NodeId(0)));
            }
            "root_extra" => {
                r.nodes.insert((sid(1), NodeId(4)), Node::Root);
            }
            "user_parent" => {
                let Node::Input { parent, .. } = r.nodes.get_mut(&(sid(1), NodeId(1))).unwrap()
                else {
                    unreachable!()
                };
                *parent = NodeId(1);
            }
            "orphan_session" => {
                r.sessions.clear();
            }
            "node_gap" => {
                let n = r.nodes.remove(&(sid(1), NodeId(3))).unwrap();
                r.nodes.insert((sid(1), NodeId(4)), n);
            }
            "turn_index_missing" => {
                r.turn_index.clear();
            }
            "turn_index_extra" => {
                r.turn_index.insert(tid(2), r.turn_index[&tid(1)]);
            }
            "turn_index_session" => r.turn_index.get_mut(&tid(1)).unwrap().session_id = sid(2),
            "turn_index_node" => r.turn_index.get_mut(&tid(1)).unwrap().node = NodeId(1),
            "turn_index_root" => r.turn_index.get_mut(&tid(1)).unwrap().node = NodeId(0),
            "config_missing" => r.configs.clear(),
            "config_digest" => {
                r.configs.values_mut().next().unwrap().system_prompt = Some("changed".into())
            }
            "catalog_digest" => r.configs.values_mut().next().unwrap().catalog.digest = [0; 32],
            "intermediate_unclosed" => {
                let Node::Step { progress, .. } = r.nodes.get_mut(&(sid(1), NodeId(2))).unwrap()
                else {
                    unreachable!()
                };
                *progress = StepProgress::Executing;
            }
            "index_missing" => {
                r.index.remove(&iid(2));
            }
            "index_extra" => {
                r.index.insert(iid(99), r.index[&iid(2)].clone());
            }
            "index_address" => r.index.get_mut(&iid(2)).unwrap().address.part_ordinal = 99,
            _ => {
                let Node::Step {
                    parent,
                    turn_id,
                    step_ordinal,
                    progress,
                    attempts,
                    reply,
                    slots,
                    ..
                } = r.nodes.get_mut(&(sid(1), NodeId(3))).unwrap()
                else {
                    unreachable!()
                };
                match case {
                    "parent_forward" => *parent = NodeId(3),
                    "parent_missing" => *parent = NodeId(99),
                    "step_turn" => *turn_id = tid(2),
                    "ordinal" => *step_ordinal = 2,
                    "last_continue" => {
                        *progress = StepProgress::Closed {
                            end: StepEnd::Continue,
                        }
                    }
                    "executing_reply" => *reply = None,
                    "reply_binding" => {
                        let MessageSemantic::ToolCall { call } =
                            &mut reply.as_mut().unwrap().parts[1].semantic
                        else {
                            unreachable!()
                        };
                        call.arguments_digest = [0; 32];
                    }
                    "attempt_provider" => {
                        attempts[0].origin.provider_id = provider("another-provider")
                    }
                    "attempt_protocol" => {
                        attempts.last_mut().unwrap().origin.protocol = AttemptProtocol::Messages
                    }
                    "reply_attempt_sent" => {
                        attempts.last_mut().unwrap().progress = AttemptProgress::Sent;
                    }
                    "reply_attempt_stopped" => {
                        attempts.last_mut().unwrap().progress = AttemptProgress::Ended {
                            end: AttemptEnd::Stopped {
                                cause: StopCause::ProcessExit,
                            },
                            diagnostic: AttemptDiagnostic::default(),
                        };
                    }
                    "reply_provider" => {
                        reply.as_mut().unwrap().provider_origin = Some(AttemptOrigin {
                            protocol: Protocol::GenerateContent,
                            ..origin()
                        })
                    }
                    "reply_provider_id" => {
                        reply
                            .as_mut()
                            .unwrap()
                            .provider_origin
                            .as_mut()
                            .unwrap()
                            .provider_id = provider("another-provider");
                    }
                    "reply_origin_missing" => reply.as_mut().unwrap().provider_origin = None,
                    "reply_slot_count" => {
                        slots.pop();
                    }
                    "slot_turn" => slots[0].binding.identity.turn_id = tid(99),
                    "slot_arguments_digest" => slots[0].binding.identity.arguments_digest = [0; 32],
                    "slot_tool_identity" => {
                        slots[0].binding.identity.tool_identity = "other-tool@v2".into()
                    }
                    "terminal_not_executed" | "terminal_no_result" => {
                        let SlotProgress::Terminal { result } = &mut slots[0].progress else {
                            unreachable!()
                        };
                        result.payload = if case == "terminal_not_executed" {
                            ResultPayload::NotExecuted
                        } else {
                            ResultPayload::Indeterminate(IndeterminateCause::NoResult)
                        };
                    }
                    "duplicate_invocation" => slots[2].binding.invocation_id = iid(2),
                    "result_digest" => {
                        let SlotProgress::Terminal { result } = &mut slots[0].progress else {
                            unreachable!()
                        };
                        let ResultPayload::Succeeded(SuccessContent::Json { digest, .. }) =
                            &mut result.payload
                        else {
                            unreachable!()
                        };
                        *digest = [0; 32];
                    }
                    _ => unreachable!("{case}"),
                }
            }
        }
        assert!(validate::records(&r).is_err(), "missed {case}");
        let raw = redb::Database::open(layout.lattice_root().join("lattice.redb")).unwrap();
        let tx = raw.begin_write().unwrap();
        overwrite_records(&tx, &r);
        tx.commit().unwrap();
        drop(raw);
        assert!(
            matches!(
                SessionStore::open(&layout),
                Err(StoreError::Corruption { .. })
            ),
            "strict open missed {case}"
        );
    }
    for case in ["start_provider", "consistent_invalid_provider"] {
        let raw = redb::Database::open(layout.lattice_root().join("lattice.redb")).unwrap();
        let tx = raw.begin_write().unwrap();
        overwrite_records(&tx, &base);
        {
            let mut nodes = tx.open_table(NODES).unwrap();
            let start_key = node_key(sid(1), NodeId(2));
            let mut start_wire: Value = {
                let value = nodes.get(start_key.as_slice()).unwrap().unwrap();
                serde_json::from_slice(&value.value()[4..]).unwrap()
            };
            start_wire["turn_start"]["provider"] = json!("Bad_id");
            if case == "consistent_invalid_provider" {
                for attempt in start_wire["attempts"].as_array_mut().unwrap() {
                    attempt["origin"]["provider_id"] = json!("Bad_id");
                }
                start_wire["reply"]["provider_origin"]["provider_id"] = json!("Bad_id");
            }
            nodes
                .insert(
                    start_key.as_slice(),
                    encode(&start_wire).unwrap().as_slice(),
                )
                .unwrap();
            if case == "consistent_invalid_provider" {
                let step_key = node_key(sid(1), NodeId(3));
                let mut step_wire: Value = {
                    let value = nodes.get(step_key.as_slice()).unwrap().unwrap();
                    serde_json::from_slice(&value.value()[4..]).unwrap()
                };
                for attempt in step_wire["attempts"].as_array_mut().unwrap() {
                    attempt["origin"]["provider_id"] = json!("Bad_id");
                }
                step_wire["reply"]["provider_origin"]["provider_id"] = json!("Bad_id");
                nodes
                    .insert(step_key.as_slice(), encode(&step_wire).unwrap().as_slice())
                    .unwrap();
            }
        }
        tx.commit().unwrap();
        drop(raw);
        assert!(
            matches!(
                SessionStore::open(&layout),
                Err(StoreError::Corruption { .. })
            ),
            "strict open missed {case}"
        );
    }
}

#[test]
fn local_reads_detect_live_corruption_and_open_checks_unrelated_sessions() {
    for case in 0..6 {
        let (_d, layout, db) = store();
        let lock = corruptible_fixture(&db);
        let (_other, _calls) = running(&db, sid(2), tid(2), 1, 99);
        let tx = db.db.begin_write().unwrap();
        let mut r = load_write(&tx).unwrap();
        match case {
            0 => {
                r.turn_index.remove(&tid(1));
            }
            1 => {
                r.configs.clear();
            }
            2 => r.sessions.get_mut(&sid(1)).unwrap().current = NodeId(99),
            3 => {
                r.index.remove(&iid(2));
            }
            4 => {
                let Node::Step { parent, .. } = r.nodes.get_mut(&(sid(1), NodeId(3))).unwrap()
                else {
                    unreachable!()
                };
                *parent = NodeId(99);
            }
            _ => {
                let Node::Step { step_ordinal, .. } =
                    r.nodes.get_mut(&(sid(2), NodeId(2))).unwrap()
                else {
                    unreachable!()
                };
                *step_ordinal = 9;
            }
        }
        replace_all_for_test(&tx, &r).unwrap();
        tx.commit().unwrap();
        let read = db.begin_read().unwrap();
        if case == 5 {
            read.turn_detail(sid(1), tid(1)).unwrap();
        } else {
            assert!(
                read.turn_detail(sid(1), tid(1)).is_err(),
                "detail case {case}"
            );
        }
        drop(read);
        drop(_other);
        drop(lock);
        drop(db);
        assert!(SessionStore::open(&layout).is_err(), "open case {case}");
    }
}

#[test]
fn ordinary_operations_load_only_referenced_turns() {
    let (_d, layout, db) = store();
    let lock = db.try_lock_session(sid(1)).unwrap();
    accept_prepared(&db, &lock, accept(sid(1), tid(1))).unwrap();
    db.rewind(&lock, NodeId(0), &project(1), now()).unwrap();
    let next = accept(sid(1), tid(2));
    accept_prepared(&db, &lock, next).unwrap();
    let other = db.try_lock_session(sid(2)).unwrap();
    accept_prepared(&db, &other, accept(sid(2), tid(3))).unwrap();
    let tx = db.db.begin_write().unwrap();
    {
        let mut t = tx.open_table(NODES).unwrap();
        for s in [sid(1), sid(2)] {
            let mut bytes = VERSION.to_be_bytes().to_vec();
            bytes.extend(b"private unrelated history");
            t.insert(node_key(s, NodeId(2)).as_slice(), bytes.as_slice())
                .unwrap();
        }
    }
    tx.commit().unwrap();
    let read = db.begin_read().unwrap();
    assert_eq!(read.session(sid(1)).unwrap().current, NodeId(4));
    assert_eq!(read.list(None, 1).unwrap().items.len(), 1);
    assert_eq!(
        read.session_ids_with_prefix("", 5).unwrap(),
        vec![sid(1), sid(2)]
    );
    assert_eq!(read.session_ids_with_prefix("", 1).unwrap(), vec![sid(1)]);
    let one = sid(1).to_string();
    assert_eq!(read.session_ids_with_prefix(&one, 5).unwrap(), vec![sid(1)]);
    assert!(read.session_ids_with_prefix("zzzz", 5).unwrap().is_empty());
    assert_eq!(read.turn_detail(sid(1), tid(2)).unwrap().steps.len(), 1);
    assert_eq!(
        entry_nodes(&read.view(sid(1), 1).unwrap().entries),
        vec![3, 4]
    );
    assert!(!read.view(sid(1), 1).unwrap().has_more);
    assert_eq!(
        read.entry(NodeRef {
            session_id: sid(1),
            node: NodeId(4)
        })
        .unwrap()
        .turn_id(),
        Some(tid(2))
    );
    let (older, has_more) = read.history(sid(1), NodeId(3), 1).unwrap();
    assert!(older.is_empty() && !has_more);
    assert!(read.turn_node(tid(1)).is_err());
    db.start_request(&lock, key(tid(2), 0, 0), origin(), now())
        .unwrap();
    db.close_step(&lock, CloseStep::Finish(reply(key(tid(2), 0, 0), 0)), now())
        .unwrap();
    assert_eq!(
        db.begin_read()
            .unwrap()
            .turn_detail(sid(1), tid(2))
            .unwrap()
            .summary
            .outcome,
        PublicTurnOutcome::Completed
    );
    assert!(db.begin_read().unwrap().log(sid(1)).is_err());
    drop(read);
    drop(other);
    drop(lock);
    drop(db);
    assert!(SessionStore::open(&layout).is_err());
}

#[test]
fn strict_open_rejects_corruption() {
    for field in ["store_kind", "store_format_version"] {
        let (_dir, layout, db) = store();
        drop(db);
        let raw = redb::Database::open(layout.lattice_root().join("lattice.redb")).unwrap();
        let tx = raw.begin_write().unwrap();
        tx.open_table(META).unwrap().remove(field).unwrap();
        tx.commit().unwrap();
        drop(raw);
        assert!(
            matches!(
                SessionStore::open(&layout),
                Err(StoreError::Corruption { .. })
            ),
            "missing {field}"
        );
    }
    let (_dir, layout, db) = store();
    let lock = db.try_lock_session(sid(1)).unwrap();
    accept_prepared(&db, &lock, accept(sid(1), tid(1))).unwrap();
    drop(lock);
    drop(db);
    corrupt_json(&layout, SESSIONS, &sid(1).to_string(), |v| {
        v["unknown"] = json!("secret")
    });
    assert!(matches!(
        SessionStore::open(&layout),
        Err(StoreError::Corruption { .. })
    ));
}

#[test]
fn local_reads_reject_broken_references() {
    for case in ["step_identity", "index_address", "step_missing"] {
        let (_dir, _layout, db) = store();
        let (lock, _) = running(&db, sid(1), tid(1), 1, 1);
        let tx = db.db.begin_write().unwrap();
        let mut r = load_write(&tx).unwrap();
        match case {
            "step_identity" => {
                let Node::Step { turn_id, .. } = r.nodes.get_mut(&(sid(1), NodeId(2))).unwrap()
                else {
                    unreachable!()
                };
                *turn_id = tid(99);
            }
            "index_address" => r.index.get_mut(&iid(1)).unwrap().address.part_ordinal = 99,
            _ => {
                r.nodes.remove(&(sid(1), NodeId(2)));
            }
        }
        replace_all_for_test(&tx, &r).unwrap();
        tx.commit().unwrap();
        let before = records(&db);
        assert!(
            matches!(
                db.begin_read().unwrap().session(sid(1)),
                Err(StoreError::Corruption { .. })
            ),
            "read accepted {case}"
        );
        assert!(
            matches!(
                db.rewind(&lock, NodeId(0), &project(1), now()),
                Err(StoreError::Corruption { .. })
            ),
            "rewind accepted {case}"
        );
        assert!(before == records(&db), "rewind changed {case}");
    }
}
