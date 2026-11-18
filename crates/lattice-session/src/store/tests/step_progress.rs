// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn progress_conclusions_and_stop_precedence() {
    let (_d, _l, db) = store();
    let (_lock, calls) = running(&db, sid(1), tid(1), 2, 1);
    let registered = InvocationSlot {
        binding: calls.bindings[0].clone(),
        progress: SlotProgress::Registered,
    };
    let mut started = registered.clone();
    started.progress = SlotProgress::Started;
    for (slot, payload) in [
        (&registered, ResultPayload::NotExecuted),
        (
            &started,
            ResultPayload::Indeterminate(IndeterminateCause::NoResult),
        ),
    ] {
        assert_eq!(
            resolve_slot(slot),
            StoredToolResult {
                invocation_id: iid(1),
                name: "observe__workspace".into(),
                payload,
            }
        );
    }
    for reply in [
        ok_reply(),
        failed_reply("execution_failed"),
        ToolReply::Cancelled,
        ToolReply::Indeterminate,
    ] {
        let term = terminal(&calls.bindings[0], reply.clone());
        let slot = InvocationSlot {
            binding: calls.bindings[0].clone(),
            progress: SlotProgress::Terminal {
                result: term.result.clone(),
            },
        };
        assert_eq!(resolve_slot(&slot), term.result);
        for stop in [
            DispatchStop::Cancelled,
            DispatchStop::Deadline,
            DispatchStop::ProcessExit,
        ] {
            let expected = match &reply {
                ToolReply::Cancelled => Some(StopCause::Cancelled),
                ToolReply::Indeterminate => Some(StopCause::InvocationIndeterminate {
                    invocation_id: iid(1),
                }),
                _ if stop == DispatchStop::ProcessExit => Some(StopCause::ProcessExit),
                _ => None,
            };
            assert_eq!(
                step_stop_cause(std::slice::from_ref(&slot), stop).unwrap(),
                expected
            );
            let mut tail = registered.clone();
            tail.binding = calls.bindings[1].clone();
            assert_eq!(
                step_stop_cause(&[slot.clone(), tail], stop).unwrap(),
                expected.or_else(|| Some(stop.into()))
            );
        }
    }
    for progress in [
        StepProgress::Reserved,
        StepProgress::Requesting,
        StepProgress::Executing,
    ] {
        assert_eq!(
            resolve_step(&progress, &[]).unwrap(),
            StepEnd::Stop {
                cause: StopCause::ProcessExit
            }
        );
    }
    for end in [
        StepEnd::Continue,
        StepEnd::Finish,
        StepEnd::Stop {
            cause: StopCause::Deadline,
        },
    ] {
        assert_eq!(
            resolve_step(&StepProgress::Closed { end: end.clone() }, &[]).unwrap(),
            end
        );
    }
    let mut attempt = AttemptRecord {
        usage_kind: lattice_conversation::UsageCompleteness::Partial,
        cost: None,
        origin: origin(),
        usage: None,
        progress: AttemptProgress::Sent,
        retry_delay_ms: None,
    };
    assert_eq!(
        resolve_attempt(&attempt),
        (
            AttemptEnd::Stopped {
                cause: StopCause::ProcessExit
            },
            AttemptDiagnostic::default()
        )
    );
    for end in [
        AttemptEnd::Stopped {
            cause: StopCause::Deadline,
        },
        AttemptEnd::Completed,
    ] {
        let diagnostic = AttemptDiagnostic {
            provider_error: Some("diagnostic".into()),
            ..Default::default()
        };
        attempt.progress = AttemptProgress::Ended {
            end: end.clone(),
            diagnostic: diagnostic.clone(),
        };
        assert_eq!(resolve_attempt(&attempt), (end, diagnostic));
    }
}

#[test]
fn every_commit_reopens_with_read_only_conclusions() {
    for cut in 0..12 {
        let (_d, layout, db) = store();
        let lock = db.try_lock_session(sid(1)).unwrap();
        accept_prepared(&db, &lock, accept(sid(1), tid(1))).unwrap();
        if cut >= 1 {
            db.start_request(&lock, key(tid(1), 0, 0), origin(), now())
                .unwrap();
        }
        if cut == 2 {
            db.stop_attempt(
                &lock,
                stopped(
                    key(tid(1), 0, 0),
                    StopCause::ProviderError { retryable: true },
                ),
                now(),
            )
            .unwrap();
        }
        if cut == 3 {
            db.stop_attempt(
                &lock,
                stopped(
                    key(tid(1), 0, 0),
                    StopCause::ProviderError { retryable: false },
                ),
                now(),
            )
            .unwrap();
        }
        if cut == 4 {
            db.close_step(&lock, CloseStep::Finish(reply(key(tid(1), 0, 0), 0)), now())
                .unwrap();
        }
        if cut >= 5 {
            let calls = db
                .register_calls(&lock, reply(key(tid(1), 0, 0), 2), &[iid(1), iid(2)], now())
                .unwrap();
            if cut >= 6 {
                db.start_invocation(&lock, &calls.bindings[0], now())
                    .unwrap();
            }
            if cut >= 7 {
                db.finish_invocation(
                    &lock,
                    TerminalInput {
                        invocation_id: iid(1),
                        terminal: terminal(
                            &calls.bindings[0],
                            if cut == 8 {
                                ToolReply::Cancelled
                            } else {
                                ok_reply()
                            },
                        ),
                    },
                    now(),
                )
                .unwrap();
            }
            if cut >= 9 {
                finish(&db, &lock, &calls.bindings[1], ok_reply());
            }
            if cut == 10 {
                db.close_step(
                    &lock,
                    CloseStep::Continue {
                        key: step(tid(1), 0),
                    },
                    now(),
                )
                .unwrap();
            }
            if cut == 11 {
                db.close_step(
                    &lock,
                    CloseStep::StopExecuting {
                        key: step(tid(1), 0),
                        stop: DispatchStop::ProcessExit,
                    },
                    now(),
                )
                .unwrap();
            }
        }
        let before = records(&db);
        let expected = detail(&db);
        drop(lock);
        drop(db);
        let db = SessionStore::open(&layout).unwrap();
        let read = db.begin_read().unwrap();
        let actual = read.turn_detail(sid(1), tid(1)).unwrap();
        assert_eq!(actual, expected, "cut {cut}");
        let last = actual.steps.last().unwrap();
        let expected_progress = match cut {
            0 | 10 => StepProgress::Reserved,
            1 | 2 => StepProgress::Requesting,
            3 => StepProgress::Closed {
                end: StepEnd::Stop {
                    cause: StopCause::ProviderError { retryable: false },
                },
            },
            4 => StepProgress::Closed {
                end: StepEnd::Finish,
            },
            11 => StepProgress::Closed {
                end: StepEnd::Stop {
                    cause: StopCause::ProcessExit,
                },
            },
            _ => StepProgress::Executing,
        };
        assert_eq!(last.progress, expected_progress, "cut {cut}");
        assert_eq!(actual.summary.progress, last.progress);
        assert_eq!(
            actual.summary.outcome,
            if cut == 4 {
                PublicTurnOutcome::Completed
            } else if cut == 8 {
                PublicTurnOutcome::Cancelled
            } else {
                PublicTurnOutcome::Failed
            }
        );
        assert_eq!(
            last.end,
            if cut == 4 {
                StepEnd::Finish
            } else if cut == 3 {
                StepEnd::Stop {
                    cause: StopCause::ProviderError { retryable: false },
                }
            } else if cut == 8 {
                StepEnd::Stop {
                    cause: StopCause::Cancelled,
                }
            } else {
                StepEnd::Stop {
                    cause: StopCause::ProcessExit,
                }
            }
        );
        if cut >= 5 {
            let first = &actual.steps[0].slots[0];
            assert_eq!(
                first.outcome,
                if cut == 5 {
                    ToolResultOutcome::NotExecuted
                } else if cut == 6 || cut == 8 {
                    ToolResultOutcome::Indeterminate
                } else {
                    ToolResultOutcome::Succeeded
                }
            );
            if cut == 6 {
                assert_eq!(
                    first.reason,
                    Some(SlotReason::Indeterminate(IndeterminateCause::NoResult))
                );
            }
            assert_eq!(
                actual.steps[0].slots[1].outcome,
                if cut >= 9 {
                    ToolResultOutcome::Succeeded
                } else {
                    ToolResultOutcome::NotExecuted
                }
            );
        }
        if cut == 1 {
            assert!(matches!(
                last.attempts[0].end,
                AttemptEndDetail::Stopped {
                    cause: StopCause::ProcessExit,
                    ..
                }
            ));
        }
        if cut == 2 {
            assert_eq!(node_at(&db, 2).retry_notices().unwrap()[0].delay_ms, 250);
        }
        let view = read.view(sid(1), 1).unwrap();
        assert_eq!(view.entries.len(), actual.steps.len() + 1);
        for (entry, expected_step) in view.entries.iter().skip(1).zip(&actual.steps) {
            let ConversationEntry::Step {
                step,
                summary,
                retries,
                ..
            } = entry
            else {
                panic!("expected step at cut {cut}");
            };
            assert_eq!(step, expected_step, "cut {cut}");
            assert_eq!(
                *retries,
                read.node(NodeRef {
                    session_id: sid(1),
                    node: step.node
                })
                .unwrap()
                .retry_notices()
                .unwrap()
            );
            assert_eq!(
                summary.as_ref(),
                (step.node == last.node).then_some(&actual.summary),
                "cut {cut}"
            );
        }
        read.log(sid(1)).unwrap();
        read.list(None, 10).unwrap();
        assert!(before == records(&db), "read changed cut {cut}");
        drop(read);
        let lock = db.try_lock_session(sid(1)).unwrap();
        db.rewind(&lock, NodeId(0), &project(1), now()).unwrap();
        let next = accept(sid(1), tid(2));
        accept_prepared(&db, &lock, next).unwrap();
        assert_eq!(detail(&db), expected, "old turn changed at cut {cut}");
    }
}

#[test]
fn step_budget_reserves_and_closes_limit_node() {
    let (_d, layout, db) = store();
    let lock = db.try_lock_session(sid(1)).unwrap();
    accept_prepared(&db, &lock, accept(sid(1), tid(1))).unwrap();
    for ordinal in 0..MAX_MODEL_STEPS {
        db.start_request(&lock, key(tid(1), ordinal, 0), origin(), now())
            .unwrap();
        let calls = db
            .register_calls(
                &lock,
                reply(key(tid(1), ordinal, 0), 1),
                &[iid(u64::from(ordinal) + 1)],
                now(),
            )
            .unwrap();
        finish(&db, &lock, &calls.bindings[0], ok_reply());
        let closed = db
            .close_step(
                &lock,
                CloseStep::Continue {
                    key: step(tid(1), ordinal),
                },
                now(),
            )
            .unwrap();
        assert_eq!(
            closed.next_node.unwrap().node,
            NodeId(u64::from(ordinal) + 3)
        );
    }
    let t = detail(&db);
    assert_eq!(t.steps.len(), 33);
    let last = t.steps.last().unwrap();
    assert_eq!(
        last.progress,
        StepProgress::Closed {
            end: StepEnd::Stop {
                cause: StopCause::ModelStepLimit
            }
        }
    );
    assert!(last.attempts.is_empty());
    assert!(
        db.start_request(&lock, key(tid(1), MAX_MODEL_STEPS, 0), origin(), now())
            .is_err()
    );
    assert_eq!(t.usage.attempts, 32);
    assert_eq!(t.usage.input.known, Some(64));
    assert_eq!(t.usage.output.known, Some(96));
    assert!(t.usage.input.complete);
    assert_eq!(t.usage.cost.unknown_attempts, 32);
    drop(lock);
    drop(db);
    assert_eq!(detail(&SessionStore::open(&layout).unwrap()), t);
}

#[test]
fn ordered_terminal_causes() {
    for stop in [
        DispatchStop::Cancelled,
        DispatchStop::Deadline,
        DispatchStop::ProcessExit,
    ] {
        let (_dir, _layout, db) = store();
        let (lock, calls) = running(&db, sid(1), tid(1), 3, 1);
        finish(
            &db,
            &lock,
            &calls.bindings[0],
            failed_reply("execution_failed"),
        );
        db.close_step(
            &lock,
            CloseStep::StopExecuting {
                key: calls.key,
                stop,
            },
            now(),
        )
        .unwrap();
        let step = detail(&db).steps.remove(0);
        assert_eq!(step.end, StepEnd::Stop { cause: stop.into() });
        let Node::Step { slots, .. } = node_at(&db, 2) else {
            unreachable!()
        };
        assert_eq!(first_stopping_cause(&slots).unwrap(), None);
        for slot in &slots[1..] {
            assert_eq!(slot.progress, SlotProgress::Registered);
            assert_eq!(resolve_slot(slot).payload, ResultPayload::NotExecuted);
        }
    }
    for (reply, expected_outcome) in [
        (ToolReply::Cancelled, PublicTurnOutcome::Cancelled),
        (ToolReply::Indeterminate, PublicTurnOutcome::Failed),
    ] {
        let (_dir, _layout, db) = store();
        let (lock, calls) = running(&db, sid(1), tid(1), 2, 1);
        finish(&db, &lock, &calls.bindings[0], reply);
        assert!(
            db.start_invocation(&lock, &calls.bindings[1], now())
                .is_err()
        );
        assert_eq!(detail(&db).summary.outcome, expected_outcome);
    }
}
