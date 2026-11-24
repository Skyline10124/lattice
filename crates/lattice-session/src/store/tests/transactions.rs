// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn commit_failure_disables_all_writers() {
    for point in [
        FaultPoint::Poisoned,
        FaultPoint::UnknownBefore,
        FaultPoint::UnknownAfter,
    ] {
        let (_dir, layout, db) = store();
        let db = Arc::new(db);
        let lock = db.try_lock_session(sid(1)).unwrap();
        let starts = std::sync::atomic::AtomicUsize::new(0);
        assert_eq!(
            db.with_execution_start(|| starts.fetch_add(1, std::sync::atomic::Ordering::SeqCst))
                .unwrap(),
            0
        );
        let timeout = std::time::Duration::from_secs(5);
        std::thread::scope(|scope| {
            let (at_commit, commit_reached) = std::sync::mpsc::channel();
            let (release_commit, commit_gate) = std::sync::mpsc::channel();
            let (at_return, returned) = std::sync::mpsc::channel();
            let (release_health, health_gate) = std::sync::mpsc::channel();
            let first = scope.spawn(|| {
                with_write_probe(
                    move |stage| match stage {
                        WriteStage::BeforeCommit => {
                            at_commit.send(()).unwrap();
                            commit_gate.recv_timeout(timeout).unwrap();
                        }
                        WriteStage::CommitReturned => {
                            at_return.send(()).unwrap();
                            health_gate.recv_timeout(timeout).unwrap();
                        }
                        _ => {}
                    },
                    || {
                        with_fault(point, || {
                            db.create_session(
                                &lock,
                                project(1),
                                SessionSettings::for_test(&config()),
                                now(),
                            )
                        })
                    },
                )
            });
            commit_reached
                .recv_timeout(timeout)
                .expect("first writer reached commit");
            let (queued, waiting) = std::sync::mpsc::channel();
            let (completed, done) = std::sync::mpsc::channel();
            let handles: Vec<_> = (2..6)
                .map(|n| {
                    let queued = queued.clone();
                    let completed = completed.clone();
                    let db = &db;
                    let starts = &starts;
                    scope.spawn(move || {
                        let lock = db.try_lock_session(sid(n)).unwrap();
                        let result = with_write_probe(
                            move |stage| {
                                if stage == WriteStage::HealthLockContended {
                                    queued.send(n).unwrap();
                                }
                            },
                            || {
                                if n == 5 {
                                    db.with_execution_start(|| {
                                        starts.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                                    })
                                    .map(|_| ())
                                } else {
                                    db.create_session(
                                        &lock,
                                        project(1),
                                        SessionSettings::for_test(&config()),
                                        now(),
                                    )
                                    .map(|_| ())
                                }
                            },
                        );
                        completed
                            .send(matches!(result, Err(StoreError::WriteDisabled)))
                            .unwrap();
                    })
                })
                .collect();
            let mut queued_ids = std::collections::BTreeSet::new();
            for _ in 2..6 {
                queued_ids.insert(
                    waiting
                        .recv_timeout(timeout)
                        .expect("writer contended on held health lock"),
                );
            }
            assert_eq!(queued_ids, (2..6).collect());
            release_commit.send(()).unwrap();
            returned
                .recv_timeout(timeout)
                .expect("redb write transaction released");
            assert!(
                matches!(
                    done.recv_timeout(std::time::Duration::from_millis(50)),
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout)
                ),
                "queued writer escaped before disabled publication"
            );
            release_health.send(()).unwrap();
            let err = first.join().unwrap().unwrap_err();
            assert!(
                matches!(
                    err,
                    StoreError::CommitFailed {
                        kind: CommitFailureKind::TransactionPoisoned,
                        ..
                    }
                ) == (point == FaultPoint::Poisoned)
            );
            for _ in 2..6 {
                assert!(done.recv_timeout(timeout).expect("queued writer completed"));
            }
            for handle in handles {
                handle.join().unwrap();
            }
        });
        assert!(matches!(
            db.with_execution_start(|| panic!("disabled start")),
            Err(StoreError::WriteDisabled)
        ));
        assert_eq!(starts.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(
            db.begin_read()
                .unwrap()
                .find_session(sid(1))
                .unwrap()
                .is_some(),
            point == FaultPoint::UnknownAfter
        );
        drop(lock);
        drop(db);
        let reopened = SessionStore::open(&layout).unwrap();
        assert_eq!(
            reopened
                .begin_read()
                .unwrap()
                .list(None, 10)
                .unwrap()
                .items
                .len(),
            usize::from(point == FaultPoint::UnknownAfter)
        );
    }
}

#[test]
fn transaction_failure_matrix() {
    for phase in 0..14 {
        for point in [
            FaultPoint::Poisoned,
            FaultPoint::UnknownBefore,
            FaultPoint::UnknownAfter,
        ] {
            let (_dir, layout, db) = store();
            let lock = db.try_lock_session(sid(1)).unwrap();
            if phase == 13 {
                let other_lock = db.try_lock_session(sid(2)).unwrap();
                accept_prepared(&db, &other_lock, accept(sid(2), tid(20))).unwrap();
                drop(other_lock);
                db.create_session(
                    &lock,
                    project(1),
                    SessionSettings::for_test(&config()),
                    now(),
                )
                .unwrap();
            } else if phase != 11 {
                accept_prepared(&db, &lock, accept(sid(1), tid(1))).unwrap();
            }
            if phase > 0 && !matches!(phase, 10..=13) {
                db.start_request(&lock, key(tid(1), 0, 0), origin(), now())
                    .unwrap();
            }
            let calls = if phase >= 4 && !matches!(phase, 8 | 10..=13) {
                Some(
                    db.register_calls(&lock, reply(key(tid(1), 0, 0), 2), &[iid(1), iid(2)], now())
                        .unwrap(),
                )
            } else {
                None
            };
            if phase >= 5 && !matches!(phase, 8 | 10..=13) {
                db.start_invocation(&lock, &calls.as_ref().unwrap().bindings[0], now())
                    .unwrap();
            }
            if phase >= 6 && !matches!(phase, 8 | 10..=13) {
                db.finish_invocation(
                    &lock,
                    TerminalInput {
                        invocation_id: iid(1),
                        terminal: terminal(&calls.as_ref().unwrap().bindings[0], ok_reply()),
                    },
                    now(),
                )
                .unwrap();
            }
            if phase == 9 {
                finish(&db, &lock, &calls.as_ref().unwrap().bindings[1], ok_reply());
            }
            let read = db.begin_read().unwrap();
            let before = codec::load_read(&read.tx).unwrap();
            let result = with_fault(point, || match phase {
                0 => db.start_request(&lock, key(tid(1), 0, 0), origin(), now()),
                1 => db
                    .stop_attempt(
                        &lock,
                        stopped(
                            key(tid(1), 0, 0),
                            StopCause::ProviderError { retryable: true },
                        ),
                        now(),
                    )
                    .map(|_| ()),
                2 => db
                    .stop_attempt(
                        &lock,
                        stopped(key(tid(1), 0, 0), StopCause::Cancelled),
                        now(),
                    )
                    .map(|_| ()),
                3 => db
                    .close_step(&lock, CloseStep::Finish(reply(key(tid(1), 0, 0), 0)), now())
                    .map(|_| ()),
                4 => db
                    .start_invocation(&lock, &calls.as_ref().unwrap().bindings[0], now())
                    .map(|_| ()),
                5 => db.finish_invocation(
                    &lock,
                    TerminalInput {
                        invocation_id: iid(1),
                        terminal: terminal(&calls.as_ref().unwrap().bindings[0], ok_reply()),
                    },
                    now(),
                ),
                6 => db
                    .close_step(
                        &lock,
                        CloseStep::StopExecuting {
                            key: step(tid(1), 0),
                            stop: DispatchStop::Deadline,
                        },
                        now(),
                    )
                    .map(|_| ()),
                7 => db.rewind(&lock, NodeId(0), &project(1), now()).map(|_| ()),
                8 => db
                    .register_calls(&lock, reply(key(tid(1), 0, 0), 2), &[iid(1), iid(2)], now())
                    .map(|_| ()),
                9 => db
                    .close_step(
                        &lock,
                        CloseStep::Continue {
                            key: step(tid(1), 0),
                        },
                        now(),
                    )
                    .map(|_| ()),
                10 => accept_prepared(&db, &lock, accept(sid(1), tid(2))).map(|_| ()),
                11 => db
                    .create_session(
                        &lock,
                        project(1),
                        SessionSettings::for_test(&config()),
                        now(),
                    )
                    .map(|_| ()),
                12 => {
                    let mut updated = SessionSettings::for_test(&config());
                    updated.profile.agent_name = agent("beta");
                    updated.system_prompt = Some("更新后的系统提示".into());
                    db.update_settings(
                        &lock,
                        &project(1),
                        updated,
                        now() + chrono::Duration::seconds(10),
                    )
                    .map(|_| ())
                }
                13 => db
                    .delete_draft(&lock, now() + chrono::Duration::seconds(1))
                    .map(|deleted| assert!(deleted, "删除过期草稿应返回 true")),
                _ => unreachable!(),
            });
            let expected_kind = if point == FaultPoint::Poisoned {
                CommitFailureKind::TransactionPoisoned
            } else {
                CommitFailureKind::OutcomeUnknown
            };
            assert!(
                matches!(
                    result,
                    Err(StoreError::CommitFailed { kind, .. }) if kind == expected_kind
                ),
                "阶段 {phase} 故障点 {point:?} 应返回预期提交失败：{result:?}"
            );
            assert!(
                before == codec::load_read(&read.tx).unwrap(),
                "已打开的只读事务快照不应受失败或提交影响"
            );
            let after = codec::load_read(&db.begin_read().unwrap().tx).unwrap();
            assert_eq!(
                before == after,
                point != FaultPoint::UnknownAfter,
                "阶段 {phase} 故障点 {point:?} 的提交前后状态不符合预期"
            );
            if phase == 13 {
                let retained = point != FaultPoint::UnknownAfter;
                assert_eq!(
                    after.sessions.contains_key(&sid(1)),
                    retained,
                    "故障点 {point:?} 下草稿 sessions 记录存在性与预期不符"
                );
                assert_eq!(
                    after.nodes.contains_key(&(sid(1), NodeId(0))),
                    retained,
                    "故障点 {point:?} 下草稿根节点存在性与预期不符"
                );
                assert_eq!(
                    after.session_settings.contains_key(&sid(1)),
                    retained,
                    "故障点 {point:?} 下草稿 session_settings 记录存在性与预期不符"
                );
                assert_eq!(
                    after.sessions.get(&sid(2)),
                    before.sessions.get(&sid(2)),
                    "其他会话的 sessions 记录不应受草稿删除事务影响"
                );
                assert_eq!(
                    after.session_settings.get(&sid(2)),
                    before.session_settings.get(&sid(2)),
                    "其他会话的 session_settings 记录不应受草稿删除事务影响"
                );
                assert_eq!(
                    after.turn_index, before.turn_index,
                    "其他会话的 turn_index 不应受草稿删除事务影响"
                );
                assert_eq!(
                    after.configs, before.configs,
                    "配置表 configs 不应受草稿删除事务影响"
                );
            }
            validate::records(&after).unwrap();
            drop(read);
            drop(lock);
            drop(db);
            let reopened = SessionStore::open(&layout).unwrap();
            assert!(
                after == codec::load_read(&reopened.begin_read().unwrap().tx).unwrap(),
                "重开会话库后的数据应与故障后快照完全一致"
            );
        }
    }
}

#[test]
fn sequence_exhaustion_aborts_transactions() {
    for case in ["accept_second_node", "continue_node", "stop_node"] {
        let (_dir, _layout, db) = store();
        let lock = db.try_lock_session(sid(1)).unwrap();
        accept_prepared(&db, &lock, accept(sid(1), tid(1))).unwrap();
        let calls = if matches!(case, "continue_node" | "stop_node") {
            db.start_request(&lock, key(tid(1), 0, 0), origin(), now())
                .unwrap();
            let calls = db
                .register_calls(&lock, reply(key(tid(1), 0, 0), 1), &[iid(1)], now())
                .unwrap();
            finish(&db, &lock, &calls.bindings[0], ok_reply());
            Some(calls)
        } else {
            None
        };
        // 压缩可分配的序号空间，保持根、Turn 内相邻节点和索引关系；分配器仍走真实领域事务。
        let tip = NodeId(if case == "accept_second_node" {
            u64::MAX - 1
        } else {
            u64::MAX
        });
        let input_id = NodeId(tip.0 - 1);
        let tx = db.db.begin_write().unwrap();
        let mut r = load_write(&tx).unwrap();
        let input_node = r.nodes.remove(&(sid(1), NodeId(1))).unwrap();
        let mut step_node = r.nodes.remove(&(sid(1), NodeId(2))).unwrap();
        let Node::Step {
            parent, turn_start, ..
        } = &mut step_node
        else {
            unreachable!()
        };
        *parent = input_id;
        turn_start.as_mut().unwrap().input_nodes = vec![input_id];
        r.nodes.insert((sid(1), input_id), input_node);
        r.nodes.insert((sid(1), tip), step_node);
        r.turn_index.get_mut(&tid(1)).unwrap().node = tip;
        r.sessions.get_mut(&sid(1)).unwrap().current = tip;
        replace_all_for_test(&tx, &r).unwrap();
        tx.commit().unwrap();

        let mut input = accept(sid(1), tid(3));
        input.config.system_prompt = Some("new config must not survive exhaustion".into());
        let new_digest = input.config.digest().unwrap();
        let before = records(&db);
        let reached_commit = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let probe = reached_commit.clone();
        let result = with_write_probe(
            move |stage| {
                if stage == WriteStage::BeforeCommit {
                    probe.store(true, std::sync::atomic::Ordering::SeqCst);
                }
            },
            || match case {
                "accept_second_node" => accept_prepared(&db, &lock, input).map(|_| ()),
                _ => db
                    .close_step(
                        &lock,
                        if case == "stop_node" {
                            CloseStep::StopExecuting {
                                key: calls.as_ref().unwrap().key,
                                stop: DispatchStop::Cancelled,
                            }
                        } else {
                            CloseStep::Continue {
                                key: calls.as_ref().unwrap().key,
                            }
                        },
                        now(),
                    )
                    .map(|_| ()),
            },
        );
        assert!(
            matches!(result, Err(StoreError::SequenceExhausted { kind: "node" })),
            "{case}: {result:?}"
        );
        assert!(
            !reached_commit.load(std::sync::atomic::Ordering::SeqCst),
            "{case}: reached commit"
        );
        let after = records(&db);
        assert!(before == after, "{case}: partial transaction survived");
        assert!(!after.turn_index.contains_key(&tid(3)));
        assert!(!after.configs.contains_key(&new_digest));
    }
}

#[test]
fn disable_writes_waits_for_inflight_commit_and_blocks_later_writes() {
    let (_dir, _layout, db) = store();
    let db = Arc::new(db);
    let lock1 = db.try_lock_session(sid(1)).expect("锁定会话 1");
    let lock2 = db.try_lock_session(sid(2)).expect("锁定会话 2");
    let timeout = std::time::Duration::from_secs(5);

    std::thread::scope(|scope| {
        let (at_commit, commit_reached) = std::sync::mpsc::channel();
        let (release_commit, commit_gate) = std::sync::mpsc::channel();
        let (at_return, returned) = std::sync::mpsc::channel();
        let (release_health, health_gate) = std::sync::mpsc::channel();
        let writer = {
            let db = &db;
            let lock1 = &lock1;
            scope.spawn(move || {
                with_write_probe(
                    move |stage| match stage {
                        WriteStage::BeforeCommit => {
                            at_commit.send(()).unwrap();
                            commit_gate.recv_timeout(timeout).unwrap();
                        }
                        WriteStage::CommitReturned => {
                            at_return.send(()).unwrap();
                            health_gate.recv_timeout(timeout).unwrap();
                        }
                        _ => {}
                    },
                    || {
                        db.create_session(
                            lock1,
                            project(1),
                            SessionSettings::for_test(&config()),
                            now(),
                        )
                    },
                )
            })
        };

        commit_reached
            .recv_timeout(timeout)
            .expect("在途写入应到达提交前探针");

        let (contended_tx, contended_rx) = std::sync::mpsc::channel();
        let (disabled_tx, disabled_rx) = std::sync::mpsc::channel();
        let disabler = {
            let db = &db;
            scope.spawn(move || {
                with_write_probe(
                    move |stage| {
                        if stage == WriteStage::HealthLockContended {
                            contended_tx.send(()).unwrap();
                        }
                    },
                    || db.disable_writes(),
                );
                disabled_tx.send(()).unwrap();
            })
        };

        contended_rx
            .recv_timeout(timeout)
            .expect("disable_writes 应在健康锁上等待正在进行的写入");
        assert!(
            matches!(
                disabled_rx.recv_timeout(std::time::Duration::from_millis(50)),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout)
            ),
            "在途写入提交前 disable_writes 不应提前返回"
        );

        release_commit.send(()).unwrap();
        returned
            .recv_timeout(timeout)
            .expect("redb 提交返回后仍持有健康锁");
        assert!(
            matches!(
                disabled_rx.recv_timeout(std::time::Duration::from_millis(50)),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout)
            ),
            "健康锁释放前 disable_writes 不应提前返回"
        );

        release_health.send(()).unwrap();
        writer.join().unwrap().expect("已进入提交的写入应正常完成");
        disabled_rx
            .recv_timeout(timeout)
            .expect("在途提交结束后 disable_writes 应完成");
        disabler.join().unwrap();
    });

    assert!(
        matches!(
            db.create_session(
                &lock2,
                project(1),
                SessionSettings::for_test(&config()),
                now(),
            ),
            Err(StoreError::WriteDisabled)
        ),
        "停写后新写入应返回 WriteDisabled"
    );
    assert!(
        matches!(
            db.with_execution_start(|| ()),
            Err(StoreError::WriteDisabled)
        ),
        "停写后执行起点交接应返回 WriteDisabled"
    );

    let head = db
        .begin_read()
        .expect("停写后只读事务应照常开启")
        .session(sid(1))
        .expect("停写后应能读取已提交的会话");
    assert_eq!(head.session_id, sid(1), "读取到的会话标识应一致");

    db.disable_writes();
    assert!(
        matches!(
            db.create_session(
                &lock2,
                project(1),
                SessionSettings::for_test(&config()),
                now(),
            ),
            Err(StoreError::WriteDisabled)
        ),
        "重复调用 disable_writes 应保持幂等并继续拒绝写入"
    );
}
